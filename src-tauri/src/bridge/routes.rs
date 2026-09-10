//! The bridge's HTTP surface, and the auth boundary in front of it.
//!
//! Deliberately capture-shaped. There is no endpoint that reads an asset, lists
//! a library, or runs a query — the only read is the folder list, so the picker
//! has something to show. That asymmetry is the point: if authentication failed
//! completely, the worst outcome is an unwanted image arriving in the library,
//! not anything leaving it.
//!
//! CORS is hand-rolled rather than pulled from `tower-http` because the
//! allowlist is DYNAMIC — it is whatever origin was pinned at pair time — and
//! this is the security boundary, so it should be readable in one place.

use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Path, Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{Emitter, Runtime};
use tokio::io::AsyncWriteExt;
use tracing::{info, instrument, warn};

use super::{CaptureOutcome, PairedClient, SharedBridge, MAX_UPLOAD_BYTES, PAIR_TIMEOUT_SECS};
use crate::assets;
use crate::db::DbState;

/// Header carrying the long-lived pairing token.
const TOKEN_HEADER: &str = "x-nova-token";
/// Header carrying a single-use upload ticket. Separate from the token so a
/// content script — which shares a tab with hostile page code — never has to
/// hold the long-lived secret.
const TICKET_HEADER: &str = "x-nova-ticket";

pub fn router<R: Runtime>(state: SharedBridge<R>) -> Router {
    Router::new()
        // Unauthenticated on purpose: this is what the launcher stub probes to
        // decide whether Nova is up. It reveals only that Nova exists, which
        // anything able to see the open port already knows.
        .route("/health", get(health::<R>))
        .route("/pair", post(pair::<R>))
        .route("/folders", get(folders::<R>))
        .route("/capture", post(capture::<R>))
        .route("/capture/{id}/upload", put(upload::<R>))
        .layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            guard::<R>,
        ))
        .with_state(state)
}

// ── The auth boundary ────────────────────────────────────────────────────────

/// True for the two extension schemes and nothing else.
///
/// Chromium ids are stable (a manifest `key` pins them) but Firefox randomises
/// `moz-extension` per install, so this scheme test is all that can be applied
/// BEFORE pairing. After pairing, the exact origin is compared instead.
fn is_extension_origin(origin: &str) -> bool {
    origin.starts_with("moz-extension://") || origin.starts_with("chrome-extension://")
}

/// The `Host` header must be the loopback LITERAL.
///
/// This is the DNS-rebinding defence. An attacker points their own hostname at
/// 127.0.0.1 so the browser treats their page as same-origin with us and skips
/// CORS entirely — but the request still arrives carrying `Host: evil.com`, and
/// dies here. `localhost` is rejected too: it is a name, and names can be made
/// to resolve wherever an attacker likes.
fn host_is_loopback(headers: &HeaderMap) -> bool {
    let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        // HTTP/2 omits Host in favour of :authority; axum normalises that back,
        // so an absent Host here means a malformed request.
        return false;
    };
    let host = host.split(':').next().unwrap_or("");
    host == "127.0.0.1"
}

/// `/capture/{id}/upload` — matched structurally rather than by prefix so that
/// nothing else under `/capture/` can accidentally inherit the token exemption.
fn is_upload_path(path: &str) -> bool {
    let mut parts = path.split('/');
    parts.next() == Some("")
        && parts.next() == Some("capture")
        && parts.next().is_some_and(|id| !id.is_empty())
        && parts.next() == Some("upload")
        && parts.next().is_none()
}

