//! The browser-extension bridge — a loopback HTTP server.
//!
//! Nova is the ONLY writer to `library.db`. An extension cannot open SQLite, and
//! must not: writing rows behind the app's back would leave `search_index`
//! unsynced, skip thumbnail generation (`thumb_hash IS NULL` is the pending
//! marker, and nothing would ever pick those rows up), bypass folder auto-tags,
//! and leave a running instance's manifest stale. So the extension asks, and
//! this module answers by calling the same functions the Tauri commands call.
//!
//! ## Why a loopback port is safe
//!
//! A listening port is reachable by any page in any browser, so this leans on
//! TWO independent barriers, neither sufficient alone:
//!
//! 1. **The browser refuses to send the request.** Every real endpoint requires
//!    the `X-Nova-Token` header, which makes it a non-simple cross-origin
//!    request and forces a CORS preflight. We answer that preflight only for a
//!    paired extension origin, so a web page's request is never dispatched.
//!    This is load-bearing rather than decorative: a *simple* cross-origin POST
//!    IS delivered and executed, with only its response withheld — the classic
//!    CSRF shape, where the side effect is the whole attack.
//! 2. **A 256-bit pairing token**, issued only after a human approves the
//!    request inside Nova. This covers everything that is not a browser: curl,
//!    a local script, another application.
//!
//! Then three smaller ones. The `Host` header must be the loopback literal,
//! which defeats DNS rebinding — a rebound request arrives carrying the
//! attacker's hostname and dies there. The socket binds 127.0.0.1, never
//! 0.0.0.0. And the surface is capture-shaped only: no "list assets", no file
//! read, no query endpoint, so the worst case of a total auth bypass is an
//! unwanted image arriving *in* the library rather than anything leaving it.
//!
//! ## Why pairing records the origin instead of hard-coding it
//!
//! Chromium extensions can pin their id with a manifest `key`, giving a stable
//! `chrome-extension://…` origin to allowlist. Firefox does not: `moz-extension`
//! origins are randomised **per installation**, so there is nothing to write
//! down in advance. Learning the origin at pair time and pinning it from then on
//! restores the same guarantee, a moment later.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tokio::sync::{oneshot, Mutex, RwLock};
use tracing::{debug, info, warn};

pub mod host;
pub mod lease;
pub mod routes;

pub use lease::Lease;

/// Ports tried in order. A fixed shortlist rather than an ephemeral port,
/// because the extension has to find us and cannot read a file to be told.
pub const PORTS: &[u16] = &[41595, 41596, 41597, 41598];

/// How long a pairing request parks waiting for a human — long enough to notice
/// the dialog, short enough that a forgotten one does not pin a connection.
pub const PAIR_TIMEOUT_SECS: u64 = 120;

/// Upload tickets are single-use and short-lived. They exist so the BYTES can be
/// sent by a content script — the only context holding the page's cookies —
/// without ever handing that page-adjacent context the long-lived token.
const TICKET_TTL_SECS: i64 = 300;

/// Cap on one uploaded capture. Past this the honest answer is "download it in
/// the browser and drag it in"; pushing a multi-gigabyte buffer through an
/// extension's messaging layer is miserable whatever we accept here.
pub const MAX_UPLOAD_BYTES: usize = 200 * 1024 * 1024;

// ── Persisted state ──────────────────────────────────────────────────────────

/// One paired browser. Per-client tokens rather than a single shared secret, so
/// revoking Zen does not also lock out Brave.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PairedClient {
    pub id: String,
    /// What the extension called itself — "Zen", "Brave".
    pub name: String,
    /// The exact origin seen at pair time, pinned from then on.
    pub origin: String,
    pub token: String,
    pub paired_at: String,
}

/// What the UI is allowed to see. The token never crosses back into the webview.
#[derive(Serialize, Clone, Debug)]
pub struct ClientSummary {
    pub id: String,
    pub name: String,
    pub origin: String,
    pub paired_at: String,
}

impl From<&PairedClient> for ClientSummary {
    fn from(c: &PairedClient) -> Self {
        Self {
            id: c.id.clone(),
            name: c.name.clone(),
            origin: c.origin.clone(),
            paired_at: c.paired_at.clone(),
        }
    }
}

