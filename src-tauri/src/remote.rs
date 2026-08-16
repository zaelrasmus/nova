//! Fetching an asset from a URL.
//!
//! Two operations, deliberately separate: [`probe`] answers "what is at this
//! link, and can it be trusted?" in ONE cheap request, and [`download`] streams
//! the bytes once the user has agreed to what the probe reported.
//!
//! The probe is the honesty mechanism of the whole feature. A single ranged GET
//! yields the status (so a dead link never becomes an asset), the redirect chain,
//! the real size, what the bytes ACTUALLY are, whether seeking will work, and
//! whether the link is one of the signed, expiring kind. The UI shows that back
//! as a receipt before anything is created — which means Nova needs no per-site
//! knowledge to be truthful about any site.
//!
//! Everything HTTP happens HERE, in Rust, never in the webview: it is the only
//! way to set a Referer and User-Agent (a great many CDNs 403 without them),
//! it keeps CORS out of the picture entirely, and it means the page's CSP never
//! has to allow arbitrary remote origins.

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use reqwest::{header, redirect, Client, StatusCode, Url};
use serde::Serialize;
use std::net::IpAddr;
use std::path::Path;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tracing::{debug, instrument, warn};

use crate::assets::AssetType;

/// Bytes the probe pulls: enough for a magic-number sniff, small enough to be
/// free even on a metered connection.
const PROBE_BYTES: usize = 1024;
const MAX_REDIRECTS: usize = 5;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);
/// Ceiling on one download. Not a policy about disk space — a guard against a
/// misbehaving server streaming forever.
pub const MAX_DOWNLOAD_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// Progress is reported per this many bytes rather than per chunk: chunks arrive
/// thousands of times a second and each report crosses the IPC boundary.
const PROGRESS_STEP: u64 = 512 * 1024;

/// A browser User-Agent, and yes that is deliberate. A large share of CDNs
/// (Twitter's included) refuse anything that doesn't look like a browser, so an
/// honest `Nova/1.0` string would turn "this feature works" into "this feature
/// 403s on the sites people actually use".
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                  (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

/// What a link turned out to be. Everything the receipt dialog shows.
#[derive(Serialize, Debug, Clone)]
pub struct UrlProbe {
    /// Verbatim as the user gave it — that is what they will recognise.
    pub original_url: String,
    /// Where the redirect chain actually ended. Shown when it differs.
    pub final_url: String,
    pub host: String,
    /// Suggested name, filesystem-legal, carrying the sniffed extension.
    pub filename: String,
    pub extension: String,
    /// Derived from the SNIFFED bytes where possible, not from the URL's spelling.
    pub asset_type: AssetType,
    pub content_type: String,
    /// `None` when the server declined to say; the download then has no total.
    pub size: Option<u64>,
    /// Whether seeking will work when this is streamed. Phase 3 needs it; the
    /// receipt reports it now because it is free here and expensive later.
    pub supports_range: bool,
    /// A web page rather than a media file — the most common paste mistake, and
    /// worth its own message instead of a generic failure.
    pub is_html: bool,
    /// A signed/expiring link (Discord, S3 presigned, CloudFront).
    pub looks_temporary: bool,
    /// Human phrase for when it dies, when that can be worked out.
    pub expires_in: Option<String>,
}

// ── Safety ───────────────────────────────────────────────────────────────────

/// Reject addresses that would make Nova a confused deputy on the user's own
/// network: loopback, RFC1918, carrier-grade NAT, and link-local — the last of
/// which covers the cloud metadata endpoint at 169.254.169.254.
fn is_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_unspecified()
                || o[0] == 0
                || (o[0] == 100 && (64..128).contains(&o[1])) // 100.64/10 CGNAT
        }
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            v6.is_loopback()
                || v6.is_unspecified()
                || (first & 0xfe00) == 0xfc00 // fc00::/7  unique-local
                || (first & 0xffc0) == 0xfe80 // fe80::/10 link-local
        }
    }
}

