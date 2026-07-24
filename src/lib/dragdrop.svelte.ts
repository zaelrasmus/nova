// Dragging assets around INSIDE the app.
//
// Built on Pointer Events, not HTML5 drag and drop. The D0 spike measured that
// `dragDropEnabled: true` — which we need, because it's what delivers real file
// paths on an OS drop — blocks HTML5 drag in the webview on Windows: a drag
// starts and the drop never lands. HTML5 also has no touch support anywhere, so
// it was never an option for pen or touchscreen input.
//
// Pointer Events unify mouse, touch, and pen behind one code path and work
// identically on all three platforms, at the cost of writing the gesture
// ourselves. That cost is smaller than it looks: the drop targets, the
// hit-testing, and the app semantics all had to be written regardless — see
// `droptarget.ts`, which this shares with the OS-file path.
//
// Like `selection.svelte.ts`, this module is dependency-free. It knows how to
// run a drag; it knows nothing about assets, folders, or Tauri. What a drop
// MEANS is the caller's business, passed in through `onDrop`.

import { resolveDropTarget, type DropTarget } from "./droptarget";

/**
 * What a drag carries.
 *
 * `folder` is singular on purpose. A multi-folder reparent has no unambiguous
 * answer when the selection spans levels — drag a parent AND its child onto a
 * new home and the child ends up beside its former parent rather than inside
 * it, which nobody means. File managers resolve this by dropping any folder
 * whose ancestor is also selected; that's the fix if multi-drag is ever wanted,
 * and it belongs here rather than in the tree.
 */
export type DragPayload =
  | { kind: "assets"; ids: string[] }
  | { kind: "folder"; id: string; name: string };

/** Pixels a mouse must travel before a press becomes a drag. */
const MOUSE_THRESHOLD = 5;
/** Milliseconds a touch must stay still before it becomes a drag. */
const TOUCH_DELAY = 250;
/** Drift allowed during that wait; more than this is a scroll, not a drag. */
const TOUCH_TOLERANCE = 10;
/** Distance from a scroll container's edge that triggers auto-scroll. */
const EDGE = 56;
/** Auto-scroll speed at the very edge, in px per frame. */
const MAX_SCROLL = 20;

/** Marks a container that should auto-scroll while a drag hovers its edges. */
export const DRAG_SCROLL_ATTR = "data-drag-scroll";

class DragStore {
  /** What's being dragged, or null when nothing is. */
  payload = $state<DragPayload | null>(null);

  /** What the cursor is over right now, or null over a non-target. */
  target = $state<DropTarget | null>(null);

  /** Cursor position in CSS pixels — drives the floating preview. */
  x = $state(0);
  y = $state(0);

  /**
   * True while Shift is held: MOVE rather than add.
   *
   * Add is the unmodified default deliberately. Nova has no undo, so the plain
   * gesture — the one people trigger by accident — must be the reversible one.
   * Move removes a membership; add only creates one.
   */
  move = $state(false);

  get active(): boolean {
    return this.payload !== null;
  }

  get count(): number {
    return this.payload?.kind === "assets" ? this.payload.ids.length : 0;
  }

  /**
   * Set by the drag source when the current target is illegal — a folder
   * dropped into its own subtree. The drop is refused in Rust regardless; this
   * is what lets the cursor say so BEFORE the user commits.
   */
  forbidden = $state(false);

  /**
   * True when a drop here would land INSIDE this folder — the row-highlight
   * check.
   *
   * The edge zones split hairs only for a FOLDER being reordered: for it,
   * before/after mean "between siblings" and get an insertion line instead. An
   * asset or file drag has no such distinction — the whole row is "into" — so
   * for those every zone highlights.
   */
  isOverFolder(id: string): boolean {
    if (this.target?.kind !== "folder" || this.target.id !== id) return false;
    return this.payload?.kind === "folder" ? this.target.zone === "into" : true;
  }

  /** @internal — driven by the `draggable` action. */
  reset(): void {
    this.payload = null;
    this.target = null;
    this.move = false;
    this.forbidden = false;
  }
}

export const drag = new DragStore();

export interface DropContext {
  /** Shift was held on release: move rather than add. */
  move: boolean;
  /**
   * What was picked up, captured when the drag STARTED. Handed back rather than
   * re-read from the source, so a drop always acts on what the user grabbed even
   * if the underlying selection has since moved on.
   */
  payload: DragPayload;
  /** Release point in CSS pixels — for callers that hit-test the drop spot. */
  x: number;
  y: number;
}

