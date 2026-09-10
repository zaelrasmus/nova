//! `nova-connector` — the native messaging host that wakes Nova.
//!
//! This is the ONE thing native messaging is used for. It carries no captures:
//! the protocol is length-prefixed JSON over stdio with message-size limits, so
//! pushing a 300MB video through it would be absurd when a loopback socket is
//! right there. The split is deliberate — **wake over native messaging, transfer
//! over HTTP**.
//!
//! It exists because an extension cannot start a process. A browser *can*, but
//! only a binary registered ahead of time in its native-messaging manifest, and
//! only one it spawns as its own child. So the browser runs this, it starts Nova
//! if Nova isn't already up, and it exits. Lifetime: a few hundred milliseconds.
//!
//! The alternative was a `nova://` protocol handler, which needs no registration
//! but shows a confirmation dialog whose "always allow" preference is reset by
//! profile changes and browser updates. This shows nothing, ever — the trust is
//! established once at install time by the registry key, not by the user
//! clicking through a prompt.
//!
//! ## Wire format
//!
//! Both directions: a 4-byte NATIVE-endian length, then that many bytes of UTF-8
//! JSON. Native rather than little-endian is what the spec says; on the only
//! platforms this ships to they are the same thing, but the cast below is
//! written to be correct rather than lucky.
//!
//! ## Why it probes before spawning
//!
//! Checking "is the port bound" is not enough. A Nova that has just decided to
//! exit closes its listener FIRST and then tears down (see `bridge::serve_once`)
//! — so a bound port can belong to a process that is already going away. This
//! makes a real `/health` request and requires a real answer, which means the
//! only two outcomes are "definitely alive" and "definitely start a new one".

use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::process::Command;
use std::time::{Duration, Instant};

/// Must match `bridge::PORTS`.
const PORTS: &[u16] = &[41595, 41596, 41597, 41598];

/// A local connection either answers at once or is not there.
const PROBE_TIMEOUT: Duration = Duration::from_millis(400);

/// How long to wait for a freshly-spawned Nova to start listening. Windowless
/// start is a few hundred milliseconds — this is generous enough to cover a cold
/// disk without leaving the extension hanging if something is truly wrong.
const WAKE_TIMEOUT: Duration = Duration::from_secs(12);

/// Firefox refuses anything larger, and nothing this speaks comes close to it.
const MAX_MESSAGE: usize = 64 * 1024;

fn main() {
    // A single request per invocation: the browser spawns us, asks, and we go.
    // Looping would keep a process alive for no reason.
    let Some(request) = read_message() else {
        return;
    };

    let response = match request.trim() {
        // Anything else is answered rather than ignored, so a version mismatch
        // shows up as a message instead of a hang.
        r if r.contains("\"wake\"") => wake(),
        _ => Reply::err("Unknown request"),
    };

    write_message(&response.to_json());
}

struct Reply {
    ok: bool,
    port: Option<u16>,
    error: Option<String>,
}

impl Reply {
    fn ok(port: u16) -> Self {
        Self {
            ok: true,
            port: Some(port),
            error: None,
        }
    }
    fn err(message: &str) -> Self {
        Self {
            ok: false,
            port: None,
            error: Some(message.to_string()),
        }
    }
    /// Hand-rolled rather than pulling serde into a binary this small. The shape
    /// is fixed and the only variable text is our own error strings.
    fn to_json(&self) -> String {
        let error = match &self.error {
            Some(e) => format!("\"{}\"", e.replace('\\', "\\\\").replace('"', "\\\"")),
            None => "null".to_string(),
        };
        let port = match self.port {
            Some(p) => p.to_string(),
            None => "null".to_string(),
        };
        format!(
            "{{\"ok\":{},\"port\":{},\"error\":{}}}",
            self.ok, port, error
        )
    }
}

/// Find a live Nova, or start one and wait for it.
fn wake() -> Reply {
    if let Some(port) = find_live_bridge() {
        return Reply::ok(port);
    }

    if let Err(e) = spawn_nova() {
        return Reply::err(&format!("Could not start Nova: {e}"));
    }

    let deadline = Instant::now() + WAKE_TIMEOUT;
    while Instant::now() < deadline {
        if let Some(port) = find_live_bridge() {
            return Reply::ok(port);
        }
        std::thread::sleep(Duration::from_millis(120));
    }
    Reply::err("Nova was started but did not begin listening")
}

fn find_live_bridge() -> Option<u16> {
    PORTS.iter().copied().find(|port| is_healthy(*port))
}