fn origin_of(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

fn cors_headers(origin: &str) -> [(header::HeaderName, HeaderValue); 4] {
    [
        (
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_str(origin).unwrap_or(HeaderValue::from_static("null")),
        ),
        (
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static("content-type, x-nova-token, x-nova-ticket"),
        ),
        (
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("GET, POST, PUT, OPTIONS"),
        ),
        (
            header::ACCESS_CONTROL_MAX_AGE,
            HeaderValue::from_static("600"),
        ),
    ]
}

/// One gate in front of everything.
///
/// Order matters: cheapest and most absolute checks first, so a hostile request
/// is rejected before it can cost anything.
async fn guard<R: Runtime>(
    State(state): State<SharedBridge<R>>,
    request: Request,
    next: Next,
) -> Response {
    let headers = request.headers().clone();
    let path = request.uri().path().to_string();
    let method = request.method().clone();

    if !host_is_loopback(&headers) {
        warn!(?path, "Rejected a bridge request with a non-loopback Host");
        return StatusCode::FORBIDDEN.into_response();
    }

    let origin = origin_of(&headers);

    // A browser only ever reaches a *cross-origin* endpoint here, so it always
    // sends Origin and cannot be told not to. An ABSENT Origin therefore means
    // the caller is not a browser (curl, a local script), and the token alone is
    // the barrier for those — which is exactly what it is there for.
    if let Some(origin) = &origin {
        if !is_extension_origin(origin) {
            warn!(%origin, ?path, "Rejected a bridge request from a non-extension origin");
            return StatusCode::FORBIDDEN.into_response();
        }
    }

    // Preflight. Answering it is what decides whether the browser will send the
    // real request at all — for a web page it never gets this far, because the
    // origin check above already refused.
    if method == Method::OPTIONS {
        return match &origin {
            Some(origin) => (StatusCode::NO_CONTENT, cors_headers(origin)).into_response(),
            None => StatusCode::NO_CONTENT.into_response(),
        };
    }

    // Three paths are not token-authenticated, each for its own reason:
    //   * `/health` — how the launcher detects us, before any token exists.
    //   * `/pair`   — how a token is obtained.
    //   * the upload — authenticated by a single-use TICKET instead, which is
    //     the entire point: the bytes are sent by whichever context holds the
    //     page's cookies, and that context shares a tab with hostile code. It
    //     must never be handed the long-lived token.
    //
    // The ticket is checked by the handler (`redeem_ticket`), which also binds
    // it to one capture id and consumes it whether or not the upload succeeds.
    let open = path == "/health" || path == "/pair" || is_upload_path(&path);

    let mut request = request;
    if !open {
        let token = headers
            .get(TOKEN_HEADER)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();

        let Some(client) = state.client_for_token(token).await else {
            return StatusCode::UNAUTHORIZED.into_response();
        };

        // A paired client is pinned to the origin it paired from, which stops a
        // second extension reusing a token it somehow obtained.
        //
        // An EMPTY stored origin means there was none to pin at pair time (see
        // `pair`), so the token alone authenticates that client. Comparing
        // against "" would lock out the very browser that just paired.
        if !client.origin.is_empty() {
            if let Some(origin) = &origin {
                if origin != &client.origin {
                    warn!(%origin, expected = %client.origin, "Token used from an unexpected origin");
                    return StatusCode::FORBIDDEN.into_response();
                }
            }
        }
        request.extensions_mut().insert(client);
    }

    let mut response = next.run(request).await;
    if let Some(origin) = &origin {
        for (name, value) in cors_headers(origin) {
            response.headers_mut().insert(name, value);
        }
    }
    response
}

// ── Endpoints ────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct Health {
    ok: bool,
    version: &'static str,
    /// Whether a library is open. The extension shows a different state for
    /// "Nova is running but has no library" — different problem, different fix.
    library: bool,
    paired: bool,
}

async fn health<R: Runtime>(State(state): State<SharedBridge<R>>) -> impl IntoResponse {
    let library = {
        use tauri::Manager;
        state.app.state::<DbState>().acquire().await.is_ok()
    };
    Json(Health {
        ok: true,
        version: env!("CARGO_PKG_VERSION"),
        library,
        paired: !state.clients().await.is_empty(),
    })
}

#[derive(Deserialize)]
struct PairRequest {
    /// What to show the human: "Zen", "Brave".
    name: Option<String>,
}

