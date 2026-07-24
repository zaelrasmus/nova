// What the user currently has selected.
//
// A peer of the asset store, NOT inspector state. Selection has several
// consumers and the inspector is only one of them: the grid paints it, the
// inspector reads and edits it, the keyboard writes it, and drag & drop will read
// it as its payload. If the inspector owned it, collapsing the panel would
// destroy the selection and the grid could not draw highlights.
//
// This module is deliberately dependency-free: it knows nothing about the
// manifest, the scope, or Tauri. Callers that need order (range selection,
// select-all) pass the ordered id list in. That keeps the interaction rules —
// the fiddly part — in one testable place.

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
 * model too. The folder TREE has its own multi-select for group delete (its own
 * `RangeSelection` instance); more than one selected there resolves to `none`
 * here, because there is nothing single to inspect.
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

/**
 * Click-to-select over an ordered list: plain click replaces, Ctrl toggles,
 * Shift extends from an anchor.
 *
 * Instantiated per surface — once for the asset grid, once for the folder tree —
 * because the RULES are identical while the contents are not. Order always comes
 * from the caller, so this never needs to know what it's selecting.
 */
export class RangeSelection {
  #ids = new SvelteSet<string>();

  /**
   * The item Shift extends *from*. Distinct from "the last item clicked":
   * repeated Shift+Clicks re-extend from this same anchor rather than chaining
   * off each other. Chaining is the classic wrong implementation and feels
   * broken the second time you shift-click.
   */
  #anchor = $state<string | null>(null);

  /**
   * Set when a press lands on an already-selected item with no modifier. The
   * collapse-to-one is deferred to release, so dragging a multi-selection keeps
   * all of it. See `pointerDown`.
   */
  #pendingCollapse: string | null = null;

  /** Selected ids in click order (NOT display order). */
  get ids(): string[] {
    return [...this.#ids];
  }

  get size(): number {
    return this.#ids.size;
  }

  get anchor(): string | null {
    return this.#anchor;
  }

  /** Membership test — the per-row check a list does while rendering. */
  has(id: string): boolean {
    return this.#ids.has(id);
  }

  clear(): void {
    this.#ids.clear();
    this.#anchor = null;
    this.#pendingCollapse = null;
  }

  /**
   * Pointer press on the item at `index`.
   *
   * Applies the change immediately — so a drag begins with the correct payload —
   * EXCEPT when the press lands on an already-selected item with no modifier.
   * That case is deferred to `click`, which is what makes "select five, then drag
   * them" work instead of collapsing to the one you grabbed.
   */
  pointerDown(orderedIds: readonly string[], index: number, mods: ClickModifiers): void {
    const id = orderedIds[index];
    if (id === undefined) return;

    if (!mods.ctrl && !mods.shift && this.#ids.has(id)) {
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
      this.replace([id], id);
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

  /** Select one item outright (keyboard activation, programmatic reveal). */
  selectOnly(id: string): void {
    this.replace([id], id);
  }

  /**
   * Select everything passed in — the caller supplies the *visible* list, so
   * Ctrl+A naturally means "all currently shown", never everything that exists.
   */
  selectAll(orderedIds: readonly string[]): void {
    this.replace(orderedIds, orderedIds[0] ?? null);
  }

  replace(ids: readonly string[], anchor: string | null): void {
    this.#ids.clear();
    for (const id of ids) this.#ids.add(id);
    this.#anchor = anchor;
    this.#pendingCollapse = null;
  }

  #apply(orderedIds: readonly string[], index: number, mods: ClickModifiers): void {
    const id = orderedIds[index];

    if (mods.shift && this.#anchor !== null) {
      // Ranges are computed over the ordered LIST, never the DOM: the grid is
      // virtualized, so a shift-click spanning 400 rows has no DOM to walk. It
      // also makes ranges respect the caller's current order for free.
      const from = orderedIds.indexOf(this.#anchor);
      if (from !== -1) {
        const [lo, hi] = from <= index ? [from, index] : [index, from];
        // Shift alone replaces; Ctrl+Shift adds the range to what's there.
        if (!mods.ctrl) this.#ids.clear();
        for (let i = lo; i <= hi; i++) this.#ids.add(orderedIds[i]);
        return; // anchor deliberately unchanged
      }
      // Anchor has fallen out of the current view; fall through and treat this
      // as a fresh click rather than selecting nothing.
    }

    if (mods.ctrl) {
      if (this.#ids.has(id)) this.#ids.delete(id);
      else this.#ids.add(id);
      this.#anchor = id;
      return;
    }

    this.replace([id], id);
  }
}

/**
 * The app-wide selection the inspector renders. Wraps a `RangeSelection` for
 * assets and adds the mutually-exclusive folder slot.
 */
class SelectionStore {
  readonly assets = new RangeSelection();
  #folder = $state<string | null>(null);

  /** The current selection, as the tagged union consumers should match on. */
  get current(): Selection {
    if (this.#folder !== null) return { kind: "folder", id: this.#folder };
    if (this.assets.size > 0) {
      return { kind: "assets", ids: this.assets.ids, anchor: this.assets.anchor };
    }
    return { kind: "none" };
  }

  get assetIds(): string[] {
    return this.assets.ids;
  }

  get assetCount(): number {
    return this.assets.size;
  }

  /** True when `id` is a selected asset — the grid's per-card check. */
  has(id: string): boolean {
    return this.assets.has(id);
  }

  clear(): void {
    this.clearAssets();
    this.#folder = null;
  }

  /**
   * Drop the asset selection but keep any selected folder.
   *
   * The invariant a manifest reload enforces is "selected ASSETS ⊆ what's on
   * screen" — a folder selection isn't a claim about the manifest's contents, so
   * reloading must not touch it. Clearing both made clicking a folder flash the
   * inspector open and immediately shut, since the click selects the folder and
   * the scope change then reloaded.
   */
  clearAssets(): void {
    this.assets.clear();
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
    this.assets.clear();
    this.#folder = id;
  }

  // ── Asset conveniences (the grid's entry points) ──────────────────────────

  pointerDownAsset(orderedIds: readonly string[], index: number, mods: ClickModifiers): void {
    this.#folder = null;
    this.assets.pointerDown(orderedIds, index, mods);
  }

  clickAsset(id: string): void {
    this.assets.click(id);
  }

  selectOnlyAsset(id: string): void {
    this.#folder = null;
    this.assets.selectOnly(id);
  }

  selectAllAssets(orderedIds: readonly string[]): void {
    this.#folder = null;
    this.assets.selectAll(orderedIds);
  }
}

export const selection = new SelectionStore();
