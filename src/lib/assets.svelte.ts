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

  /** id -> manifest index, for O(1) row patching (rebuilt on each load). */
  #indexById = new Map<string, number>();
  /** Ids with an in-flight on-view thumbnail request, to avoid re-requesting. */
  #thumbRequested = new Set<string>();

  /** (Re)load the full manifest for the active library. */
  async load(): Promise<void> {
    const token = ++this.#loadToken;
    this.isLoading = true;
    this.error = null;
    this.manifest = [];
    this.heavy.clear();
    this.#pending.clear();
    this.#thumbRequested.clear();
    this.#indexById.clear();

    try {
      const channel = new Channel<AssetLightRow[]>();
      const collected: AssetLightRow[] = [];
      channel.onmessage = (chunk) => {
        if (token !== this.#loadToken) return; // a newer load superseded this one
        // Build the id->index map incrementally, in lock-step with the manifest.
        // (Channel messages can arrive AFTER the invoke promise resolves, so a
        // one-shot rebuild after `await` would run on an empty manifest.)
        const base = collected.length;
        collected.push(...chunk);
        for (let i = 0; i < chunk.length; i++) this.#indexById.set(chunk[i].id, base + i);
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
   * Rebuild every thumbnail in the active library with `mode` (clears the cache,
   * then regenerates). Resolves with the count once done; rows are patched in
   * place by the `thumbnail-progress` listener as batches complete, so no reload
   * is needed. Note: because regenerated files keep the same `id.webp` path, the
   * webview may show the cached OLD image until restart — the on-disk file (and
   * its size) is updated immediately.
   */
  rebuildThumbnails(mode: string, quality: number): Promise<number> {
    return invoke<number>("rebuild_thumbnails", { thumbMode: mode, quality });
  }

  /**
   * On-view thumbnail generation: request thumbnails for the given ids (the
   * caller passes only images still missing one). De-dupes in-flight ids so
   * scrolling doesn't re-request; the backend also filters `WHERE thumb_hash IS
   * NULL`, so this is idempotent. The backend emits `thumbnail-progress` as
   * batches complete and `applyThumbnails` patches them in. Non-fatal on failure.
   */
  async ensureThumbnails(ids: string[], mode: string, quality: number): Promise<void> {
    const need = ids.filter((id) => !this.#thumbRequested.has(id));
    if (!need.length) return;

    need.forEach((id) => this.#thumbRequested.add(id));
    try {
      await invoke<number>("generate_thumbnails_for_ids", {
        ids: need,
        thumbMode: mode,
        quality,
      });
    } catch (e) {
      console.error("Thumbnail generation request failed:", e);
    } finally {
      need.forEach((id) => this.#thumbRequested.delete(id));
    }
  }

  /**
   * Patch freshly-generated thumbnails into their rows in place — O(batch), no
   * manifest reload (which would flash the grid). Sets `thumb_hash` so the
   * ThumbHash placeholder appears, and, if the heavy row is cached, updates its
   * `thumb_path` so the real thumbnail loads immediately (no re-fetch needed).
   * Uncached rows pick up `thumb_path` on their next hydration.
   */
  applyThumbnails(ready: { id: string; thumb_hash: string; thumb_path: string }[]): void {
    for (const r of ready) {
      const idx = this.#indexById.get(r.id);
      if (idx !== undefined) this.manifest[idx].thumb_hash = r.thumb_hash; // deep-reactive
      const heavy = this.heavy.get(r.id);
      if (heavy) {
        this.heavy.set(r.id, { ...heavy, thumb_hash: r.thumb_hash, thumb_path: r.thumb_path });
      }
    }
  }

  /** Hydrate heavy rows for the given ids (visible window + overscan). */
    async ensure(ids: string[]): Promise<void> {
      const missing = ids.filter((id) => !this.heavy.has(id) && !this.#pending.has(id));
      if (missing.length) {
        missing.forEach((id) => this.#pending.add(id));
        try {
          const rows = await invoke<AssetMetadata[]>("fetch_assets_by_ids", { ids: missing });
          for (const row of rows) this.heavy.set(row.id, row);
        } catch (e) {
          console.error("Asset hydration failed:", e);
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
