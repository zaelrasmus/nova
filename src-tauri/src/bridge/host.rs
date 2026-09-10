//! Registering `nova-connector` as a native messaging host.
//!
//! Nova does this ITSELF, on every start, rather than leaving it to the
//! installer or a setup script. Three reasons, in order of how much they matter:
//!
//! 1. **It is the only way to be right about the path.** The manifest has to name
//!    an absolute path to the connector, and Nova is the only thing that knows
//!    where it actually ended up — installed, portable, moved, or run out of a
//!    build directory. An installer-time registration is wrong the moment the
//!    folder moves.
//! 2. **It needs no privileges and no extra machinery.** Everything is under
//!    HKCU, so no administrator prompt, no NSIS hook writing JSON, and it works
//!    identically for the MSI, the NSIS package and an unpacked build.
//! 3. **It is self-healing.** A tester who moves the install, or whose registry
//!    is cleaned by some other tool, gets it back on next launch instead of a
//!    browser extension that mysteriously cannot start Nova.
//!
//! Idempotent: if the key and the manifest already say the right thing, nothing
//! is written. So the common case costs one registry read per browser.
//!
//! Windows only. The other platforms use per-user JSON files in known locations
//! rather than a registry, and Nova does not ship there yet.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::json;
use tracing::{debug, info, warn};

/// Must match the name the extensions ask for.
const HOST_NAME: &str = "com.nova.connector";

/// The Firefox-family id, pinned in `extension/manifest.json`.
const GECKO_ID: &str = "nova@local";

/// The Chromium id, derived from the public `key` in `manifest.chromium.json`.
/// Stable by construction — that key is what pins it.
const CHROMIUM_ORIGIN: &str = "chrome-extension://helhjhnfgjfdkbbmojccpckkffafkamm/";

/// Where each browser family looks. Gecko browsers share one path — Zen reports
/// `Vendor=Mozilla` and reads Firefox's. Chromium vendors emphatically do not
/// share, so each needs its own key; writing one for a browser that is not
/// installed is harmless and means it simply works the day it is.
const GECKO_KEYS: &[&str] = &[r"Software\Mozilla\NativeMessagingHosts"];
const CHROMIUM_KEYS: &[&str] = &[
    r"Software\BraveSoftware\Brave-Browser\NativeMessagingHosts",
    r"Software\Helium\Helium\NativeMessagingHosts",
    r"Software\Google\Chrome\NativeMessagingHosts",
    r"Software\Chromium\NativeMessagingHosts",
];

/// Register the connector with every browser family, best-effort.
///
/// Never fails the caller: this is a convenience that makes the extension able
/// to WAKE Nova. Without it the extension still pairs, still captures, and still
/// queues — it just cannot start Nova when Nova is closed. That is not worth
/// refusing to launch over.
pub fn ensure_registered(config_dir: &Path) {
    if !cfg!(windows) {
        return;
    }

    let connector = match connector_path() {
        Ok(path) => path,
        Err(e) => {
            debug!(error = %e, "No connector beside the executable; skipping host registration");
            return;
        }
    };

    if let Err(e) = register(config_dir, &connector) {
        warn!(error = %e, "Could not register the native messaging host (non-fatal)");
    }
}

/// The connector lives beside Nova — as a Tauri sidecar in an installed build,
/// and as a sibling target in a cargo build. Both are "next to me".
fn connector_path() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("Cannot locate the running executable")?;
    let dir = exe.parent().context("Executable has no parent directory")?;
    let connector = dir.join(if cfg!(windows) {
        "nova-connector.exe"
    } else {
        "nova-connector"
    });
    if !connector.exists() {
        anyhow::bail!("no connector at {}", connector.display());
    }
    Ok(connector)
}

fn register(config_dir: &Path, connector: &Path) -> Result<()> {
    std::fs::create_dir_all(config_dir).context("Could not create the config dir")?;

    // Two manifests, because the two families identify the caller differently:
    // Firefox matches on `allowed_extensions` (a gecko id), Chromium on
    // `allowed_origins` (a chrome-extension:// URL). One file cannot satisfy both.
    let gecko = config_dir.join(format!("{HOST_NAME}.json"));
    let chromium = config_dir.join(format!("{HOST_NAME}.chromium.json"));

    write_manifest(
        &gecko,
        json!({
            "name": HOST_NAME,
            "description": "Starts Nova so the browser extension can save into it",
            "path": connector,
            "type": "stdio",
            "allowed_extensions": [GECKO_ID],
        }),
    )?;
    write_manifest(
        &chromium,
        json!({
            "name": HOST_NAME,
            "description": "Starts Nova so the browser extension can save into it",
            "path": connector,
            "type": "stdio",
            "allowed_origins": [CHROMIUM_ORIGIN],
        }),
    )?;

    #[cfg(windows)]
    {
        let mut written = 0;
        for (keys, manifest) in [(GECKO_KEYS, &gecko), (CHROMIUM_KEYS, &chromium)] {
            for key in keys {
                match write_key(key, manifest) {
                    Ok(true) => written += 1,
                    Ok(false) => {}
                    Err(e) => debug!(key, error = %e, "Could not write a host key"),
                }
            }
        }
        if written > 0 {
            info!(written, "Registered the native messaging host");
        }
    }

    Ok(())
}

/// Written WITHOUT a BOM, deliberately. A byte-order mark makes the manifest
/// fail to parse, and the browser then reports the host as simply not existing —
/// which sends you hunting through the registry rather than the file it names.
fn write_manifest(path: &Path, value: serde_json::Value) -> Result<()> {
    let json = serde_json::to_vec_pretty(&value)?;
    // Skip the write when it would change nothing, so a launch does not touch
    // the disk for no reason.
    if std::fs::read(path).is_ok_and(|existing| existing == json) {
        return Ok(());
    }
    std::fs::write(path, json).with_context(|| format!("Could not write {}", path.display()))?;
    Ok(())
}

/// Point one browser's host key at our manifest. Returns whether it changed.
#[cfg(windows)]
fn write_key(subkey: &str, manifest: &Path) -> Result<bool> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let path = format!(r"{subkey}\{HOST_NAME}");
    let wanted = manifest.to_string_lossy().to_string();

    if let Ok(existing) = hkcu.open_subkey_with_flags(&path, KEY_READ) {
        if existing.get_value::<String, _>("").is_ok_and(|v| v == wanted) {
            return Ok(false);
        }
    }

    let (key, _) = hkcu
        .create_subkey_with_flags(&path, KEY_WRITE)
        .with_context(|| format!("Could not create {path}"))?;
    key.set_value("", &wanted)
        .with_context(|| format!("Could not set the default value of {path}"))?;
    Ok(true)
}

#[cfg(not(windows))]
fn write_key(_subkey: &str, _manifest: &Path) -> Result<bool> {
    Ok(false)
}
