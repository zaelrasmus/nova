// Where a drag would land, and how to find it from a screen coordinate.
//
// Shared by BOTH drag systems, which is the point of this module existing:
//
//   * `dropzone.svelte.ts` — files dragged in from the OS. Tauri's native event
//     reports a coordinate and no DOM target, so hit-testing is the only option.
//   * `dragdrop.svelte.ts` — assets dragged inside the app. A pointer-captured
//     drag sends every move to the SOURCE element, so the element under the
//     cursor never sees an event either.
//
// Two mechanisms, same missing piece, so they share one resolver and one set of
// attributes. A folder row marked once is a target for both, and the two can't
// drift apart into subtly different notions of "what am I over".
//
// Targets are declared with data attributes rather than a registry. Nothing has
// to register or unregister, which matters because the grid mounts and unmounts
// rows constantly under virtualization — a registry would need teardown on every
// scroll tick, and a stale entry is a drop into a folder that isn't there.

/** Marks a folder row. The name rides along so overlays can name the target. */
export const DROP_FOLDER_ATTR = "data-drop-folder";
export const DROP_FOLDER_NAME_ATTR = "data-drop-folder-name";
/** Marks the neutral import surface (the grid, its header, its empty states). */
export const DROP_LIBRARY_ATTR = "data-drop-library";

/**
 * Where a drag would land.
 *
 * `library` is the neutral surface, meaning "the library at large". It is NOT
 * the same as `null`: the sidebar's empty space and everything outside the
 * window resolve to `null`, and a drop there does nothing. An ambiguous target
 * doing nothing beats guessing at a destination.
 */
export type DropTarget =
  | { kind: "library" }
  | { kind: "folder"; id: string; name: string; zone: DropZone };

/**
 * Where within a folder row the cursor sits.
 *
 * Only tree reorganisation cares: dragging a FOLDER onto the middle of a row
 * reparents, onto an edge inserts between siblings, and those must look
 * different or tree drag & drop becomes a guessing game. Dragging assets or
 * files ignores this and treats every row as `into` — "file this between two
 * folders" has no meaning.
 *
 * Computed here rather than by each consumer so one definition of "the edge"
 * serves the indicator, the drop, and any future target.
 */
export type DropZone = "before" | "into" | "after";

/**
 * Height of the insert band at a row's top and bottom.
 *
 * A proportion alone makes tall rows absurdly easy and short rows impossible to
 * hit; a fixed size ignores the row entirely. Capping a proportion keeps the
 * middle — the reparent target, which is the more consequential action — the
 * dominant part of every row.
 */
const EDGE_BAND = 8;

/**
 * Resolve a point, in CSS pixels relative to the viewport, to a drop target.
 *
 * Callers with physical pixels (Tauri's native event) must divide by
 * `devicePixelRatio` first — the two only coincide at 100% display scaling,
 * which is exactly why that bug hides so well.
 *
 * Note `elementFromPoint` skips elements with `pointer-events: none`. Drag
 * overlays rely on that to stay out of their own way, so any element drawn over
 * a target during a drag MUST set it.
 */
export function resolveDropTarget(cssX: number, cssY: number): DropTarget | null {
  const el = document.elementFromPoint(cssX, cssY);
  if (!el) return null;

  const folder = el.closest(`[${DROP_FOLDER_ATTR}]`);
  if (folder) {
    const rect = folder.getBoundingClientRect();
    const band = Math.min(EDGE_BAND, rect.height * 0.3);
    const zone: DropZone =
      cssY < rect.top + band ? "before" : cssY > rect.bottom - band ? "after" : "into";
    return {
      kind: "folder",
      id: folder.getAttribute(DROP_FOLDER_ATTR) ?? "",
      name: folder.getAttribute(DROP_FOLDER_NAME_ATTR) ?? "folder",
      zone,
    };
  }

  return el.closest(`[${DROP_LIBRARY_ATTR}]`) ? { kind: "library" } : null;
}