/// Ask to pair. Parks until a human answers inside Nova.
///
/// Requires no token — this is how one is obtained — so the human IS the
/// authentication. Which means the request must be visible somewhere: if no
/// window is open there is nobody to ask, and this says so rather than hanging
/// for two minutes and timing out.
#[instrument(skip_all)]
async fn pair<R: Runtime>(
    State(state): State<SharedBridge<R>>,
    headers: HeaderMap,
    Json(body): Json<PairRequest>,
) -> Response {
    // Pin the origin WHEN THERE IS ONE.
    //
    // Firefox grants extensions privileged cross-origin fetch for hosts in their
    // `host_permissions`, and such requests may carry no `Origin` at all. Since
    // `moz-extension://` ids are randomised per install there was never a value
    // to allowlist ahead of time anyway — so on Firefox the token carries the
    // weight, exactly as designed, and an empty pin means "token only".
    //
    // This is not a hole: the guard has already refused any origin that is not
    // an extension scheme, so a web page cannot arrive here regardless.
    let origin = origin_of(&headers).unwrap_or_default();

    if !state.lease.windowed() {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "error": "no_window",
                "message": "Open Nova and try pairing again — approving needs its window."
            })),
        )
            .into_response();
    }

    let name = body
        .name
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| "Browser extension".to_string());

    info!(%origin, %name, "Pairing requested");
    let (request_id, waiter) = state.open_pairing(name, origin).await;

    let outcome =
        tokio::time::timeout(std::time::Duration::from_secs(PAIR_TIMEOUT_SECS), waiter).await;

    // Always withdraw. Approve and deny have already removed it; a timeout has
    // not, and leaving it parked would leave the settings panel offering to
    // approve a request whose other end hung up minutes ago.
    state.close_pairing(&request_id).await;

    match outcome {
        Ok(Ok(Some(token))) => Json(json!({ "token": token })).into_response(),
        Ok(Ok(None)) => (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "denied", "message": "Pairing was declined." })),
        )
            .into_response(),
        // Sender dropped, or nobody answered in time.
        _ => (
            StatusCode::REQUEST_TIMEOUT,
            Json(json!({ "error": "timeout", "message": "Nobody approved the request." })),
        )
            .into_response(),
    }
}

async fn folders<R: Runtime>(State(state): State<SharedBridge<R>>) -> Response {
    use tauri::Manager;
    let _lease = state.lease.guard();

    let handle = match state.app.state::<DbState>().acquire().await {
        Ok(handle) => handle,
        Err(_) => return no_library(),
    };
    match assets::fetch_folders(&handle.pool).await {
        Ok(folders) => Json(json!({ "folders": folders })).into_response(),
        Err(e) => {
            warn!(error = %e, "Bridge could not list folders");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

fn no_library() -> Response {
    (
        StatusCode::CONFLICT,
        Json(json!({
            "error": "no_library",
            "message": "Nova is running but no library is open."
        })),
    )
        .into_response()
}

#[derive(Deserialize, Debug)]
struct CaptureRequest {
    /// Client-generated, and the whole basis of retry safety. A capture that
    /// fails at the connection level is indistinguishable from one that
    /// succeeded and lost its reply, so the extension resends with the SAME id
    /// and we answer from memory instead of importing twice.
    id: String,
    /// The media URL.
    url: String,
    /// The page it was found on. Provenance, and the more useful of the two to
    /// a human six months later.
    page_url: Option<String>,
    title: Option<String>,
    folder_id: Option<String>,
    /// Tag NAMES, not ids — the extension has no way to know Nova's ids, and
    /// making it ask for them first would put a round trip in front of every
    /// capture. Resolved (creating as needed) after the asset exists.
    #[serde(default)]
    tags: Vec<String>,
    /// Skip the download and record the link only — the online-asset path.
    #[serde(default)]
    link_only: bool,
    /// Skip straight to the bytes handshake. The extension sets this when it
    /// already knows fetching is hopeless: a `blob:` URL, or a page it knows is
    /// behind a login.
    #[serde(default)]
    bytes: bool,
}

/// Capture from a URL, or hand back a ticket for the bytes.
///
/// The negotiation is the interesting part. Most media is fetchable by Nova
/// directly, which is cheap and reuses the whole existing import path. But
/// anything behind a login, or hotlink-protected, will 403 a request that has no
/// cookies — and the extension DOES have cookies. So a failed fetch is not an
/// error, it is a request to try the other way round.
#[instrument(skip_all, fields(capture = %body.id))]
async fn capture<R: Runtime>(
    State(state): State<SharedBridge<R>>,
    axum::Extension(client): axum::Extension<PairedClient>,
    Json(body): Json<CaptureRequest>,
) -> Response {
    use tauri::Manager;
    let _lease = state.lease.guard();

    if let Some(previous) = state.recall(&body.id).await {
        info!("Replaying a capture the extension already sent");
        return Json(previous).into_response();
    }

    let handle = match state.app.state::<DbState>().acquire().await {
        Ok(handle) => handle,
        Err(_) => return no_library(),
    };

    // Straight to bytes: no point probing a blob: URL that means nothing outside
    // the page that made it.
    if body.bytes {
        let ticket = state
            .issue_ticket(&body.id, &client.id, body.folder_id.clone(), body.tags.clone())
            .await;
        return Json(json!({ "status": "needs_bytes", "ticket": ticket })).into_response();
    }

    let probe = match crate::remote::probe(&body.url).await {
        Ok(probe) => probe,
        Err(e) => {
            // The fetch failed, which is the auth-gated case more often than it
            // is a dead link. Offer the bytes path rather than giving up.
            info!(error = %e, "Direct fetch failed; asking the extension for bytes");
            let ticket = state
            .issue_ticket(&body.id, &client.id, body.folder_id.clone(), body.tags.clone())
            .await;
            return Json(json!({
                "status": "needs_bytes",
                "ticket": ticket,
                "reason": e.to_string(),
            }))
            .into_response();
        }
    };

    if probe.is_html {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "error": "not_media",
                "message": "That link is a web page, not a media file."
            })),
        )
            .into_response();
    }

    let name = title_to_filename(body.title.as_deref(), &probe.filename, &probe.extension);

    let outcome = if body.link_only {
        match assets::create_remote_asset(
            &handle.pool,
            &handle.root,
            assets::RemoteAssetSpec {
                filename: name,
                extension: probe.extension.clone(),
                asset_type: probe.asset_type,
                remote_url: probe.original_url.clone(),
                size: probe.size,
                supports_range: probe.supports_range,
            },
            body.folder_id.as_deref(),
        )
        .await
        {
            Ok(asset) => CaptureOutcome {
                status: "linked".into(),
                filename: Some(asset.filename.clone()),
                asset_id: Some(asset.id),
                duplicate: false,
            },
            Err(e) => return failed(e),
        }
    } else {
        match download_and_import(
            &state,
            &handle,
            &body.url,
            &name,
            body.folder_id.clone(),
            body.page_url.as_deref().unwrap_or(&body.url),
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(e) => return failed(e),
        }
    };

    apply_tags(&handle, &outcome, &body.tags).await;
    state.remember(&body.id, outcome.clone()).await;
    announce(&state, &outcome);
    Json(outcome).into_response()
}