export interface DraggableOptions {
  /**
   * The payload for a press at this event, or null to ignore the press
   * entirely. Called on pointerdown, so it sees the selection AFTER the click
   * that press produced — which is what makes "click a card, drag it" carry
   * that card.
   */
  payload: (e: PointerEvent) => DragPayload | null;
  /** The drag actually began (threshold passed), not merely a press. */
  onStart?: () => void;
  /**
   * Whether this target may be dropped on. Called every time the target
   * changes, so the answer can be shown while the drag is in flight rather than
   * only discovered on release. A forbidden target still paints its hover
   * state; only `onDrop` is withheld.
   */
  validate?: (target: DropTarget, payload: DragPayload) => boolean;
  /**
   * The drag was released (not cancelled, not vetoed). `target` is null when the
   * release missed every registered target — the source decides what that means,
   * which is what lets the grid treat "released in its own empty space" as a
   * reorder rather than a dead drop.
   */
  onDrop?: (target: DropTarget | null, ctx: DropContext) => void;
  /**
   * The pointer left the app window mid-drag, still held — hand off to a native
   * OS drag (drag OUT to Explorer/Photoshop/…). The internal drag has already
   * ended when this fires, so `onDrop` will NOT also run. Only reached for
   * payloads that make sense outside the app (assets, not folders); the source
   * decides by ignoring the ones it doesn't handle.
   */
  onDragOut?: (payload: DragPayload) => void;
}

/**
 * Make a container the source of drags for whatever is pressed inside it.
 *
 * Attached ONCE to a container rather than per item. That's not a shortcut: the
 * asset grid is virtualized, so per-item listeners would be added and removed on
 * every scroll tick, and an item can unmount mid-drag. `payload` decides from
 * the event whether the press counts, which keeps this immune to what happens
 * to be mounted.
 */
