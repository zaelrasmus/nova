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
  file_size: number;
  imported_date: string;
  creation_date: string;
  modified_date: string;

  thumb_path: string; // "" => no thumbnail; fallback to dest_path
}

/** Which slice of the library is shown. A scope is a place, not a filter. */
export type ManifestScope =
  | { kind: "all" }
  | { kind: "folder"; id: string }
  | { kind: "uncategorized" };

export type OrderBy =
  | "imported_date"
  | "creation_date"
  | "modified_date"
  | "filename"
  | "file_size"
  | "resolution"
  | "manual";

export interface Sort {
  order_by: OrderBy;
  is_ascending: boolean;
}

/** Mirrors Rust's DEFAULT_SORT — newest first. */
export const DEFAULT_SORT: Sort = { order_by: "imported_date", is_ascending: false };

/** Sort dropdown options, in display order. */
export const ORDER_BY_LABELS: { value: OrderBy; label: string }[] = [
  { value: "imported_date", label: "Date added" },
  { value: "creation_date", label: "Date created" },
  { value: "modified_date", label: "Date modified" },
  { value: "filename", label: "Name" },
  { value: "file_size", label: "File size" },
  { value: "resolution", label: "Resolution" },
  { value: "manual", label: "Manual" },
];

/**
 * Types offered as a filter. Deliberately NOT the full AssetType union: import
 * drops `unknown` rows entirely, so offering it would be a filter that can only
 * ever return nothing.
 */
export type AssetTypeFilter = "image" | "video" | "audio";

/**
 * Shape of an asset. The broad variants overlap by design — an ultrawide image
 * is also horizontal, and 16:9 is both. See Shape in assets.rs.
 */
export type Shape =
  | { kind: "horizontal" }
  | { kind: "vertical" }
  | { kind: "square" }
  | { kind: "ultrawide" }
  | { kind: "panoramic_vertical" }
  | { kind: "ratio"; num: number; den: number; tolerance: number };

/** Ephemeral narrowing of the current scope. Dimensions AND together. */
export interface FilterSet {
  asset_types: AssetTypeFilter[];
  shape: Shape | null;
}

/** A fresh empty set — a function, so no two call sites share one object. */
export const emptyFilters = (): FilterSet => ({ asset_types: [], shape: null });

export const ASSET_TYPE_LABELS: { value: AssetTypeFilter; label: string }[] = [
  { value: "image", label: "Images" },
  { value: "video", label: "Videos" },
  { value: "audio", label: "Audio" },
];

/**
 * Ratio match tolerance, in ratio units: 0.02 on 16:9 (1.778) accepts
 * 1.758–1.798, so a 1918×1080 crop still counts as 16:9. Exact equality would
 * make the fixed presets miss almost everything real.
 */
export const RATIO_TOLERANCE = 0.02;

const ratio = (num: number, den: number): Shape => ({
  kind: "ratio",
  num,
  den,
  tolerance: RATIO_TOLERANCE,
});

/**
 * Shape dropdown entries. `key` is the <select> value — a flat string, because
 * a ratio can't be an option value on its own. Grouped for the UI.
 */
export const SHAPE_PRESETS: { key: string; label: string; group: string; shape: Shape }[] = [
  { key: "horizontal", label: "Horizontal", group: "General", shape: { kind: "horizontal" } },
  { key: "vertical", label: "Vertical", group: "General", shape: { kind: "vertical" } },
  { key: "square", label: "Square", group: "General", shape: { kind: "square" } },
  { key: "ultrawide", label: "Ultrawide", group: "Panoramic", shape: { kind: "ultrawide" } },
  {
    key: "panoramic_vertical",
    label: "Panoramic vertical",
    group: "Panoramic",
    shape: { kind: "panoramic_vertical" },
  },
  { key: "16:9", label: "16:9", group: "Fixed ratio", shape: ratio(16, 9) },
  { key: "9:16", label: "9:16", group: "Fixed ratio", shape: ratio(9, 16) },
  { key: "3:2", label: "3:2", group: "Fixed ratio", shape: ratio(3, 2) },
  { key: "2:3", label: "2:3", group: "Fixed ratio", shape: ratio(2, 3) },
  { key: "4:3", label: "4:3", group: "Fixed ratio", shape: ratio(4, 3) },
  { key: "3:4", label: "3:4", group: "Fixed ratio", shape: ratio(3, 4) },
];

