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

export type ManifestFilter =
  | { kind: "all" }
  | { kind: "folder"; id: string }
  | { kind: "uncategorized" };

export interface Folder {
  id: string;
  name: string;
  parent_id: string | null;
  position: number;
  order_by: string;
  is_ascending: boolean;
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

  /** The slice of the library currently shown (drives which folder is active). */
  manifestFilter = $state<ManifestFilter>({ kind: "all" });
  /** Flat folder list for the tree UI, refreshed on library switch + import. */
  folders = $state<Folder[]>([]);

  /**
   * Cache-buster for regenerated thumbnails. A rebuild reuses the same
   * `id.webp` path with new bytes, so the webview would serve the stale cached
   * image; bumping this after a rebuild is appended to thumbnail URLs (see
   * AssetCard) to force a refetch. No effect on freshly generated thumbnails.
   */
  thumbVersion = $state(0);

  /**
   * Background thumbnail-generation progress for the UI indicator; null when
   * idle. Only multi-chunk runs (Rebuild / large pending) are surfaced — on-view
   * batches are single-chunk and finish before a bar would help.
   */
  thumbProgress = $state<{ current: number; total: number } | null>(null);

  /** Heavy rows keyed by id, hydrated per visible window. Reactive. */
  heavy = new SvelteMap<string, AssetMetadata>();

  #pending = new Set<string>();
  #loadToken = 0;

  /** id -> manifest index, for O(1) row patching (rebuilt on each load). */
  #indexById = new Map<string, number>();
  /** Ids with an in-flight on-view thumbnail request, to avoid re-requesting. */
  #thumbRequested = new Set<string>();
  /**
   * On-view generation queue (front = highest priority). Each request pushes the
   * current window to the FRONT so what's on screen generates first, while windows
   * scrolled past stay queued behind it — keeping the pipeline full. Drained by
   * `#pumpThumbnails` with up to `#THUMB_CONCURRENCY` batches in flight, which
   * keeps the CPU busy across IPC/DB round-trips without letting a fast scroll
   * flood SQLite with concurrent writers.
   */
  #thumbQueue: string[] = [];
  #thumbInFlight = 0;
  #thumbMode = "auto";
  #thumbQuality = 80;
  #thumbProgressHideTimer: ReturnType<typeof setTimeout> | undefined;
  /** Max concurrent generation batches, and ids per batch. */
  static #THUMB_CONCURRENCY = 3;
  static #THUMB_BATCH = 32;