/// Default idle window. Generous because idling costs ~30MB and no window means
/// no WebView2 at all — being aggressive only buys another launch on the next
/// capture in a burst.
pub const DEFAULT_IDLE_EXIT_SECS: u64 = 15 * 60;

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
struct BridgeConfig {
    clients: Vec<PairedClient>,
    /// None = use the default. Stored HERE rather than in the frontend's
    /// settings.json because the bridge is what obeys it, and in tray mode there
    /// is no frontend to read that file.
    #[serde(default)]
    idle_exit_secs: Option<u64>,
}

/// A pairing request parked on a human. The HTTP handler waits on `respond`
/// until the UI approves, denies, or the timeout wins.
struct PendingPair {
    id: String,
    name: String,
    origin: String,
    respond: oneshot::Sender<Option<String>>,
}

/// What the UI needs to render a request. It shows the ORIGIN deliberately —
/// that string is the only thing distinguishing the real extension from an
/// impostor asking at the same moment.
#[derive(Serialize, Clone, Debug)]
pub struct PendingSummary {
    pub id: String,
    pub name: String,
    pub origin: String,
}

/// A parked capture, waiting for its bytes.
///
/// It carries the FILING intent — folder and tags — because those were chosen
/// on the first request and the upload that follows is a separate HTTP call with
/// no memory of it. Without this, choosing a folder and then hitting the
/// auth-gated path silently dropped both, which is the sort of bug nobody
/// reports because it only happens on the sites where the fallback triggers.
struct Ticket {
    capture_id: String,
    client_id: String,
    expires_at: i64,
    folder_id: Option<String>,
    tags: Vec<String>,
}

/// What a redeemed ticket hands back to the upload handler.
pub(super) struct TicketClaim {
    /// Which paired browser was issued this ticket. Cannot be *verified* at
    /// redemption — the upload carries a ticket, not a token, deliberately — so
    /// it is attribution for the log rather than a second authentication.
    pub client_id: String,
    pub folder_id: Option<String>,
    pub tags: Vec<String>,
}

/// The result of a capture, kept briefly so a retry is recognised rather than
/// imported twice. See `routes::capture`.
#[derive(Serialize, Clone, Debug)]
pub struct CaptureOutcome {
    pub status: String,
    pub asset_id: Option<String>,
    pub filename: Option<String>,
    pub duplicate: bool,
}

// ── Server state ─────────────────────────────────────────────────────────────

pub struct BridgeState<R: Runtime> {
    pub app: AppHandle<R>,
    config_path: PathBuf,
    config: RwLock<BridgeConfig>,
    pending: Mutex<HashMap<String, PendingPair>>,
    tickets: Mutex<HashMap<String, Ticket>>,
    /// Captures already accepted, by the client-supplied id.
    seen: Mutex<HashMap<String, CaptureOutcome>>,
    pub lease: Lease,
    port: RwLock<Option<u16>>,
}

pub type SharedBridge<R> = Arc<BridgeState<R>>;