/// Synchronous checks: scheme, and a host given as a private IP LITERAL.
///
/// Also used for every redirect hop, where it is all we can do — the redirect
/// policy callback is sync, so a hop to a *hostname* is not re-resolved. The
/// residual gap is DNS rebinding, which needs a custom resolver to close and is
/// not a threat worth that machinery for a desktop app fetching links its own
/// user pasted.
fn guard_url(url: &Url) -> std::result::Result<(), String> {
    match url.scheme() {
        "http" | "https" => {}
        other => return Err(format!("{other}:// links aren't supported, only http and https")),
    }
    let Some(host) = url.host_str() else {
        return Err("That URL has no host".into());
    };
    if let Ok(ip) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
        if is_forbidden_ip(ip) {
            return Err(format!("{host} is a private address"));
        }
    }
    Ok(())
}

/// `guard_url`, plus a DNS resolution whose every answer must also be public.
async fn guard_host(url: &Url) -> Result<()> {
    guard_url(url).map_err(|e| anyhow::anyhow!(e))?;

    let host = url.host_str().context("That URL has no host")?;
    if host.trim_matches(['[', ']']).parse::<IpAddr>().is_ok() {
        return Ok(()); // literal, already checked
    }

    let port = url.port_or_known_default().unwrap_or(443);
    let mut resolved = false;
    for addr in tokio::net::lookup_host((host, port))
        .await
        .with_context(|| format!("Could not find {host}"))?
    {
        resolved = true;
        if is_forbidden_ip(addr.ip()) {
            bail!("{host} points at a private address");
        }
    }
    if !resolved {
        bail!("Could not find {host}");
    }
    Ok(())
}

fn client() -> Result<Client> {
    Client::builder()
        .user_agent(UA)
        .connect_timeout(CONNECT_TIMEOUT)
        .redirect(redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                return attempt.error("too many redirects");
            }
            match guard_url(attempt.url()) {
                Ok(()) => attempt.follow(),
                Err(e) => attempt.error(e),
            }
        }))
        .build()
        .context("Failed to build the HTTP client")
}

// ── Naming ───────────────────────────────────────────────────────────────────

/// Strip everything a filesystem (or an outbound drag hardlink) would choke on.
/// The result is used verbatim as the imported asset's filename.
///
/// Also applied to a name the USER typed in the receipt dialog — and only ever
/// to the stem, never the extension, so a rename in the dialog can change what
/// the file is called but not what Nova thinks it is.
pub(crate) fn sanitize_stem(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').trim().to_string();
    if cleaned.is_empty() {
        "download".to_string()
    } else {
        cleaned.chars().take(120).collect()
    }
}

/// Last path segment of the URL, percent-decoded, split into stem + extension.
fn name_from_url(url: &Url) -> (String, Option<String>) {
    let last = url
        .path_segments()
        .and_then(|mut s| s.next_back())
        .unwrap_or("");
    let decoded = percent_decode(last);
    match decoded.rsplit_once('.') {
        Some((stem, ext)) if !ext.is_empty() && ext.len() <= 8 && !stem.is_empty() => {
            (sanitize_stem(stem), Some(ext.to_ascii_lowercase()))
        }
        _ => (sanitize_stem(&decoded), None),
    }
}

/// Minimal percent-decoding — enough for filenames, without a new dependency.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ── Expiry heuristics ────────────────────────────────────────────────────────

fn humanize_until(ts: i64) -> String {
    let delta = ts - Utc::now().timestamp();
    if delta <= 0 {
        return "already expired".into();
    }
    let hours = delta / 3600;
    let days = hours / 24;
    if days >= 2 {
        format!("about {days} days")
    } else if hours >= 2 {
        format!("about {hours} hours")
    } else {
        format!("about {} minutes", (delta / 60).max(1))
    }
}