/// Attach the tags the capture dialog collected.
///
/// Non-fatal by design: the asset is already saved, and losing a tag is a much
/// smaller failure than reporting the whole capture as failed because one tag
/// name was awkward. Nothing to do for a duplicate — that row already has
/// whatever the user filed it under the first time.
async fn apply_tags(handle: &crate::db::LibraryHandle, outcome: &CaptureOutcome, tags: &[String]) {
    let Some(asset_id) = outcome.asset_id.clone() else {
        return;
    };
    let wanted: Vec<&String> = tags.iter().filter(|t| !t.trim().is_empty()).collect();
    if wanted.is_empty() {
        return;
    }

    let ids = [asset_id];
    for name in wanted {
        match crate::tags::ensure_tag(&handle.pool, name).await {
            Ok(tag_id) => {
                let assigned = async {
                    let mut conn = handle.pool.acquire().await?;
                    crate::tags::assign_tag_in(&mut conn, &tag_id, &ids).await
                }
                .await;
                if let Err(e) = assigned {
                    warn!(error = %e, %name, "Could not attach a tag (non-fatal)");
                }
            }
            Err(e) => warn!(error = %e, %name, "Could not create a tag (non-fatal)"),
        }
    }

    // Tags are part of the FTS document, so the row has to be re-indexed or it
    // stays unfindable by the tag it was just given.
    if let Err(e) = crate::search::reindex_assets(&handle.pool, &ids).await {
        warn!(error = %e, "Could not reindex after tagging (non-fatal)");
    }
}

/// Tell the window a capture landed.
///
/// Without this the row exists in the database and the grid never hears about
/// it: the frontend's manifest is a snapshot taken at load time, and a write
/// arriving over HTTP is invisible to it until something else forces a reload.
/// Nothing here assumes a window EXISTS — in tray mode the emit finds no
/// listener and that is the normal case, not a failure.
fn announce<R: Runtime>(state: &SharedBridge<R>, outcome: &CaptureOutcome) {
    // A duplicate changed nothing, so a reload would only cost a re-stream of
    // the whole manifest to show exactly what is already on screen.
    if outcome.asset_id.is_none() {
        return;
    }
    if let Err(e) = state.app.emit("bridge-captured", outcome) {
        tracing::debug!(error = %e, "No listener for bridge-captured");
    }
}

fn failed(e: anyhow::Error) -> Response {
    warn!(error = %e, "Bridge capture failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": "failed", "message": e.to_string() })),
    )
        .into_response()
}

/// Prefer the page's title for the name, but never let it decide the EXTENSION —
/// that comes from what the bytes actually are.
fn title_to_filename(title: Option<&str>, fallback: &str, extension: &str) -> String {
    let Some(title) = title.map(str::trim).filter(|t| !t.is_empty()) else {
        return fallback.to_string();
    };
    let stem = crate::remote::sanitize_stem(title);
    if extension.is_empty() {
        stem
    } else {
        format!("{stem}.{extension}")
    }
}