impl<R: Runtime> BridgeState<R> {
    pub async fn new(app: AppHandle<R>, lease: Lease) -> Result<Arc<Self>> {
        let dir = app
            .path()
            .app_config_dir()
            .context("No app config dir available")?;
        tokio::fs::create_dir_all(&dir)
            .await
            .context("Could not create the app config dir")?;
        let config_path = dir.join("bridge.json");

        let config = match tokio::fs::read(&config_path).await {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                warn!(error = %e, "bridge.json unreadable; starting with no paired clients");
                BridgeConfig::default()
            }),
            Err(_) => BridgeConfig::default(),
        };

        Ok(Arc::new(Self {
            app,
            config_path,
            config: RwLock::new(config),
            pending: Mutex::new(HashMap::new()),
            tickets: Mutex::new(HashMap::new()),
            seen: Mutex::new(HashMap::new()),
            lease,
            port: RwLock::new(None),
        }))
    }

    async fn persist(&self) -> Result<()> {
        let snapshot = self.config.read().await.clone();
        let json = serde_json::to_vec_pretty(&snapshot)?;
        tokio::fs::write(&self.config_path, json)
            .await
            .context("Could not write bridge.json")?;
        Ok(())
    }

    pub async fn set_port(&self, port: Option<u16>) {
        *self.port.write().await = port;
    }

    /// How long to idle before exiting.
    ///
    /// `NOVA_IDLE_EXIT_SECS` wins when set — a headless override for watching
    /// the behaviour without a window to change the setting in. Otherwise the
    /// stored preference, otherwise the default.
    pub async fn idle_exit_secs(&self) -> u64 {
        if let Some(secs) = std::env::var("NOVA_IDLE_EXIT_SECS")
            .ok()
            .and_then(|raw| raw.parse::<u64>().ok())
            .filter(|secs| *secs > 0)
        {
            return secs;
        }
        self.config
            .read()
            .await
            .idle_exit_secs
            .unwrap_or(DEFAULT_IDLE_EXIT_SECS)
    }

    pub async fn set_idle_exit_secs(&self, secs: u64) -> Result<()> {
        // Clamped rather than validated-and-rejected: the UI offers a slider, and
        // a value outside the range is a bug in the caller, not something worth
        // failing a settings write over.
        let secs = secs.clamp(30, 24 * 60 * 60);
        self.config.write().await.idle_exit_secs = Some(secs);
        self.persist().await?;
        info!(secs, "Idle exit updated");
        Ok(())
    }

    pub async fn port(&self) -> Option<u16> {
        *self.port.read().await
    }

    pub async fn clients(&self) -> Vec<ClientSummary> {
        self.config
            .read()
            .await
            .clients
            .iter()
            .map(ClientSummary::from)
            .collect()
    }

    pub async fn pending_pairings(&self) -> Vec<PendingSummary> {
        self.pending
            .lock()
            .await
            .values()
            .map(|p| PendingSummary {
                id: p.id.clone(),
                name: p.name.clone(),
                origin: p.origin.clone(),
            })
            .collect()
    }

    /// Park a pairing request. Returns its id alongside the channel, because the
    /// caller MUST be able to withdraw it — a request that times out and is left
    /// in the map shows up in the UI forever as something to approve, with
    /// nobody on the other end to receive the token.
    pub(super) async fn open_pairing(
        &self,
        name: String,
        origin: String,
    ) -> (String, oneshot::Receiver<Option<String>>) {
        let (tx, rx) = oneshot::channel();
        let id = uuid::Uuid::new_v4().to_string();
        self.pending.lock().await.insert(
            id.clone(),
            PendingPair {
                id: id.clone(),
                name,
                origin,
                respond: tx,
            },
        );
        self.notify_ui().await;
        (id, rx)
    }

    /// Withdraw a parked request. Idempotent — approve/deny have usually already
    /// removed it by the time the handler unwinds.
    pub(super) async fn close_pairing(&self, id: &str) {
        if self.pending.lock().await.remove(id).is_some() {
            self.notify_ui().await;
        }
    }

    /// Approve a waiting request: mint a token, pin the origin, persist, and
    /// unblock the parked handler.
    pub async fn approve(&self, request_id: &str) -> Result<()> {
        let pending = self.pending.lock().await.remove(request_id);
        let Some(pending) = pending else {
            anyhow::bail!("That pairing request is no longer waiting");
        };

        let token = random_token();
        let client = PairedClient {
            id: uuid::Uuid::new_v4().to_string(),
            name: pending.name.clone(),
            origin: pending.origin.clone(),
            token: token.clone(),
            paired_at: Utc::now().to_rfc3339(),
        };

        {
            let mut config = self.config.write().await;
            // Re-pairing an origin REPLACES its entry. Stacking duplicates would
            // leave old tokens authenticating forever with no way to see them.
            config.clients.retain(|c| c.origin != pending.origin);
            config.clients.push(client);
        }
        self.persist().await?;

        info!(origin = %pending.origin, "Paired a browser extension");
        let _ = pending.respond.send(Some(token));
        self.notify_ui().await;
        Ok(())
    }

    pub async fn deny(&self, request_id: &str) -> Result<()> {
        if let Some(pending) = self.pending.lock().await.remove(request_id) {
            let _ = pending.respond.send(None);
        }
        self.notify_ui().await;
        Ok(())
    }

    pub async fn revoke(&self, client_id: &str) -> Result<()> {
        {
            let mut config = self.config.write().await;
            config.clients.retain(|c| c.id != client_id);
        }
        self.persist().await?;
        self.notify_ui().await;
        Ok(())
    }

    async fn notify_ui(&self) {
        if let Err(e) = self.app.emit("bridge-changed", ()) {
            debug!(error = %e, "No listener for bridge-changed");
        }
    }

    /// Resolve a token to its client, in constant time with respect to the
    /// token's VALUE. A short-circuiting compare leaks the shared prefix length,
    /// which over enough attempts recovers the whole secret.
    async fn client_for_token(&self, token: &str) -> Option<PairedClient> {
        let config = self.config.read().await;
        let mut found: Option<PairedClient> = None;
        for client in &config.clients {
            if constant_time_eq(client.token.as_bytes(), token.as_bytes()) {
                found = Some(client.clone());
            }
        }
        found
    }

    pub(super) async fn issue_ticket(
        &self,
        capture_id: &str,
        client_id: &str,
        folder_id: Option<String>,
        tags: Vec<String>,
    ) -> String {
        let ticket = random_token();
        let mut tickets = self.tickets.lock().await;
        // Opportunistic sweep — tickets are few and short-lived, so this never
        // needs a timer of its own.
        let now = Utc::now().timestamp();
        tickets.retain(|_, t| t.expires_at > now);
        tickets.insert(
            ticket.clone(),
            Ticket {
                capture_id: capture_id.to_string(),
                client_id: client_id.to_string(),
                expires_at: now + TICKET_TTL_SECS,
                folder_id,
                tags,
            },
        );
        ticket
    }

    /// Redeem a ticket. Single-use: taken out of the map whether or not the
    /// upload that follows succeeds, so a leaked ticket cannot be replayed.
    pub(super) async fn redeem_ticket(
        &self,
        ticket: &str,
        capture_id: &str,
    ) -> Option<TicketClaim> {
        let mut tickets = self.tickets.lock().await;
        let entry = tickets.remove(ticket)?;
        if entry.expires_at <= Utc::now().timestamp() || entry.capture_id != capture_id {
            return None;
        }
        Some(TicketClaim {
            client_id: entry.client_id,
            folder_id: entry.folder_id,
            tags: entry.tags,
        })
    }

    async fn remember(&self, capture_id: &str, outcome: CaptureOutcome) {
        let mut seen = self.seen.lock().await;
        // Bounded: this is a retry guard, not a history. Oldest entries are
        // irrelevant once the extension has moved on.
        if seen.len() > 256 {
            seen.clear();
        }
        seen.insert(capture_id.to_string(), outcome);
    }

    async fn recall(&self, capture_id: &str) -> Option<CaptureOutcome> {
        self.seen.lock().await.get(capture_id).cloned()
    }
}