/// Spot the signed, time-limited links that will be dead tomorrow.
///
/// Heuristic by nature — it recognises the three families that account for
/// almost all of them (Discord, S3/GCS presigned, CloudFront/Azure) and falls
/// back to "there is a signature here, so assume temporary" for the rest. It
/// will miss cases, which is exactly why the UI treats a POSITIVE result as a
/// reason to default to downloading rather than as the only warning it gives.
fn expiry_of(url: &Url) -> (bool, Option<String>) {
    let mut signed = false;
    let mut amz_expires: Option<i64> = None;
    let mut amz_date: Option<i64> = None;

    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            // Discord CDN: hex unix seconds.
            "ex" => {
                if let Ok(ts) = i64::from_str_radix(value.as_ref(), 16) {
                    return (true, Some(humanize_until(ts)));
                }
                signed = true;
            }
            // CloudFront / Azure SAS / generic: decimal unix seconds.
            "Expires" | "expires" | "se" => {
                if let Ok(ts) = value.parse::<i64>() {
                    return (true, Some(humanize_until(ts)));
                }
                signed = true;
            }
            "X-Amz-Expires" | "X-Goog-Expires" => amz_expires = value.parse().ok(),
            "X-Amz-Date" | "X-Goog-Date" => {
                // Basic ISO8601 form, e.g. 20260815T101530Z
                amz_date = DateTime::parse_from_str(
                    &format!("{value}+0000"),
                    "%Y%m%dT%H%M%SZ%z",
                )
                .ok()
                .map(|d| d.timestamp());
                signed = true;
            }
            "hm" | "Signature" | "X-Amz-Signature" | "X-Goog-Signature" | "sig" | "token"
            | "Key-Pair-Id" => signed = true,
            _ => {}
        }
    }

    if let (Some(start), Some(ttl)) = (amz_date, amz_expires) {
        return (true, Some(humanize_until(start + ttl)));
    }
    (signed, None)
}

// ── Probe ────────────────────────────────────────────────────────────────────

/// Ask what is at `raw` using ONE ranged GET.
///
/// A ranged GET rather than a HEAD on purpose: plenty of CDNs answer HEAD with a
/// 405, and plenty more lie about `Accept-Ranges` there. A 1KB GET is nearly as
/// cheap and tells the truth, and it doubles as the content sniff.
#[instrument(skip_all, fields(url = %raw))]
pub async fn probe(raw: &str) -> Result<UrlProbe> {
    let original = raw.trim();
    let url = Url::parse(original).context("That doesn't look like a link")?;
    guard_host(&url).await?;

    let response = client()?
        .get(url.clone())
        .header(header::RANGE, format!("bytes=0-{}", PROBE_BYTES - 1))
        // Own-origin Referer: satisfies the majority of hotlink checks and leaks
        // nothing the host doesn't already know.
        .header(header::REFERER, format!("{}://{}/", url.scheme(), url.host_str().unwrap_or("")))
        .timeout(PROBE_TIMEOUT)
        .send()
        .await
        .context("Could not reach that link")?;

    let status = response.status();
    if !status.is_success() {
        bail!(
            "The server answered {} for that link",
            status.as_u16()
        );
    }

    let final_url = response.url().clone();
    let headers = response.headers().clone();

    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();

    let accepts_ranges = headers
        .get(header::ACCEPT_RANGES)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("bytes"));
    let partial = status == StatusCode::PARTIAL_CONTENT;
    let supports_range = partial || accepts_ranges;

    // A 206 reports the slice in Content-Length; the total is after the slash in
    // Content-Range. A 200 means the range was ignored and the body is whole.
    let size = if partial {
        headers
            .get(header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit('/').next())
            .and_then(|v| v.parse::<u64>().ok())
    } else {
        response.content_length()
    };

    // Take only the head, then drop the response — on a 200 the server is
    // otherwise willing to send us the entire file.
    let head = read_head(response, PROBE_BYTES).await?;

    let sniffed = infer::get(&head);
    let is_html = content_type.starts_with("text/html") || looks_like_html(&head);

    let (url_stem, url_ext) = name_from_url(&final_url);
    let extension = sniffed
        .map(|k| k.extension().to_ascii_lowercase())
        .or(url_ext)
        .unwrap_or_default();

    let asset_type = crate::assets::asset_type_for_extension(&extension);
    let (looks_temporary, expires_in) = expiry_of(&final_url);

    let filename = if extension.is_empty() {
        url_stem
    } else {
        format!("{url_stem}.{extension}")
    };

    debug!(%content_type, ?size, supports_range, is_html, "Probed");

    Ok(UrlProbe {
        original_url: original.to_string(),
        final_url: final_url.to_string(),
        host: final_url.host_str().unwrap_or("").to_string(),
        filename,
        extension,
        asset_type,
        content_type,
        size,
        supports_range,
        is_html,
        looks_temporary,
        expires_in,
    })
}