/** Preset groups in display order, for <optgroup>. */
export const SHAPE_GROUPS = ["General", "Panoramic", "Fixed ratio"] as const;

/**
 * Active shape -> <select> key. A ratio that matches no preset is "custom", which
 * is what keeps the custom inputs open after a reload.
 */
export function shapeKey(shape: Shape | null): string {
  if (!shape) return "";
  if (shape.kind !== "ratio") return shape.kind;
  const preset = SHAPE_PRESETS.find(
    (p) => p.shape.kind === "ratio" && p.shape.num === shape.num && p.shape.den === shape.den,
  );
  return preset ? preset.key : "custom";
}

export interface Folder {
  id: string;
  name: string;
  parent_id: string | null;
  position: number;
  order_by: OrderBy;
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
  scope = $state<ManifestScope>({ kind: "all" });
  /** The sort the CURRENT manifest was built with — always what the query did. */
  sort = $state<Sort>({ ...DEFAULT_SORT });
  /**
   * Ephemeral narrowing of the current scope. Never persisted (see FilterSet in
   * assets.rs), but DOES survive switching folders within a session — filtering
   * to images and then browsing folders is a normal workflow. That persistence
   * is exactly why the UI must keep an always-visible clear affordance.
   */
  filters = $state<FilterSet>(emptyFilters());
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

  /** Ids with an in-flight heavy-row hydration request, to avoid re-requesting. */
  #pending = new Set<string>();
  /** Bumped per manifest load (scope/sort change) — guards manifest streaming. */
  #loadToken = 0;
  /**
   * Bumped ONLY on a library switch — guards the heavy-row and thumbnail caches.
   * Separate from #loadToken because a sort change reorders the same library:
   * cached rows stay valid, so re-sorting must not throw away hydration work.
   */
  #libraryToken = 0;

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

