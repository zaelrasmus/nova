// The asset viewer (lightbox) — QuickLook and Fullscreen.
//
// A peer store like `selection`/`dropzone`, but this one is inherently coupled
// to the manifest: it navigates the SAME ordered, filtered list the grid shows
// (`assetLibrary.displayed`), so it reads that directly rather than pretending
// to be data-agnostic.
//
// Source of truth is an INDEX into that list, not the selection — navigating
// must not silently destroy a multi-selection on every arrow press. The
// selection is instead SYNCED (debounced) from the current index so the
// Inspector stays live without an IPC storm while scrubbing.

import { assetLibrary, type AssetLightRow } from "./assets.svelte";
import { selection } from "./selection.svelte";

export type ViewerMode = "closed" | "quicklook" | "fullscreen";

/** How long to wait after the last navigation before syncing the selection. */
const SYNC_MS = 140;

class Viewer {
  mode = $state<ViewerMode>("closed");
  /** Index into `assetLibrary.displayed`. */
  index = $state(0);

  #syncTimer: ReturnType<typeof setTimeout> | undefined;

  get isOpen(): boolean {
    return this.mode !== "closed";
  }

  /** The list being browsed — the grid's current filtered/sorted view. */
  get list(): AssetLightRow[] {
    return assetLibrary.displayed;
  }

  get count(): number {
    return this.list.length;
  }

  /** The asset on screen, or null if the list is empty / index fell out. */
  get current(): AssetLightRow | null {
    return this.list[this.index] ?? null;
  }

  // ── Opening ────────────────────────────────────────────────────────────────

  /** Open at a list index. Ignores an out-of-range index rather than opening blank. */
  open(index: number, mode: Exclude<ViewerMode, "closed"> = "quicklook"): void {
    if (index < 0 || index >= this.list.length) return;
    this.index = index;
    this.mode = mode;
    this.#syncSelection(true);
  }

  openById(id: string, mode: Exclude<ViewerMode, "closed"> = "quicklook"): void {
    const i = this.list.findIndex((a) => a.id === id);
    if (i >= 0) this.open(i, mode);
  }

  /**
   * Open from the current selection — the keyboard entry point. Falls back to
   * the first item so Space/F always do something when there's a view to show.
   */
  openFromSelection(mode: Exclude<ViewerMode, "closed"> = "quicklook"): void {
    if (this.list.length === 0) return;
    const target = selection.assets.anchor ?? selection.assetIds[0] ?? this.list[0]?.id;
    const i = target ? this.list.findIndex((a) => a.id === target) : 0;
    this.open(i >= 0 ? i : 0, mode);
  }

  close(): void {
    this.mode = "closed";
    clearTimeout(this.#syncTimer);
  }

  // ── Navigation ───────────────────────────────────────────────────────────

  next(): void {
    this.#step(1);
  }

  prev(): void {
    this.#step(-1);
  }

  /** Clamped at the ends (no wrap) — a viewer that loops surprises people. */
  #step(delta: number): void {
    const n = this.list.length;
    if (n === 0) return;
    const next = Math.min(Math.max(this.index + delta, 0), n - 1);
    if (next === this.index) return;
    this.index = next;
    this.#syncSelection(false);
  }

  // ── Mode toggles ───────────────────────────────────────────────────────────

  /** Space: open QuickLook if closed, otherwise close. */
  toggleQuickLook(): void {
    if (this.isOpen) this.close();
    else this.openFromSelection("quicklook");
  }

  /** F: closed → open Fullscreen; quicklook ⇄ fullscreen. */
  toggleFullscreen(): void {
    if (!this.isOpen) this.openFromSelection("fullscreen");
    else this.mode = this.mode === "fullscreen" ? "quicklook" : "fullscreen";
  }

  // ── Selection sync ─────────────────────────────────────────────────────────

  /**
   * Reflect the current asset into the selection so the Inspector follows.
   * Debounced during scrubbing (`immediate=false`); instant on open so the
   * panel doesn't lag the first frame.
   */
  #syncSelection(immediate: boolean): void {
    clearTimeout(this.#syncTimer);
    const apply = () => {
      const id = this.current?.id;
      if (id) selection.selectOnlyAsset(id);
    };
    if (immediate) apply();
    else this.#syncTimer = setTimeout(apply, SYNC_MS);
  }
}

export const viewer = new Viewer();