fn looks_like_html(head: &[u8]) -> bool {
    let start = String::from_utf8_lossy(&head[..head.len().min(256)]).to_ascii_lowercase();
    let start = start.trim_start();
    start.starts_with("<!doctype html") || start.starts_with("<html")
}

/// Read at most `limit` bytes, then drop the response (closing the connection).
async fn read_head(response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    let mut stream = response.bytes_stream();
    let mut buf = Vec::with_capacity(limit);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("The connection dropped while reading")?;
        buf.extend_from_slice(&chunk);
        if buf.len() >= limit {
            break;
        }
    }
    buf.truncate(limit);
    Ok(buf)
}

// ── Download ─────────────────────────────────────────────────────────────────

/// Stream `url` to `dest`, reporting `(received, total)` as it goes.
///
/// Enforces `MAX_DOWNLOAD_BYTES` against the bytes actually received, not
/// against the advertised Content-Length — a server that lies about its length
/// is exactly the case the cap exists for. A partial file is removed on failure
/// rather than left for the import pipeline to find.
#[instrument(skip_all, fields(url = %url, dest = ?dest))]
pub async fn download<F>(url: &str, dest: &Path, on_progress: F) -> Result<u64>
where
    F: Fn(u64, Option<u64>) + Send,
{
    let parsed = Url::parse(url.trim()).context("That doesn't look like a link")?;
    guard_host(&parsed).await?;

    let response = client()?
        .get(parsed.clone())
        .header(
            header::REFERER,
            format!("{}://{}/", parsed.scheme(), parsed.host_str().unwrap_or("")),
        )
        .send()
        .await
        .context("Could not reach that link")?;

    let status = response.status();
    if !status.is_success() {
        bail!("The server answered {} for that link", status.as_u16());
    }

    let total = response.content_length();
    if total.is_some_and(|t| t > MAX_DOWNLOAD_BYTES) {
        bail!("That file is larger than the {} GB limit", MAX_DOWNLOAD_BYTES / (1024 * 1024 * 1024));
    }

    let result = stream_to_file(response, dest, total, on_progress).await;
    if result.is_err() {
        // Never leave a truncated file where the import pipeline could pick it up.
        if let Err(e) = tokio::fs::remove_file(dest).await {
            warn!(error = %e, ?dest, "Could not clean up a failed download (non-fatal)");
        }
    }
    result
}

async fn stream_to_file<F>(
    response: reqwest::Response,
    dest: &Path,
    total: Option<u64>,
    on_progress: F,
) -> Result<u64>
where
    F: Fn(u64, Option<u64>) + Send,
{
    let mut file = tokio::fs::File::create(dest)
        .await
        .with_context(|| format!("Could not create {dest:?}"))?;

    let mut stream = response.bytes_stream();
    let mut received: u64 = 0;
    let mut next_report: u64 = 0;

    on_progress(0, total);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("The connection dropped mid-download")?;
        received += chunk.len() as u64;
        if received > MAX_DOWNLOAD_BYTES {
            bail!("That download exceeded the size limit and was stopped");
        }
        file.write_all(&chunk)
            .await
            .context("Could not write the downloaded file")?;

        if received >= next_report {
            on_progress(received, total);
            next_report = received + PROGRESS_STEP;
        }
    }

    file.flush().await.context("Could not flush the download")?;
    on_progress(received, total);

    if received == 0 {
        bail!("The server sent an empty file");
    }
    Ok(received)
}

// ── Streaming proxy ──────────────────────────────────────────────────────────
//
// Backs `nova-remote://<asset id>`, which is what the viewer points a <video> or
// an <img> at when the bytes aren't local.
//
// The webview does the hard part. It issues its own `Range` requests as playback
// moves, so this never has to hold a streaming body open — it forwards one range,
// returns that slice complete, and is done. Two consequences worth stating:
//
//   · Memory is bounded by MAX_CHUNK regardless of file size.
//   · Cancellation stops being a problem. When the user navigates away the
//     webview drops the connection, and Tauri does not tell us — but an orphaned
//     upstream fetch can only ever be one small chunk, so it finishes and goes
//     away on its own instead of downloading a 2GB file into nothing.