/// A real request, not a connect test. See the note at the top about why a bound
/// port is not proof of a living process.
fn is_healthy(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, PROBE_TIMEOUT) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(PROBE_TIMEOUT));
    let _ = stream.set_write_timeout(Some(PROBE_TIMEOUT));

    // `Host` must be the loopback literal — the bridge rejects anything else as
    // a DNS-rebinding signature, and that check applies to us too.
    let request = format!(
        "GET /health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }

    let mut buffer = [0u8; 256];
    let read = stream.read(&mut buffer).unwrap_or(0);
    let _ = stream.shutdown(Shutdown::Both);
    read > 0 && buffer[..read].starts_with(b"HTTP/1.1 200")
}

/// Start Nova windowless.
///
/// Located relative to this executable rather than by a configured path: the
/// connector is installed beside `nova.exe`, so "next to me" is both correct and
/// impossible to get out of sync with an install that moved.
fn spawn_nova() -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    let dir = exe.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "connector has no parent dir")
    })?;
    let nova = dir.join(if cfg!(windows) { "nova.exe" } else { "nova" });

    if !nova.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("nova not found beside the connector at {}", nova.display()),
        ));
    }

    #[cfg(windows)]
    stop_inheriting_std_handles();

    let mut command = Command::new(nova);
    command
        .arg("--tray")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        /// No console, and — the part that matters — no inheriting OUR handles.
        /// Without this Nova inherits the browser's end of the native-messaging
        /// pipe and holds it for its entire lifetime, so the browser never sees
        /// the connector's output close. Caught by the first end-to-end test:
        /// Nova started correctly and the caller hung forever waiting for EOF.
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        /// Browsers put child processes in a Job object that terminates
        /// everything in it when the browser exits. Nova must outlive the
        /// browser — being killed because you closed a tab would be absurd —
        /// so break out of it. Not all jobs permit this, hence the fallback.
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

        command.creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB);
        if command.spawn().is_ok() {
            return Ok(());
        }

        // The job refused to let us out. Still better to start Nova and share
        // the browser's fate than not to start it at all.
        command.creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW);
    }

    command.spawn()?;
    Ok(())
}

/// Mark our own std handles non-inheritable before spawning Nova.
///
/// `DETACHED_PROCESS` only governs the CONSOLE; it does nothing about handle
/// inheritance, and Rust's `Command` always spawns with `bInheritHandles=TRUE`
/// with no way to turn it off. So without this, Nova inherits the browser's end
/// of the native-messaging pipe and holds it open for its entire lifetime —
/// which means the browser never observes the connector's output closing.
///
/// Found the hard way: the first end-to-end test started Nova perfectly and then
/// hung forever waiting for EOF that a long-lived grandchild was holding.
///
/// Declared inline rather than adding a Windows binding crate — two symbols from
/// kernel32 is not worth a dependency in a binary this small.
#[cfg(windows)]
fn stop_inheriting_std_handles() {
    const STD_INPUT_HANDLE: u32 = -10i32 as u32;
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;
    const INVALID_HANDLE_VALUE: isize = -1;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetStdHandle(which: u32) -> isize;
        fn SetHandleInformation(handle: isize, mask: u32, flags: u32) -> i32;
    }

    for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: both calls take a handle we just obtained from the OS and
        // neither dereferences memory. A failure is not actionable — the worst
        // case is the handle leak this exists to avoid.
        unsafe {
            let handle = GetStdHandle(which);
            if handle != 0 && handle != INVALID_HANDLE_VALUE {
                SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

// ── Native messaging framing ─────────────────────────────────────────────────

fn read_message() -> Option<String> {
    let mut stdin = std::io::stdin().lock();
    let mut length = [0u8; 4];
    stdin.read_exact(&mut length).ok()?;

    let length = u32::from_ne_bytes(length) as usize;
    if length == 0 || length > MAX_MESSAGE {
        return None;
    }

    let mut payload = vec![0u8; length];
    stdin.read_exact(&mut payload).ok()?;
    String::from_utf8(payload).ok()
}

fn write_message(json: &str) {
    let bytes = json.as_bytes();
    let mut stdout = std::io::stdout().lock();
    // A partial write here means the browser has gone; there is nothing useful
    // to do about it and no channel left to report it on.
    if stdout
        .write_all(&(bytes.len() as u32).to_ne_bytes())
        .is_err()
    {
        return;
    }
    let _ = stdout.write_all(bytes);
    let _ = stdout.flush();
}
