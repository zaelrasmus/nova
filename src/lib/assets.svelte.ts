// Streams the manifest via Channel. Caches Heavy rows by id with batched loading + eviction.

import { invoke, Channel } from "@tauri-apps/api/core";
import { SvelteMap } from "svelte/reactivity";
import { thumbHashToDataURL } from "thumbhash";

export interface AssetLightRow {
  id: string;
  width: number;
  height: number;
  asset_type: "image" | "audio" | "video" | "unknown";
  thumb_hash: string | null;
  is_animated: boolean;
}

export interface AssetMetadata extends AssetLightRow {
  filename: string;
  extension: string;
  dest_path: string;
  imported_date: string;
  creation_date: string;
  modified_date: string;

  thumb_path: string; // "" => no thumbnail; fallback to dest_path
}

// ThumbHash (base64) -> data URL, memoized (cards mount/unmount on scroll).
const thumbUrlCache = new Map<string, string>();
export function thumbHashUrl(hash: string | null): string | null {
  if (!hash) return null;
  let url = thumbUrlCache.get(hash);
  if (!url) {
    const bin = atob(hash);
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    url = thumbHashToDataURL(bytes);
    thumbUrlCache.set(hash, url);
  }
  return url;
}

// Cap on hydrated heavy rows kept in memory. A few sccreenfuls of slacks.
const MAX_HEAVY = 600;

class AssetLibrary {
  /** Layout source of truth: light rows for every asset, sort order from Rust. */

  manifest = $state<AssetLightRow[]>([]);
  isLoading = $state(false);
  error = $state<string | null>(null);

  /** Heavy rows keyed by id, hydrated per visible window. Reactive. */
  heavy = new SvelteMap<string, AssetMetadata>();

  #pending = new Set<string>();
  #loadToken = 0;

  /** (Re)load the full manifest for the active library. */
  async load(): Promise<void> {
    const token = ++this.#loadToken;
    this.isLoading = true;
    this.error = null;
    this.manifest = [];
    this.heavy.clear();
    this.#pending.clear();

    try {
      const channel = new Channel<AssetLightRow[]>();
      const collected: AssetLightRow[] = [];
      channel.onmessage = (chunk) => {
        if (token !== this.#loadToken) return; // a newer load superseded this one
        collected.push(...chunk);
        this.manifest = collected.slice(); // publish progressively as chunks arrive
      };
      await invoke("stream_manifest", { onChunk: channel });
    } catch (e) {
      if (token === this.#loadToken) {
        this.error = typeof e === "string" ? e : "Failed to load assets.";
      }
    } finally {
      if (token === this.#loadToken) this.isLoading = false;
    }
  }

  reload(): Promise<void> {
    return this.load();
  }

  /**
   * Kick off background thumbnail generation for any images still missing one
   * (freshly imported, or interrupted on a previous run). Fire-and-forget: the
   * backend emits `thumbnail-progress` as rows fill in; a run already in flight
   * makes this a no-op. Thumbnails are a rebuildable cache, so a failure here is
   * non-fatal and simply retried the next time the library opens.
   */
  async generateThumbnails(mode: string): Promise<void> {
    try {
      await invoke("generate_thumbnails", { thumbMode: mode });
    } catch {
      /* non-fatal */
    }
  }

  /**
   * Patch freshly-generated thumbnails into the manifest in place, without a
   * full reload (which would flash the grid empty). Sets each row's thumb_hash
   * so the ThumbHash placeholder appears immediately, and evicts the stale heavy
   * row (its thumb_path was "") so it re-hydrates with the real thumbnail — the
   * manifest reassignment re-runs AssetGrid's hydration effect, so no scroll is
   * needed. (Phase 2: index rows by id to avoid the O(n) manifest map at scale.)
   */
  applyThumbnails(ready: { id: string; thumb_hash: string }[]): void {
    if (!ready.length) return;
    const byId = new Map(ready.map((r) => [r.id, r.thumb_hash]));
    this.manifest = this.manifest.map((row) =>
      byId.has(row.id) ? { ...row, thumb_hash: byId.get(row.id)! } : row,
    );
    for (const r of ready) this.heavy.delete(r.id);
  }

  /** Hydrate heavy rows for the given ids (visible window + overscan). */
    async ensure(ids: string[]): Promise<void> {
      const missing = ids.filter((id) => !this.heavy.has(id) && !this.#pending.has(id));
      if (missing.length) {
        missing.forEach((id) => this.#pending.add(id));
        try {
          const rows = await invoke<AssetMetadata[]>("fetch_assets_by_ids", { ids: missing });
          for (const row of rows) this.heavy.set(row.id, row);
        } catch {
          // Swallow — the cell keeps its placeholder and retries on next scroll.
        } finally {
          missing.forEach((id) => this.#pending.delete(id));
        }
      }
      this.#evict(ids);
    }

    #evict(keep: string[]): void {
      if (this.heavy.size <= MAX_HEAVY) return;
      const keepSet = new Set(keep);
      for (const id of this.heavy.keys()) {
        // SvelteMap iterates in insertion order → oldest, non-visible first.
        if (this.heavy.size <= MAX_HEAVY) break;
        if (!keepSet.has(id)) this.heavy.delete(id);
      }
    }
  }

  export const assetLibrary = new AssetLibrary();