/// Largest slice served in one response. Small enough that an abandoned request
/// is cheap; large enough that playback isn't a request storm.
const MAX_CHUNK: u64 = 4 * 1024 * 1024;
/// A server that ignores `Range` can only be served by buffering it whole, so
/// there has to be a point where we refuse and say "download it instead".
const NO_RANGE_MAX: u64 = 128 * 1024 * 1024;

/// One slice of a remote asset, ready to become an HTTP response.
pub struct Slice {
    pub status: u16,
    pub content_type: String,
    /// Set when answering with 206 — `bytes start-end/total`.
    pub content_range: Option<String>,
    pub body: Vec<u8>,
}

/// Parse the single-range form media elements actually send: `bytes=start-` or
/// `bytes=start-end`. Multi-range is not something a media element asks for, and
/// answering one wrongly is worse than ignoring it.
fn parse_range(header: &str) -> Option<(u64, Option<u64>)> {
    let spec = header.trim().strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None;
    }
    let (start, end) = spec.split_once('-')?;
    let start: u64 = start.trim().parse().ok()?;
    let end = end.trim();
    let end = if end.is_empty() {
        None
    } else {
        Some(end.parse::<u64>().ok()?)
    };
    Some((start, end))
}

/// Fetch the requested slice of `url`.
#[instrument(skip_all, fields(url = %url, range = ?range))]
pub async fn fetch_slice(url: &str, range: Option<&str>) -> Result<Slice> {
    let parsed = Url::parse(url.trim()).context("Stored link is not a valid URL")?;
    guard_host(&parsed).await?;

    let (start, requested_end) = range.and_then(parse_range).unwrap_or((0, None));
    // Cap the slice ourselves even when the webview asked for more (or for the
    // whole remainder) — this is the bound that makes an abandoned request cheap.
    let end = requested_end
        .unwrap_or(start + MAX_CHUNK - 1)
        .min(start + MAX_CHUNK - 1);

    let response = client()?
        .get(parsed.clone())
        .header(header::RANGE, format!("bytes={start}-{end}"))
        .header(
            header::REFERER,
            format!("{}://{}/", parsed.scheme(), parsed.host_str().unwrap_or("")),
        )
        .send()
        .await
        .context("Could not reach the source")?;

    let status = response.status();
    if !status.is_success() {
        bail!("The source answered {}", status.as_u16());
    }

    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();

    if status == StatusCode::PARTIAL_CONTENT {
        let content_range = response
            .headers()
            .get(header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        let body = read_head(response, (end - start + 1) as usize).await?;
        return Ok(Slice {
            status: 206,
            content_type,
            content_range,
            body,
        });
    }

    // The server ignored the range, so there is no way to serve this piecemeal:
    // it is the whole file or nothing. Bounded, and the refusal names the fix.
    let total = response.content_length();
    if total.is_some_and(|t| t > NO_RANGE_MAX) {
        bail!("This source doesn't support seeking and the file is too large to stream — download it to the library instead");
    }
    let body = read_head(response, NO_RANGE_MAX as usize).await?;
    Ok(Slice {
        status: 200,
        content_type,
        content_range: None,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_and_metadata_addresses_are_refused() {
        for host in [
            "http://127.0.0.1/x.png",
            "http://localhost.localdomain./x.png", // resolves later, literal check passes
            "http://192.168.1.10/x.png",
            "http://10.0.0.5/x.png",
            "http://169.254.169.254/latest/meta-data", // cloud metadata
            "http://[::1]/x.png",
        ] {
            let url = Url::parse(host).unwrap();
            // Only the IP-literal forms are caught synchronously; the hostname one
            // is here to document that it passes this stage and is caught by DNS.
            if url.host_str().unwrap().parse::<IpAddr>().is_ok()
                || url.host_str().unwrap().starts_with('[')
            {
                assert!(guard_url(&url).is_err(), "{host} must be refused");
            }
        }
    }

    #[test]
    fn public_addresses_and_https_pass() {
        for host in ["https://example.com/a.png", "http://93.184.216.34/a.png"] {
            assert!(guard_url(&Url::parse(host).unwrap()).is_ok(), "{host}");
        }
    }

    #[test]
    fn non_http_schemes_are_refused() {
        for host in ["file:///etc/passwd", "ftp://example.com/a.png"] {
            assert!(guard_url(&Url::parse(host).unwrap()).is_err(), "{host}");
        }
    }

    #[test]
    fn discord_style_links_are_read_as_temporary() {
        // `ex` is hex unix seconds; use one comfortably in the future.
        let future = format!("{:x}", Utc::now().timestamp() + 8 * 3600);
        let url = Url::parse(&format!(
            "https://cdn.discordapp.com/attachments/1/2/clip.mp4?ex={future}&is=abc&hm=def"
        ))
        .unwrap();
        let (temporary, phrase) = expiry_of(&url);
        assert!(temporary, "a signed Discord link must read as temporary");
        assert_eq!(phrase.as_deref(), Some("about 8 hours"));
    }

    #[test]
    fn presigned_s3_links_are_read_as_temporary() {
        let url = Url::parse(
            "https://b.s3.amazonaws.com/k.mp4?X-Amz-Date=20260815T100000Z\
             &X-Amz-Expires=3600&X-Amz-Signature=deadbeef",
        )
        .unwrap();
        let (temporary, _) = expiry_of(&url);
        assert!(temporary);
    }

    #[test]
    fn a_plain_link_is_not_temporary() {
        let url = Url::parse("https://pbs.twimg.com/media/abc.jpg?format=jpg&name=large").unwrap();
        assert_eq!(expiry_of(&url), (false, None));
    }

    #[test]
    fn filenames_are_derived_and_made_legal() {
        let (stem, ext) = name_from_url(&Url::parse("https://x.com/a/My%20Clip%3A2.MP4").unwrap());
        assert_eq!(stem, "My Clip_2");
        assert_eq!(ext.as_deref(), Some("mp4"));

        // No usable segment at all still yields something importable.
        let (stem, ext) = name_from_url(&Url::parse("https://x.com/").unwrap());
        assert_eq!(stem, "download");
        assert!(ext.is_none());
    }

    #[test]
    fn range_headers_parse_in_the_forms_media_elements_send() {
        assert_eq!(parse_range("bytes=0-"), Some((0, None)));
        assert_eq!(parse_range("bytes=1024-2047"), Some((1024, Some(2047))));
        assert_eq!(parse_range(" bytes=500- "), Some((500, None)));
        // Multi-range: ignored rather than answered wrongly.
        assert_eq!(parse_range("bytes=0-99,200-299"), None);
        assert_eq!(parse_range("items=0-10"), None);
        assert_eq!(parse_range("garbage"), None);
    }

    #[test]
    fn a_slice_is_capped_even_when_the_webview_asks_for_everything() {
        // The open-ended form is what a <video> sends first, and it must not be
        // read as "send me the whole 2GB file".
        let (start, end) = parse_range("bytes=0-").unwrap();
        assert_eq!(end, None);
        let capped = end.unwrap_or(start + MAX_CHUNK - 1).min(start + MAX_CHUNK - 1);
        assert_eq!(capped, MAX_CHUNK - 1);

        // An explicit range larger than the cap is clamped too.
        let (start, end) = parse_range("bytes=0-999999999").unwrap();
        let capped = end.unwrap_or(start + MAX_CHUNK - 1).min(start + MAX_CHUNK - 1);
        assert_eq!(capped, MAX_CHUNK - 1);
    }

    #[test]
    fn html_is_detected_from_the_first_bytes() {
        assert!(looks_like_html(b"<!DOCTYPE html><html><head>"));
        assert!(looks_like_html(b"  \n<html lang=\"en\">"));
        assert!(!looks_like_html(&[0xFF, 0xD8, 0xFF, 0xE0])); // JPEG magic
    }
}