  /**
   * (Re)load the manifest for `scope` with `sort`. Both are stored so the UI can
   * always show what the current view actually did.
   *
   * The heavy-row cache is deliberately NOT cleared here: a scope or sort change
   * reorders or narrows the same library, and rows are keyed by id, so every
   * cached row stays valid. Only a library switch invalidates them —
   * `clearCaches()`.
   */
  async load(
    scope: ManifestScope = this.scope,
    sort: Sort = this.sort,
    filters: FilterSet = this.filters,
  ): Promise<void> {
    this.scope = scope;
    this.sort = sort;
    this.filters = filters;
    const token = ++this.#loadToken;
    this.isLoading = true;
    this.error = null;
    this.manifest = [];
    this.#indexById.clear();
    // Queued windows describe the OLD order, so drop them; the grid re-requests
    // its visible window as soon as the new manifest paints.
    this.#thumbQueue = [];

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
      await invoke("stream_manifest", { query: { scope, filters, sort }, onChunk: channel });
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
   * Drop every cached row. Valid ONLY on a library switch — asset ids are
   * library-scoped, so carrying them over would mix two libraries' data.
   * Bumping `#libraryToken` also invalidates in-flight hydration + thumbnail work.
   */
  clearCaches(): void {
    this.#libraryToken++;
    this.heavy.clear();
    this.#pending.clear();
    this.#thumbRequested.clear();
    this.#thumbQueue = [];
    // Session view state rather than a cache, but it resets with the library for
    // the same reason filters aren't persisted: landing in a fresh library that
    // silently shows a subset reads as "my import didn't work".
    this.filters = emptyFilters();
  }

  /**
   * Switch the visible slice. Reads that scope's persisted sort first, so the
   * sort control never lies about what the query did — one extra round trip, and
   * the alternative (render a guess, then correct it) flickers.
   */
  async setScope(scope: ManifestScope): Promise<void> {
    let sort: Sort;
    try {
      sort = await invoke<Sort>("fetch_sort", { scope });
    } catch (e) {
      console.error("Failed to read persisted sort; keeping the current one:", e);
      // Read AFTER the await on purpose: a synchronous `this.sort` read here would
      // become a dependency of any $effect that calls setScope, and load() writes
      // `sort` — that pair is an effect loop. Post-await reads aren't tracked.
      sort = this.sort;
    }
    await this.load(scope, sort);
  }

  /** Whether any filter dimension is active — drives the "Clear" affordance. */
  get hasFilters(): boolean {
    return this.filters.asset_types.length > 0 || this.filters.shape !== null;
  }

  /** Replace the whole filter set and reload. Filters are never persisted. */
  setFilters(filters: FilterSet): Promise<void> {
    return this.load(this.scope, this.sort, filters);
  }

  clearFilters(): Promise<void> {
    return this.setFilters(emptyFilters());
  }

  /** Add/remove one type. Types within the dimension OR together. */
  toggleAssetType(type: AssetTypeFilter): Promise<void> {
    const current = this.filters.asset_types;
    const asset_types = current.includes(type)
      ? current.filter((t) => t !== type)
      : [...current, type];
    return this.setFilters({ ...this.filters, asset_types });
  }

  /** Constrain by shape; `null` clears the dimension. */
  setShape(shape: Shape | null): Promise<void> {
    return this.setFilters({ ...this.filters, shape });
  }

  /** Change the sort for the CURRENT scope, persist it, and reload. */
  async setSort(sort: Sort): Promise<void> {
    const scope = this.scope;
    try {
      await invoke("set_sort", { scope, sort });
    } catch (e) {
      // Non-fatal: apply it for this session even if persistence failed.
      console.error("Failed to persist sort:", e);
    }
    await this.load(scope, sort);
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
    const active = this.scope;
    if (active.kind === "folder" && !this.folders.some((f) => f.id === active.id)) {
      await this.setScope({ kind: "all" });
    }
  }

  async moveFolder(id: string, newParentId: string | null): Promise<void> {
    await invoke("move_folder", { id, newParentId });
    await this.loadFolders();
  }

  /** Add assets to a folder; reload the manifest if the change affects the view. */
  async addAssetsToFolder(folderId: string, assetIds: string[]): Promise<void> {
    await invoke("add_assets_to_folder", { folderId, assetIds });
    const active = this.scope;
    if (active.kind === "uncategorized" || (active.kind === "folder" && active.id === folderId)) {
      await this.reload();
    }
  }

  async removeAssetsFromFolder(folderId: string, assetIds: string[]): Promise<void> {
    await invoke("remove_assets_from_folder", { folderId, assetIds });
    const active = this.scope;
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
      // Guarded by the LIBRARY token: a sort change reorders the same assets, so
      // generation in flight is still wanted and must keep draining.
      const token = this.#libraryToken;
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
          if (token === this.#libraryToken) this.#pumpThumbnails();
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
      // Library token, not load token: heavy rows survive a scope/sort change.
      const token = this.#libraryToken;
      const missing = ids.filter((id) => !this.heavy.has(id) && !this.#pending.has(id));
      if (missing.length) {
        missing.forEach((id) => this.#pending.add(id));
        try {
          const rows = await invoke<AssetMetadata[]>("fetch_assets_by_ids", { ids: missing });
          // A library switch may have superseded this request; dropping the stale
          // rows avoids polluting the new library's cache (T1.2).
          if (token !== this.#libraryToken) return;
          for (const row of rows) this.heavy.set(row.id, row);
        } catch (e) {
          console.error("Asset hydration failed:", e);
        } finally {
          missing.forEach((id) => this.#pending.delete(id));
        }
      }
      if (token === this.#libraryToken) this.#evict(ids);
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
