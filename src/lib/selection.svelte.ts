// What the user currently has selected.
//
// A peer of the asset store, NOT inspector state. Selection has several
// consumers and the inspector is only one of them: the grid paints it, the
// inspector reads and (later) edits it, the keyboard writes it, and drag & drop
// will read it as its payload. If the inspector owned it, collapsing the panel
// would destroy the selection and the grid could not draw highlights.
//
// This module is deliberately dependency-free: it knows nothing about the
// manifest, the scope, or Tauri. Callers that need order (range selection,
// select-all) pass the ordered id list in. That keeps the interaction rules —
// the fiddly part — in one place with nothing to mock.

import { SvelteSet } from "svelte/reactivity";

/**
 * Assets OR a folder, never both. Modelled as a union so the illegal state
 * (three assets *and* a folder, which no inspector layout can render) is
 * unrepresentable, and so the inspector's three modes are a pattern match
 * instead of a pile of booleans.
 *
 * `folder` is singular on purpose. Multi-folder selection has no unambiguous
 * aggregate — select a parent and its child and "total size" has two defensible
 * answers — and `Scope::Folder { id }` holds one id, so it would fork the query
 * model too. Group operations on folders belong to the tree, with their own
 * transient state, and resolve to `none` here.
 */
export type Selection =
  | { kind: "none" }
  | { kind: "assets"; ids: string[]; anchor: string | null }
  | { kind: "folder"; id: string };

/** Which modifiers were held. `ctrl` covers Cmd on macOS. */
export interface ClickModifiers {
  ctrl: boolean;
  shift: boolean;
}

class SelectionStore {
  /** Selected asset ids. A Set so the grid's per-card `has()` is O(1). */
  #assets = new SvelteSet<string>();

  /**
   * The item Shift extends *from*. Distinct from "the last item clicked":
   * repeated Shift+Clicks re-extend from this same anchor rather than chaining
   * off each other. Chaining is the classic wrong implementation and feels
   * broken the second time you shift-click.
   */
  #anchor = $state<string | null>(null);

  #folder = $state<string | null>(null);

  /**
   * Set when a press lands on an already-selected item with no modifier. The
   * collapse-to-one is deferred to release, so dragging a multi-selection keeps
   * all of it. See `pointerDown`.
   */
  #pendingCollapse: string | null = null;

  /** The current selection, as the tagged union consumers should match on. */
  get current(): Selection {
    if (this.#folder !== null) return { kind: "folder", id: this.#folder };
    if (this.#assets.size > 0) {
      return { kind: "assets", ids: [...this.#assets], anchor: this.#anchor };
    }
    return { kind: "none" };
  }

  /** Selected asset ids in click order (NOT manifest order). */
  get assetIds(): string[] {
    return [...this.#assets];
  }

  get assetCount(): number {
    return this.#assets.size;
  }

  /** True when `id` is a selected asset — the grid's per-card check. */
  has(id: string): boolean {
    return this.#assets.has(id);
  }

  clear(): void {
    this.#assets.clear();
    this.#anchor = null;
    this.#folder = null;
    this.#pendingCollapse = null;
  }

  /**
   * Select a folder. Clears any asset selection: navigating into a folder makes
   * it the inspected object, the same way it does in Finder.
   *
   * "All" and "Uncategorized" are scopes with no folder row behind them — no
   * name, no notes, no timestamp to show — so they must call `clear()`, not
   * this. They are places, not things.
   */
  selectFolder(id: string): void {
    this.#assets.clear();
    this.#anchor = null;
    this.#pendingCollapse = null;
    this.#folder = id;
  }

  /**
   * Pointer press on the asset at `index`.
   *
   * Applies the change immediately — so a drag begins with the correct payload —
   * EXCEPT when the press lands on an already-selected item with no modifier.
   * That case is deferred to `click`, which is what makes "select five, then
   * drag them" work instead of collapsing to the one you grabbed.
   */
  pointerDown(orderedIds: readonly string[], index: number, mods: ClickModifiers): void {
    const id = orderedIds[index];
    if (id === undefined) return;

    if (!mods.ctrl && !mods.shift && this.#assets.has(id)) {
      this.#pendingCollapse = id;
      return;
    }
    this.#pendingCollapse = null;
    this.#apply(orderedIds, index, mods);
  }

  /** Release without a drag: resolve whatever `pointerDown` deferred. */
  click(id: string): void {
    if (this.#pendingCollapse === id) {
      this.#pendingCollapse = null;
      this.#replace([id], id);
    }
    // Otherwise pointerDown already did the work.
  }

  /**
   * A drag started, so the press must NOT collapse the selection on release.
   * Nothing calls this yet; the DnD phase wires it to `onDragStart` and the
   * multi-item payload works with no further changes here.
   */
  cancelPendingCollapse(): void {
    this.#pendingCollapse = null;
  }

  /** Select one asset outright (keyboard activation, programmatic reveal). */
  selectOnly(id: string): void {
    this.#replace([id], id);
  }

  /**
   * Select everything passed in — the caller supplies the *visible* manifest, so
   * Ctrl+A naturally means "all currently shown", never all rows in the library.
   */
  selectAll(orderedIds: readonly string[]): void {
    this.#replace(orderedIds, orderedIds[0] ?? null);
  }

  #apply(orderedIds: readonly string[], index: number, mods: ClickModifiers): void {
    const id = orderedIds[index];

    if (mods.shift && this.#anchor !== null) {
      // Ranges are computed over MANIFEST indices, never the DOM: the grid is
      // virtualized, so a shift-click spanning 400 rows has no DOM to walk. This
      // also makes the range respect the active sort and filters for free.
      const from = orderedIds.indexOf(this.#anchor);
      if (from !== -1) {
        const [lo, hi] = from <= index ? [from, index] : [index, from];
        // Shift alone replaces; Ctrl+Shift adds the range to what's there.
        if (!mods.ctrl) this.#assets.clear();
        for (let i = lo; i <= hi; i++) this.#assets.add(orderedIds[i]);
        this.#folder = null;
        return; // anchor deliberately unchanged
      }
      // Anchor has fallen out of the current view; fall through and treat this
      // as a fresh click rather than selecting nothing.
    }

    if (mods.ctrl) {
      if (this.#assets.has(id)) this.#assets.delete(id);
      else this.#assets.add(id);
      this.#anchor = id;
      this.#folder = null;
      return;
    }

    this.#replace([id], id);
  }

  #replace(ids: readonly string[], anchor: string | null): void {
    this.#assets.clear();
    for (const id of ids) this.#assets.add(id);
    this.#anchor = anchor;
    this.#folder = null;
  }
}

export const selection = new SelectionStore();