export function draggable(node: HTMLElement, options: DraggableOptions) {
  let opts = options;

  let pointerId: number | null = null;
  let startX = 0;
  let startY = 0;
  let started = false;
  let pending: DragPayload | null = null;
  let touchTimer: ReturnType<typeof setTimeout> | null = null;

  // Auto-scroll state, collected once per drag so the hover path stays cheap.
  let scrollers: HTMLElement[] = [];
  let raf = 0;

  /** Resolve what's under a point and ask the source whether it's allowed. */
  function setTarget(x: number, y: number) {
    const target = resolveDropTarget(x, y);
    drag.target = target;
    drag.forbidden =
      target !== null && pending !== null && opts.validate !== undefined
        ? !opts.validate(target, pending)
        : false;
  }

  function clearTouchTimer() {
    if (touchTimer !== null) {
      clearTimeout(touchTimer);
      touchTimer = null;
    }
  }

  function begin(e: PointerEvent) {
    if (!pending) return;
    started = true;
    drag.payload = pending;
    drag.move = e.shiftKey;
    drag.x = e.clientX;
    drag.y = e.clientY;
    setTarget(e.clientX, e.clientY);

    scrollers = [...document.querySelectorAll<HTMLElement>(`[${DRAG_SCROLL_ATTR}]`)];
    // A drag that selects text as it goes looks broken and leaves a highlight
    // behind on release.
    document.body.style.userSelect = "none";

    opts.onStart?.();
    raf = requestAnimationFrame(autoScroll);
  }

  /**
   * Scroll a container the cursor is hovering the edge of.
   *
   * Runs on its own frame loop rather than off pointermove: holding the cursor
   * still at the edge must keep scrolling, and a stationary pointer emits no
   * move events. Speed ramps with proximity so a nudge creeps and a deep hover
   * flies.
   */
  function autoScroll() {
    if (!started) return;
    for (const el of scrollers) {
      const r = el.getBoundingClientRect();
      if (drag.x < r.left || drag.x > r.right) continue;

      const fromTop = drag.y - r.top;
      const fromBottom = r.bottom - drag.y;
      let dy = 0;
      if (fromTop >= 0 && fromTop < EDGE) dy = -MAX_SCROLL * (1 - fromTop / EDGE);
      else if (fromBottom >= 0 && fromBottom < EDGE) dy = MAX_SCROLL * (1 - fromBottom / EDGE);

      if (dy !== 0) {
        el.scrollTop += dy;
        // The content moved under a stationary cursor, so what's beneath it has
        // changed even though the pointer hasn't.
        setTarget(drag.x, drag.y);
      }
    }
    raf = requestAnimationFrame(autoScroll);
  }

  function finish() {
    clearTouchTimer();
    cancelAnimationFrame(raf);
    if (started) document.body.style.userSelect = "";
    pointerId = null;
    pending = null;
    started = false;
    scrollers = [];
    drag.reset();
  }

  function onPointerDown(e: PointerEvent) {
    // Left button only for a mouse; touch and pen report button 0 too.
    if (e.pointerType === "mouse" && e.button !== 0) return;
    if (pointerId !== null) return; // a second finger during a drag

    const payload = opts.payload(e);
    if (!payload) return;

    pointerId = e.pointerId;
    startX = e.clientX;
    startY = e.clientY;
    pending = payload;
    started = false;

    window.addEventListener("pointermove", onPointerMove);
    window.addEventListener("pointerup", onPointerUp);
    window.addEventListener("pointercancel", onPointerCancel);
    window.addEventListener("keydown", onKeyDown);

    // Touch starts on a long press, not on movement: a finger swiping across
    // the grid means scroll, and stealing that would make the grid unusable.
    if (e.pointerType === "touch") {
      touchTimer = setTimeout(() => begin(e), TOUCH_DELAY);
    }
  }

  /**
   * The pointer crossed OUTSIDE the app window while dragging — hand the gesture
   * to the OS as a drag-out. Detected as true out-of-bounds coordinates, NOT as
   * "near an edge": the sidebar and inspector live at the edges, and dragging
   * onto them must stay internal. A held-button drag keeps delivering moves with
   * out-of-range coordinates (the OS captures to the window), so this fires the
   * instant the cursor leaves the glass.
   */
  function tryHandoff(e: PointerEvent): boolean {
    if (!started || !pending || pending.kind !== "assets" || !opts.onDragOut) return false;
    const outside =
      e.clientX < 0 ||
      e.clientY < 0 ||
      e.clientX > window.innerWidth ||
      e.clientY > window.innerHeight;
    if (!outside) return false;

    const payload = pending;
    detach();
    finish(); // ends the internal drag so its pointerup can't also fire onDrop
    opts.onDragOut(payload);
    return true;
  }

  function onPointerMove(e: PointerEvent) {
    if (e.pointerId !== pointerId) return;
    if (tryHandoff(e)) return;

    if (!started) {
      const dx = Math.abs(e.clientX - startX);
      const dy = Math.abs(e.clientY - startY);
      if (touchTimer !== null) {
        // Moved before the long press completed — that was a scroll.
        if (dx > TOUCH_TOLERANCE || dy > TOUCH_TOLERANCE) finish();
        return;
      }
      if (dx < MOUSE_THRESHOLD && dy < MOUSE_THRESHOLD) return;
      begin(e);
    }

    drag.x = e.clientX;
    drag.y = e.clientY;
    drag.move = e.shiftKey;
    setTarget(e.clientX, e.clientY);
  }

  function onPointerUp(e: PointerEvent) {
    if (e.pointerId !== pointerId) return;
    // A vetoed target is passed as null: the source treats it as a miss.
    const target = started && !drag.forbidden ? drag.target : null;
    const payload = drag.payload;
    const move = drag.move;
    const x = e.clientX;
    const y = e.clientY;
    const wasDrag = started;
    detach();
    finish();
    // Reset BEFORE the callback: it may reload the manifest, and leaving stale
    // drag state visible across that await paints a preview over the new rows.
    // Fire on any real drag (target may be null); a press that never crossed the
    // threshold isn't a drop.
    if (wasDrag && payload) opts.onDrop?.(target, { move, payload, x, y });
  }

  function onPointerCancel(e: PointerEvent) {
    if (e.pointerId !== pointerId) return;
    detach();
    finish();
  }

  /** Escape abandons a drag in flight, the way it does in every file manager. */
  function onKeyDown(e: KeyboardEvent) {
    if (e.key !== "Escape") return;
    detach();
    finish();
  }

  function detach() {
    window.removeEventListener("pointermove", onPointerMove);
    window.removeEventListener("pointerup", onPointerUp);
    window.removeEventListener("pointercancel", onPointerCancel);
    window.removeEventListener("keydown", onKeyDown);
  }

  node.addEventListener("pointerdown", onPointerDown);

  return {
    update(next: DraggableOptions) {
      opts = next;
    },
    destroy() {
      node.removeEventListener("pointerdown", onPointerDown);
      detach();
      finish();
    },
  };
}