async fn download_and_import<R: Runtime>(
    state: &SharedBridge<R>,
    handle: &crate::db::LibraryHandle,
    url: &str,
    filename: &str,
    folder_id: Option<String>,
    source_url: &str,
) -> anyhow::Result<CaptureOutcome> {
    let staging = std::env::temp_dir().join(format!("nova-bridge-{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir_all(&staging).await?;
    let staged = staging.join(filename);

    let result = async {
        crate::remote::download(url, &staged, |_, _| {}).await?;
        assets::import_single_file(
            &handle.pool,
            &handle.root,
            Arc::new(SilentProgress),
            staged.clone(),
            folder_id,
            Some(source_url),
        )
        .await
    }
    .await;

    let _ = tokio::fs::remove_dir_all(&staging).await;
    let _ = state; // reserved: progress emission once the UI wants it

    let imported = result?;
    Ok(outcome_from(imported))
}

fn outcome_from(result: assets::ImportResult) -> CaptureOutcome {
    match result.assets.first() {
        Some(asset) => CaptureOutcome {
            status: "imported".into(),
            asset_id: Some(asset.id.clone()),
            filename: Some(asset.filename.clone()),
            duplicate: false,
        },
        // No new row and a duplicate counted: the library already held these
        // exact bytes. Worth saying so — "saved" would be a lie and "failed"
        // would be worse.
        None => CaptureOutcome {
            status: if result.duplicates > 0 {
                "duplicate".into()
            } else {
                "skipped".into()
            },
            asset_id: None,
            filename: None,
            duplicate: result.duplicates > 0,
        },
    }
}

/// Import progress has nowhere useful to go for a single web capture, and the
/// bridge may be running with no window at all.
struct SilentProgress;
impl assets::ProgressReporter for SilentProgress {
    fn report(&self, _progress: assets::ImportProgress) {}
}

/// Receive the bytes a content script fetched with the page's cookies.
///
/// Streamed to disk rather than buffered: a capture can be hundreds of megabytes
/// and holding that in memory to then write it out would double the cost for no
/// reason. The cap is enforced against bytes ACTUALLY received, not a declared
/// Content-Length, because a client that lies about its length is exactly what a
/// cap is for.
#[instrument(skip_all, fields(capture = %capture_id))]
async fn upload<R: Runtime>(
    State(state): State<SharedBridge<R>>,
    Path(capture_id): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    use tauri::Manager;
    let _lease = state.lease.guard();

    let ticket = headers
        .get(TICKET_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();

    // The claim carries the folder and tags chosen on the FIRST request. This
    // upload is a separate HTTP call with no memory of that, so without it the
    // filing silently vanishes on exactly the sites where the bytes fallback
    // triggers — which is nobody's idea of a good place for a silent bug.
    let Some(claim) = state.redeem_ticket(ticket, &capture_id).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    info!(client = %claim.client_id, "Receiving capture bytes");

    if let Some(previous) = state.recall(&capture_id).await {
        return Json(previous).into_response();
    }

    let handle = match state.app.state::<DbState>().acquire().await {
        Ok(handle) => handle,
        Err(_) => return no_library(),
    };

    let filename = headers
        .get("x-nova-filename")
        .and_then(|v| v.to_str().ok())
        .map(crate::remote::sanitize_stem)
        .unwrap_or_else(|| "capture".to_string());
    let source_url = headers
        .get("x-nova-source")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();

    let staging = std::env::temp_dir().join(format!("nova-bridge-{}", uuid::Uuid::new_v4()));
    if tokio::fs::create_dir_all(&staging).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    let result = stream_to_file(body, &staging, &filename).await;
    let staged = match result {
        Ok(path) => path,
        Err(e) => {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            return failed(e);
        }
    };

    let imported = assets::import_single_file(
        &handle.pool,
        &handle.root,
        Arc::new(SilentProgress),
        staged,
        claim.folder_id.clone(),
        (!source_url.is_empty()).then_some(source_url.as_str()),
    )
    .await;
    let _ = tokio::fs::remove_dir_all(&staging).await;

    match imported {
        Ok(result) => {
            let outcome = outcome_from(result);
            apply_tags(&handle, &outcome, &claim.tags).await;
            state.remember(&capture_id, outcome.clone()).await;
            announce(&state, &outcome);
            Json(outcome).into_response()
        }
        Err(e) => failed(e),
    }
}

/// Write the request body out, sniffing the extension from the leading bytes.
///
/// The name the client supplied contributes its STEM only. What the file IS gets
/// decided here, from the content — the same rule the URL path follows, and for
/// the same reason: a caller should be able to name a capture, never to retype
/// what it is.
async fn stream_to_file(
    body: Body,
    dir: &std::path::Path,
    stem: &str,
) -> anyhow::Result<std::path::PathBuf> {
    let temp = dir.join("capture.part");
    let mut file = tokio::fs::File::create(&temp).await?;
    let mut stream = body.into_data_stream();
    let mut received = 0usize;
    let mut head = Vec::with_capacity(1024);

    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        received += chunk.len();
        if received > MAX_UPLOAD_BYTES {
            anyhow::bail!(
                "That capture is larger than the {}MB limit",
                MAX_UPLOAD_BYTES / (1024 * 1024)
            );
        }
        if head.len() < 1024 {
            head.extend_from_slice(&chunk[..chunk.len().min(1024 - head.len())]);
        }
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    drop(file);

    if received == 0 {
        anyhow::bail!("The upload was empty");
    }

    let extension = infer::get(&head)
        .map(|kind| kind.extension().to_string())
        .unwrap_or_default();
    let named = if extension.is_empty() {
        dir.join(stem)
    } else {
        dir.join(format!("{stem}.{extension}"))
    };
    tokio::fs::rename(&temp, &named).await?;
    Ok(named)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_extension_origins_are_recognised() {
        assert!(is_extension_origin("moz-extension://abc-123"));
        assert!(is_extension_origin("chrome-extension://deadbeef"));
        // The whole point: a web page must never look like an extension.
        assert!(!is_extension_origin("https://evil.com"));
        assert!(!is_extension_origin("http://127.0.0.1:41595"));
        assert!(!is_extension_origin("null"));
        assert!(!is_extension_origin(""));
    }

    fn headers_with(name: header::HeaderName, value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(name, HeaderValue::from_str(value).unwrap());
        headers
    }

    #[test]
    fn only_the_loopback_literal_passes_the_host_check() {
        assert!(host_is_loopback(&headers_with(header::HOST, "127.0.0.1:41595")));
        assert!(host_is_loopback(&headers_with(header::HOST, "127.0.0.1")));

        // A rebound request carries the attacker's name, which is the signature
        // of the attack this check exists for.
        assert!(!host_is_loopback(&headers_with(header::HOST, "evil.com")));
        assert!(!host_is_loopback(&headers_with(
            header::HOST,
            "evil.com:41595"
        )));
        // Rejected deliberately: a NAME can be made to resolve anywhere.
        assert!(!host_is_loopback(&headers_with(header::HOST, "localhost:41595")));
        assert!(!host_is_loopback(&HeaderMap::new()));
    }

    #[test]
    fn only_the_exact_upload_shape_skips_the_token() {
        assert!(is_upload_path("/capture/abc-123/upload"));

        // Everything else under /capture stays token-authenticated. A prefix
        // match would have handed the whole subtree to anyone holding a ticket.
        assert!(!is_upload_path("/capture"));
        assert!(!is_upload_path("/capture/abc"));
        assert!(!is_upload_path("/capture//upload"));
        assert!(!is_upload_path("/capture/abc/upload/extra"));
        assert!(!is_upload_path("/capture/abc/uploadX"));
        assert!(!is_upload_path("/folders"));
    }

    #[test]
    fn a_supplied_title_never_changes_the_file_type() {
        // The stem is the caller's to choose; the extension is not.
        assert_eq!(
            title_to_filename(Some("My Art"), "abc.png", "png"),
            "My Art.png"
        );
        // A title that tries to smuggle an extension keeps the sniffed one.
        assert_eq!(
            title_to_filename(Some("payload.exe"), "abc.png", "png"),
            "payload.exe.png"
        );
        // Path separators cannot escape the staging directory.
        let escaped = title_to_filename(Some("../../etc/passwd"), "abc.png", "png");
        assert!(!escaped.contains('/'), "got {escaped}");
        assert!(!escaped.contains('\\'), "got {escaped}");
        // No usable title falls back to what the probe worked out.
        assert_eq!(title_to_filename(None, "abc.png", "png"), "abc.png");
        assert_eq!(title_to_filename(Some("   "), "abc.png", "png"), "abc.png");
    }
}