  /** (Re)load the full manifest for the active library. */
  async load(filter: ManifestFilter = this.manifestFilter): Promise<void> {
    this.manifestFilter = filter;
    const token = ++this.#loadToken;
    this.isLoading = true;
    this.error = null;
    this.manifest = [];
    this.heavy.clear();
    this.#pending.clear();
    this.#thumbRequested.clear();
    this.#thumbQueue = [];
    this.#indexById.clear();

    try {
      const channel = new Channel<AssetLightRow[]>();
      channel.onmessage = (chunk) => {
        if (token !== this.#loadToken) return; // a newer load superseded this one
        // Build the id->index map incrementally, in lock-step with the manifest.
        // (Channel messages can arrive AFTER the invoke promise resolves, so a
        // one-shot rebuild after `await` would run on an empty manifest.)
        const base = this.manifest.length;
        for (let i = 0; i < chunk.length; i++) this.#indexById.set(chunk[i].id, base + i);
        // Push in place (reactive under $state) — O(chunk), not the O(n) full-array
        // copy `slice()` did per chunk (which was O(n²) over a large library).
        this.manifest.push(...chunk);
      };
      await invoke("stream_manifest", { filter, onChunk: channel });
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

  /** Switch the visible slice (folder / all / uncategorized) and reload. */
  setFilter(filter: ManifestFilter): Promise<void> {
    return this.load(filter);
  }

  /** Refresh the folder tree for the active library. Non-fatal on failure. */
  async loadFolders(): Promise<void> {
    try {
      this.folders = await invoke<Folder[]>("fetch_folders");
    } catch (e) {
      console.error("Failed to load folders:", e);
      this.folders = [];
    }
  }

  /** Create a folder (root when `parentId` is null) and refresh the tree. */
  async createFolder(name: string, parentId: string | null = null): Promise<void> {
    await invoke<Folder>("create_folder", { name, parentId });
    await this.loadFolders();
  }

  async renameFolder(id: string, name: string): Promise<void> {
    await invoke("rename_folder", { id, name });
    await this.loadFolders();
  }

  /**
   * Delete a folder (cascades to subfolders + memberships; assets are kept). If
   * the active view was the deleted folder or one of its now-gone descendants,
   * fall back to the full library.
   */
  async deleteFolder(id: string): Promise<void> {
    await invoke("delete_folder", { id });
    await this.loadFolders();
    const active = this.manifestFilter;
    if (active.kind === "folder" && !this.folders.some((f) => f.id === active.id)) {
      await this.setFilter({ kind: "all" });
    }
  }

  async moveFolder(id: string, newParentId: string | null): Promise<void> {
    await invoke("move_folder", { id, newParentId });
    await this.loadFolders();
  }

  /** Add assets to a folder; reload the manifest if the change affects the view. */
  async addAssetsToFolder(folderId: string, assetIds: string[]): Promise<void> {
    await invoke("add_assets_to_folder", { folderId, assetIds });
    const active = this.manifestFilter;
    if (active.kind === "uncategorized" || (active.kind === "folder" && active.id === folderId)) {
      await this.reload();
    }
  }

  async removeAssetsFromFolder(folderId: string, assetIds: string[]): Promise<void> {
    await invoke("remove_assets_from_folder", { folderId, assetIds });
    const active = this.manifestFilter;
    if (active.kind === "folder" && active.id === folderId) {
      await this.reload();
    }
  }

  /**
   * Rebuild every thumbnail in the active library with `mode` (clears the cache,
   * then regenerates). Resolves with the count once done; rows are patched in
   * place by the `thumbnail-progress` listener as batches complete, so no reload
   * is needed. Regenerated files keep the same `id.webp` path, so `thumbVersion`
   * is bumped afterward to bust the webview's image cache (see AssetCard).
   */
  async rebuildThumbnails(mode: string, quality: number): Promise<number> {
    const count = await invoke<number>("rebuild_thumbnails", { settings: { mode, quality } });
    // Files were rewritten in place under the same id.webp paths — bump the
    // version so on-screen thumbnails refetch instead of showing the cache.
    this.thumbVersion++;
    return count;
  }

  /**
   * On-view thumbnail generation: request thumbnails for the given ids (the
   * caller passes the current visible window, images still missing one). The
   * window is pushed to the FRONT of the queue (so it generates first), then the
   * queue is drained by `#pumpThumbnails` with up to `#THUMB_CONCURRENCY` batches
   * in flight — full throughput without oversubscribing SQLite. Ids already queued
   * or in flight are skipped; the backend also filters `WHERE thumb_hash IS NULL`,
   * so this stays idempotent. `thumbnail-progress` patches rows in via
   * `applyThumbnails`. Non-fatal on failure.
   */
  ensureThumbnails(ids: string[], mode: string, quality: number): void {
    this.#thumbMode = mode;
    this.#thumbQuality = quality;
    // Current window to the front; dedup against what's already queued/in flight.
    const queued = new Set(this.#thumbQueue);
    const fresh = ids.filter((id) => !this.#thumbRequested.has(id) && !queued.has(id));
    if (fresh.length) this.#thumbQueue = [...fresh, ...this.#thumbQueue];
    this.#pumpThumbnails();
  }

  /** Drain the generation queue, keeping up to `#THUMB_CONCURRENCY` in flight. */
  #pumpThumbnails(): void {
    const MAX = AssetLibrary.#THUMB_CONCURRENCY;
    const SIZE = AssetLibrary.#THUMB_BATCH;
    while (this.#thumbInFlight < MAX && this.#thumbQueue.length > 0) {
      const token = this.#loadToken;
      const batch = this.#thumbQueue.splice(0, SIZE);
      batch.forEach((id) => this.#thumbRequested.add(id));
      this.#thumbInFlight++;
      invoke<number>("generate_thumbnails_for_ids", {
        ids: batch,
        settings: { mode: this.#thumbMode, quality: this.#thumbQuality },
      })
        .catch((e) => console.error("Thumbnail generation request failed:", e))
        .finally(() => {
          batch.forEach((id) => this.#thumbRequested.delete(id));
          this.#thumbInFlight--;
          // Keep draining, unless a library switch superseded this run.
          if (token === this.#loadToken) this.#pumpThumbnails();
        });
    }
  }

  /**
   * Patch freshly-generated thumbnails into their rows in place — O(batch), no
   * manifest reload (which would flash the grid). Sets `thumb_hash` so the
   * ThumbHash placeholder appears, and, if the heavy row is cached, updates its
   * `thumb_path` so the real thumbnail loads immediately (no re-fetch needed).
   * Uncached rows pick up `thumb_path` on their next hydration.
   */
  /**
   * Feed a `thumbnail-progress` event to the UI indicator. Ignores small
   * single-chunk (≤64) runs — those are on-view batches that finish instantly —
   * and auto-hides shortly after completion (a fresh batch cancels the hide).
   */
  reportThumbProgress(current: number, total: number): void {
    if (total <= 64) return; // on-view batch; not worth a progress bar
    clearTimeout(this.#thumbProgressHideTimer);
    this.thumbProgress = { current, total };
    if (current >= total) {
      this.#thumbProgressHideTimer = setTimeout(() => {
        this.thumbProgress = null;
      }, 800);
    }
  }

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
      const token = this.#loadToken;
      const missing = ids.filter((id) => !this.heavy.has(id) && !this.#pending.has(id));
      if (missing.length) {
        missing.forEach((id) => this.#pending.add(id));
        try {
          const rows = await invoke<AssetMetadata[]>("fetch_assets_by_ids", { ids: missing });
          // A library switch may have superseded this request; dropping the stale
          // rows avoids polluting the new library's cache (T1.2).
          if (token !== this.#loadToken) return;
          for (const row of rows) this.heavy.set(row.id, row);
        } catch (e) {
          console.error("Asset hydration failed:", e);
        } finally {
          missing.forEach((id) => this.#pending.delete(id));
        }
      }
      if (token === this.#loadToken) this.#evict(ids);
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
