import { settings } from "../routes/settings.svelte";

/**
 * ═══════════════════════════════════════════════════════════════════════════
 * APP SHELL STATE — the three-pane layout
 * ═══════════════════════════════════════════════════════════════════════════
 *
 *   ┌────────────┬──────────────────────────┬─────────────┬────────┐
 *   │ sidebar    │ grid                     │ inspector   │ ─ ▢ ✕ │  ← window
 *   │  header    │  header                  │  header     │        │    controls
 *   ├────────────┼──────────────────────────┼─────────────┴────────┤    (fixed
 *   │  tree      │  assets                  │  metadata            │     overlay)
 *   └────────────┴──────────────────────────┴──────────────────────┘
 *
 * All three headers share ONE height (`--chrome-h`) so they align pixel-perfect
 * and read as a single strip. There is no real header element — the window drag
 * region is the empty space inside each pane header.
 *
 * ── The sidebar has three modes, driven by TWO independent axes ─────────────
 *
 *   hidden (toggle button / Ctrl+B)  ×  width (resize handle)
 *
 *   hidden = true                  → "hidden"    (column collapses to 0)
 *   hidden = false, width <= RAIL  → "rail"      (section icons only)
 *   hidden = false, width >  RAIL  → "expanded"  (full tree)
 *
 * The rail is NOT a third stop on the toggle button — you get there by dragging
 * the resize handle below SIDEBAR_SNAP, like VS Code and Finder. A tri-state
 * toggle would be unpredictable (the user can't tell what one click will do);
 * a width is continuous and self-explanatory.
 *
 * The inspector has no rail: an icon-only inspector shows nothing useful, so it
 * simply clamps at INSPECTOR_MIN and is hidden with its own toggle.
 */

/** Height of every pane header. Mirrors `--chrome-h` in layout.css. */
export const CHROME_H = 44;

/** Width of the collapsed sidebar (section icons). Mirrors `--rail-w`. */
export const RAIL_W = 52;

const SIDEBAR_MIN = 180;
const SIDEBAR_MAX = 400;
/** Drag the sidebar narrower than this and it snaps to the rail. */
const SIDEBAR_SNAP = 140;
/** Width the sidebar springs back to when you expand out of the rail. */
const SIDEBAR_RESTORE = 240;

const INSPECTOR_MIN = 200;
const INSPECTOR_MAX = 480;

export type SidebarMode = "expanded" | "rail" | "hidden";

class LayoutState {
    // Live geometry. Seeded with the defaults, then overwritten by `hydrate()`
    // once the persisted preferences have resolved from disk. Kept separate from
    // `settings.preferences` because a resize drag writes these ~60×/second and
    // every write to preferences hits the disk.
    sidebarWidth = $state(SIDEBAR_RESTORE);
    inspectorWidth = $state(320);
    sidebarHidden = $state(false);
    inspectorHidden = $state(false);

    /**
     * Is the filter bar showing?
     *
     * It has to be togglable rather than "visible when filters are active": the
     * bar IS the controls, so gating it on `hasFilters` is circular — you could
     * never set the first filter. It's force-shown whenever a filter IS active
     * (see AssetGrid), so a narrowed view always shows what's narrowing it and
     * the way to clear it.
     *
     * Session-only, deliberately: it mirrors the filters, and those never
     * survive a restart either.
     */
    filterBarOpen = $state(false);

    /** True while a resize handle is being dragged — kills the column transition. */
    resizing = $state(false);

    get sidebarMode(): SidebarMode {
        if (this.sidebarHidden) return "hidden";
        return this.sidebarWidth <= RAIL_W ? "rail" : "expanded";
    }

    /** Grid column widths, fed straight into `--sidebar-col` / `--inspector-col`. */
    get sidebarCol(): number {
        return this.sidebarHidden ? 0 : this.sidebarWidth;
    }
    get inspectorCol(): number {
        return this.inspectorHidden ? 0 : this.inspectorWidth;
    }

    /** Called once from the shell, after the settings store has read the disk. */
    async hydrate(): Promise<void> {
        await settings.ready;
        this.sidebarWidth = settings.preferences.sidebarWidth;
        this.inspectorWidth = settings.preferences.inspectorWidth;
        this.sidebarHidden = settings.preferences.sidebarHidden;
        this.inspectorHidden = settings.preferences.inspectorHidden;
    }

    /**
     * Live resize. `px` is the desired width; below SIDEBAR_SNAP it collapses to
     * the rail rather than being clamped at the minimum — so a single confident
     * drag to the left gets you there without a second gesture.
     */
    resizeSidebar(px: number): void {
        this.sidebarWidth =
            px < SIDEBAR_SNAP ? RAIL_W : Math.min(SIDEBAR_MAX, Math.max(SIDEBAR_MIN, px));
    }

    resizeInspector(px: number): void {
        this.inspectorWidth = Math.min(INSPECTOR_MAX, Math.max(INSPECTOR_MIN, px));
    }

    /** Persist geometry. Call on pointer-up, never during a drag. */
    persist(): void {
        void settings.set("sidebarWidth", this.sidebarWidth);
        void settings.set("inspectorWidth", this.inspectorWidth);
        void settings.set("sidebarHidden", this.sidebarHidden);
        void settings.set("inspectorHidden", this.inspectorHidden);
    }

    toggleSidebar(): void {
        this.sidebarHidden = !this.sidebarHidden;
        this.persist();
    }

    toggleInspector(): void {
        this.inspectorHidden = !this.inspectorHidden;
        this.persist();
    }

    /**
     * Give the sidebar its width back — the click half of "hover looks, click
     * stays". The expanded sidebar shows every section at once, so there's no
     * section to select: clicking any rail icon means the same thing.
     */
    expand(): void {
        this.sidebarHidden = false;
        if (this.sidebarWidth <= RAIL_W) this.sidebarWidth = SIDEBAR_RESTORE;
        this.persist();
    }
}

export const layout = new LayoutState();
