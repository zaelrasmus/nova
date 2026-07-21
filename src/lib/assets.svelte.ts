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
