// Adding an asset from a URL.
//
// A thin typed wrapper over the two `remote.rs` commands. Every HTTP request
// Nova makes happens in Rust — the webview never fetches a user-supplied URL —
// so this file is only shapes and one string test.

import { invoke, convertFileSrc } from "@tauri-apps/api/core";
import type { AssetMetadata } from "./assets.svelte";

/**
 * URL the webview reads an online asset through.
 *
 * `convertFileSrc` handles the platform split for us: Windows serves custom
 * protocols as `http://nova-remote.localhost/…` while macOS and Linux use
 * `nova-remote://localhost/…`. Both forms are in the CSP.
 */
export function remoteAssetUrl(id: string): string {
  return convertFileSrc(id, "nova-remote");
}

/** True when this asset's bytes are NOT on disk. The one state test that matters. */
export function isRemote(asset: { origin?: string } | undefined | null): boolean {
  return asset?.origin === "remote";
}

/**
 * What a link turned out to be, from ONE ranged GET.
 *
 * Mirrors `remote::UrlProbe`. Every field here exists to be SHOWN: the receipt
 * dialog puts the whole thing in front of the user before anything is created,
 * which is what lets Nova be truthful about sites it knows nothing about.
 */
export interface UrlProbe {
  /** As the user typed it — what they'll recognise. */
  original_url: string;
  /** Where the redirect chain ended. Worth showing when it differs. */
  final_url: string;
  host: string;
  filename: string;
  extension: string;
  /** From the SNIFFED bytes where possible, not from the URL's spelling. */
  asset_type: "image" | "audio" | "video" | "unknown";
  content_type: string;
  /** null when the server declined to say; the download then has no total. */
  size: number | null;
  supports_range: boolean;
  /** A web page, not a media file — the most common paste mistake. */
  is_html: boolean;
  looks_temporary: boolean;
  /** e.g. "about 8 hours", when it can be worked out. */
  expires_in: string | null;
}

export interface UrlImportResult {
  assets: AssetMetadata[];
  duplicates: number;
  restored: number;
}

/** Ask what is at a URL. Creates nothing. Rejects with a showable sentence. */
export function probeUrl(url: string): Promise<UrlProbe> {
  return invoke<UrlProbe>("probe_url", { url });
}

/**
 * Download a URL into the library. Emits `url-download-progress` as `[received,
 * total]` while the bytes arrive, then the ordinary `import-progress` events
 * once the file reaches the import pipeline.
 *
 * `filename` is advisory: Rust honours only its STEM and always applies the
 * extension it sniffed, so the name can be changed but the type cannot.
 */
export function importFromUrl(
  url: string,
  filename: string | null,
  targetFolder: string | null,
): Promise<UrlImportResult> {
  return invoke<UrlImportResult>("import_from_url", { url, filename, targetFolder });
}

/**
 * Record a URL as an asset WITHOUT downloading it — "Save link only".
 *
 * The asset behaves like any other: it appears in the grid, filters, sorts,
 * takes tags and folders. Only opening it at full size needs the network.
 */
export function addRemoteAsset(
  url: string,
  filename: string | null,
  targetFolder: string | null,
): Promise<AssetMetadata> {
  return invoke<AssetMetadata>("add_remote_asset", { url, filename, targetFolder });
}

/**
 * Download an online asset's bytes and make it local. One-way and permanent.
 * Emits `remote-download-progress` as `[id, received, total]`.
 */
export function keepRemoteOffline(id: string): Promise<AssetMetadata> {
  return invoke<AssetMetadata>("keep_remote_offline", { id });
}

/** Re-check the link. Returns the new state. */
export function verifyRemoteAsset(id: string): Promise<"ok" | "unavailable"> {
  return invoke<"ok" | "unavailable">("verify_remote_asset", { id });
}

/**
 * Report a duration the webview measured. Rust has no media decoder, so this is
 * the only way `duration_ms` is ever filled in.
 */
export function setMediaDuration(id: string, durationMs: number): Promise<void> {
  return invoke<void>("set_media_duration", { id, durationMs: Math.round(durationMs) });
}

/**
 * Is this pasted text worth treating as a link?
 *
 * Deliberately strict: paste is a global shortcut, and treating every clipboard
 * string as a candidate would pop a dialog when someone copies a tag name. Only
 * an explicit http/https URL qualifies — anything else is left alone, and the
 * backend refuses other schemes anyway.
 */
export function looksLikeUrl(text: string): boolean {
  const trimmed = text.trim();
  if (!/^https?:\/\//i.test(trimmed)) return false;
  if (/\s/.test(trimmed)) return false;
  try {
    return Boolean(new URL(trimmed).host);
  } catch {
    return false;
  }
}
