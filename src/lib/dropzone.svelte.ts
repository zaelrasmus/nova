// Files dragged INTO the window from the OS.
//
// This rides Tauri's native drag-drop event rather than HTML5 `dragover`/`drop`,
// for two reasons measured in the D0 spike:
//   1. On Windows, `dragDropEnabled: true` blocks HTML5 drop events in the
//      webview entirely — the page never sees them.
//   2. The native event carries absolute FILE PATHS. HTML5 would hand us `File`
//      objects with no path, meaning gigabytes of image data streamed through
//      the webview only to be handed straight back to Rust.
//
// The cost is that the event has no DOM target: it reports a physical-pixel
// position and nothing else, so this module does its own hit-testing and paints
// its own hover state. Surfaces opt in by rendering a `data-drop-*` attribute;
// nothing has to register or unregister, which matters because the grid's rows
// mount and unmount constantly under virtualization.
//
// Deliberately dependency-free, like `selection.svelte.ts`: it knows how to find
// a target and what the cursor is over, and NOTHING about what a drop means. The
// caller supplies that via `attach`.

import { getCurrentWebview } from "@tauri-apps/api/webview";
import type { UnlistenFn } from "@tauri-apps/api/event";

/**
 * Where a drop would land.
 *
 * `library` is the neutral surface — the grid, the empty space — meaning "import
 * at the top level". It is NOT the same as no target: dragging over the sidebar's
 * gaps or outside the window is `null`, and a drop there does nothing.
 */
export type DropTarget =
  | { kind: "library" }
  | { kind: "folder"; id: string; name: string };

/** Marks a folder row. The name rides along so the overlay can name the target. */
export const DROP_FOLDER_ATTR = "data-drop-folder";
export const DROP_FOLDER_NAME_ATTR = "data-drop-folder-name";
/** Marks the neutral import surface. */
export const DROP_LIBRARY_ATTR = "data-drop-library";

/** What `attach` needs from the app to turn a drop into an import. */
export interface DropHandler {
  (paths: string[], target: DropTarget): Promise<void> | void;
}

class DropZone {
  /** True while an OS drag is over the window — drives the global affordance. */
  active = $state(false);

  /** What the cursor is over right now, or null over a non-target. */
  target = $state<DropTarget | null>(null);

  /**
   * How many paths are in flight. Known from `enter` and kept for the whole
   * drag, because `over` events carry NO paths — only `enter` and `drop` do.
   */
  count = $state(0);

  /** True when a folder row is the current target — the tree's per-row check. */
  isOverFolder(id: string): boolean {
    return this.target?.kind === "folder" && this.target.id === id;
  }

  /**
   * Resolve a native drag position to a drop target.
   *
   * The conversion is the whole trick. Tauri reports a PhysicalPosition; the DOM
   * works in CSS pixels. They differ by `devicePixelRatio` on any scaled display
   * — measured in the D0 spike at 125% scaling, where the raw coordinates missed
   * every target and the divided ones hit exactly. On a 100% display the two are
   * identical, which is why this bug hides so well.
   */
  #resolve(physicalX: number, physicalY: number): DropTarget | null {
    const dpr = window.devicePixelRatio || 1;
    const el = document.elementFromPoint(physicalX / dpr, physicalY / dpr);
    if (!el) return null;

    const folder = el.closest(`[${DROP_FOLDER_ATTR}]`);
    if (folder) {
      return {
        kind: "folder",
        id: folder.getAttribute(DROP_FOLDER_ATTR) ?? "",
        name: folder.getAttribute(DROP_FOLDER_NAME_ATTR) ?? "folder",
      };
    }

    return el.closest(`[${DROP_LIBRARY_ATTR}]`) ? { kind: "library" } : null;
  }

  #reset(): void {
    this.active = false;
    this.target = null;
    this.count = 0;
  }

  /**
   * Start listening. Returns an unlisten function; call it on teardown.
   *
   * `handler` receives the paths and the resolved target. A drop on a non-target
   * is swallowed here and never reaches it — dropping on a scrollbar or a menu
   * should do nothing, not silently import at the top level.
   */
  async attach(handler: DropHandler): Promise<UnlistenFn> {
    return getCurrentWebview().onDragDropEvent(async (event) => {
      const p = event.payload;

      if (p.type === "enter") {
        this.active = true;
        this.count = p.paths.length;
        this.target = this.#resolve(p.position.x, p.position.y);
      } else if (p.type === "over") {
        this.target = this.#resolve(p.position.x, p.position.y);
      } else if (p.type === "leave") {
        this.#reset();
      } else {
        // drop
        const target = this.#resolve(p.position.x, p.position.y);
        const paths = p.paths;
        this.#reset();
        if (!target || paths.length === 0) return;
        await handler(paths, target);
      }
    });
  }
}

export const dropzone = new DropZone();
