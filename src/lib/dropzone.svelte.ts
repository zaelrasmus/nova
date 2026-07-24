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
import { resolveDropTarget, type DropTarget } from "./droptarget";

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
   * Native drag position -> drop target.
   *
   * The division is the whole trick. Tauri reports a PhysicalPosition; the DOM
   * works in CSS pixels. They differ by `devicePixelRatio` on any scaled display
   * — measured in the D0 spike at 125%, where the raw coordinates missed every
   * target and the divided ones hit exactly.
   */
  #resolve(physicalX: number, physicalY: number): DropTarget | null {
    const dpr = window.devicePixelRatio || 1;
    return resolveDropTarget(physicalX / dpr, physicalY / dpr);
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