/// Byte-for-byte compare that always inspects the whole of both inputs.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// 256 bits from the OS CSPRNG, hex-encoded.
fn random_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the OS must provide randomness");
    let mut out = String::with_capacity(64);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

// ── Serving ──────────────────────────────────────────────────────────────────

/// Bind the first free port from `PORTS`.
async fn bind() -> Result<(tokio::net::TcpListener, u16)> {
    for port in PORTS {
        // LOOPBACK, never 0.0.0.0 — written as the constant rather than a
        // configurable string precisely so it cannot be widened by accident.
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, *port));
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => return Ok((listener, *port)),
            Err(e) => debug!(port, error = %e, "Bridge port unavailable; trying the next"),
        }
    }
    anyhow::bail!("No bridge port available in {PORTS:?}")
}

/// Serve until idle, then drain in two phases and report whether to exit.
///
/// Returns `true` when the process should exit. The caller loops on `false`,
/// which is the abort-the-shutdown path: something took a lease in the gap
/// between the countdown finishing and the listener closing, so we re-bind and
/// carry on rather than dying with work outstanding.
pub async fn serve_once<R: Runtime>(state: SharedBridge<R>) -> Result<bool> {
    let (listener, port) = bind().await?;
    state.set_port(Some(port)).await;
    info!(port, "Extension bridge listening on 127.0.0.1");

    let router = routes::router(Arc::clone(&state));

    // Phase one: stop accepting. `axum` closes the listener as soon as this
    // resolves, so anything arriving afterwards is REFUSED — which the launcher
    // reads as "not running" and recovers from by starting a fresh instance.
    // The timeout is read on every wait iteration, so changing it in Settings
    // takes effect as soon as the window closes and the countdown restarts.
    let drain = {
        let lease = state.lease.clone();
        let state = Arc::clone(&state);
        async move {
            let secs = state.idle_exit_secs().await;
            lease
                .idle_for(move || std::time::Duration::from_secs(secs))
                .await;
        }
    };

    axum::serve(listener, router)
        .with_graceful_shutdown(drain)
        .await
        .context("Bridge server failed")?;

    state.set_port(None).await;

    // Phase two: re-check with the door already shut.
    if state.lease.is_idle() {
        info!("Bridge idle and drained; exiting");
        Ok(true)
    } else {
        info!("Work arrived while draining; staying up");
        Ok(false)
    }
}
