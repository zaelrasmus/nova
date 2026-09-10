//! Activity leases, and the idle exit they gate.
//!
//! Nova can be woken by the browser extension, do a capture, and put itself away
//! again — so on a memory-tight machine it costs nothing while you are not
//! saving. The whole difficulty is deciding *when* it is safe to go.
//!
//! A countdown you reset per request is the obvious design and it is wrong. It
//! races in at least five places: a request arriving as the timer fires; one
//! arriving mid-teardown; the launcher seeing a still-bound port on a dying
//! process; a thumbnail pass or import still running; and the user opening the
//! window at the last second.
//!
//! So this is a REFCOUNT, not a timer. Everything that must not be interrupted
//! takes a lease — an in-flight request, a running import, an open window — and
//! the countdown only exists while the count is zero. A late request does not
//! *reset* the timer, it *destroys* it, because taking a lease cancels the wait
//! outright.
//!
//! The remaining sliver (timer fires, request arrives a microsecond later) is
//! closed by the caller draining in two phases: close the listener FIRST, then
//! re-check. Once the listener is closed a new connection is refused, and a
//! refused connection is recoverable — the launcher reads it as "not running"
//! and starts a fresh instance. A half-served connection into a process tearing
//! down its database pool is not recoverable. Always prefer the failure that
//! looks like "closed" over the one that looks like "open but doomed".

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;
use tracing::debug;

#[derive(Default)]
struct Inner {
    /// In-flight work. Zero means "nothing would be interrupted by exiting".
    count: AtomicUsize,
    /// A window exists. Held as a flag rather than a lease because it is set and
    /// cleared by Tauri events, not by a scope.
    windowed: AtomicBool,
    /// Woken whenever the count reaches zero, so the idle watcher can start
    /// counting without polling.
    idle: Notify,
    /// Woken whenever a lease is TAKEN, so a pending wait can abandon itself.
    busy: Notify,
}

/// Hand out with `.guard()`; the count falls when the guard drops.
#[derive(Clone, Default)]
pub struct Lease {
    inner: Arc<Inner>,
}

/// Holds the count above zero for as long as it lives.
pub struct LeaseGuard {
    inner: Arc<Inner>,
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        let previous = self.inner.count.fetch_sub(1, Ordering::AcqRel);
        if previous == 1 {
            // Last one out: let the idle watcher start counting.
            self.inner.idle.notify_waiters();
        }
    }
}

impl Lease {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take a lease. Cancels any wait already in progress.
    pub fn guard(&self) -> LeaseGuard {
        self.inner.count.fetch_add(1, Ordering::AcqRel);
        self.inner.busy.notify_waiters();
        LeaseGuard {
            inner: Arc::clone(&self.inner),
        }
    }

    pub fn active(&self) -> usize {
        self.inner.count.load(Ordering::Acquire)
    }

    /// True when nothing is in flight AND no window is open.
    ///
    /// The window check is separate on purpose: if you opened Nova yourself, it
    /// must never decide to quit underneath you, however long you leave it idle.
    pub fn is_idle(&self) -> bool {
        self.active() == 0 && !self.inner.windowed.load(Ordering::Acquire)
    }

    pub fn set_windowed(&self, windowed: bool) {
        self.inner.windowed.store(windowed, Ordering::Release);
        if windowed {
            self.inner.busy.notify_waiters();
        } else {
            self.inner.idle.notify_waiters();
        }
    }

    pub fn windowed(&self) -> bool {
        self.inner.windowed.load(Ordering::Acquire)
    }

    /// Resolve once the process has been idle continuously for `timeout`.
    ///
    /// "Continuously" is the point. Any lease taken, or any window opened,
    /// during the wait abandons it and starts the whole thing over — so a
    /// capture at 4:59 of a five-minute window does not squeak through, it
    /// cancels the countdown entirely.
    pub async fn idle_for(&self, timeout: impl Fn() -> Duration) {
        loop {
            // Subscribe BEFORE testing, so a lease taken between the test and
            // the wait cannot be missed.
            let busy = self.inner.busy.notified();
            let idle = self.inner.idle.notified();

            if !self.is_idle() {
                // Wait for the count to fall, then re-test from the top.
                idle.await;
                continue;
            }

            tokio::select! {
                // Read on every iteration, not once: the timeout is a user
                // setting, and any activity restarts this loop — so a change made
                // in the window takes effect the moment the window closes.
                _ = tokio::time::sleep(timeout()) => {
                    // Re-test rather than trusting the elapsed sleep: `busy`
                    // only fires for waiters that were subscribed, and being
                    // certain here is cheap.
                    if self.is_idle() {
                        debug!("Idle for the full window; ready to drain");
                        return;
                    }
                }
                _ = busy => {
                    debug!("Activity during the idle wait; countdown abandoned");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_guard_holds_the_count_until_it_drops() {
        let lease = Lease::new();
        assert!(lease.is_idle());

        let guard = lease.guard();
        assert_eq!(lease.active(), 1);
        assert!(!lease.is_idle(), "in-flight work must block the idle exit");

        drop(guard);
        assert_eq!(lease.active(), 0);
        assert!(lease.is_idle());
    }

    #[test]
    fn an_open_window_blocks_the_exit_on_its_own() {
        let lease = Lease::new();
        lease.set_windowed(true);
        assert!(
            !lease.is_idle(),
            "a window you opened yourself must never be closed by a timer"
        );

        lease.set_windowed(false);
        assert!(lease.is_idle(), "closing the window releases it to the background");
    }

    #[tokio::test(start_paused = true)]
    async fn idle_for_resolves_only_after_a_full_quiet_window() {
        let lease = Lease::new();
        let timeout = Duration::from_secs(300);

        let waiter = tokio::spawn({
            let lease = lease.clone();
            async move { lease.idle_for(|| timeout).await }
        });

        // Nothing has happened, but the window has not elapsed either.
        tokio::time::advance(Duration::from_secs(299)).await;
        assert!(!waiter.is_finished(), "must not fire early");

        tokio::time::advance(Duration::from_secs(2)).await;
        waiter.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn a_capture_at_the_last_second_cancels_the_countdown() {
        // The 4:59 case. A reset-on-request timer gets this wrong by letting the
        // already-scheduled fire win the race.
        let lease = Lease::new();
        let timeout = Duration::from_secs(300);

        let waiter = tokio::spawn({
            let lease = lease.clone();
            async move { lease.idle_for(|| timeout).await }
        });

        tokio::time::advance(Duration::from_secs(299)).await;
        let guard = lease.guard();

        // Well past the original deadline: the countdown must be gone, not due.
        tokio::time::advance(Duration::from_secs(60)).await;
        assert!(
            !waiter.is_finished(),
            "work started before the deadline must cancel the exit outright"
        );

        // And when that work finishes, the FULL window has to elapse again.
        drop(guard);
        tokio::time::advance(Duration::from_secs(299)).await;
        assert!(!waiter.is_finished(), "the wait restarts, it does not resume");

        tokio::time::advance(Duration::from_secs(2)).await;
        waiter.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn opening_a_window_mid_countdown_cancels_it_too() {
        let lease = Lease::new();
        let waiter = tokio::spawn({
            let lease = lease.clone();
            async move { lease.idle_for(|| Duration::from_secs(300)).await }
        });

        tokio::time::advance(Duration::from_secs(200)).await;
        lease.set_windowed(true);
        tokio::time::advance(Duration::from_secs(600)).await;

        assert!(
            !waiter.is_finished(),
            "an open window blocks the exit no matter how long it idles"
        );
    }
}
