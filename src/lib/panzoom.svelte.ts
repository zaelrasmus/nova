// Hand-rolled pan & zoom for the viewer — no dependency.
//
// The math is small: a `translate3d(tx, ty) scale(s)` applied to the image with
// transform-origin at its top-left. Zoom-to-cursor keeps the image point under
// the pointer fixed while scaling; pan is a plain translate with edge clamping.
//
// The transform is written to `element.style` IMPERATIVELY on every frame, NOT
// through reactive state — driving a Svelte-reactive transform per pointermove
// would churn the reactivity graph. Only the zoom PERCENT is mirrored into a
// rune, for the toolbar readout.

export interface PanZoomOptions {
  /** Hard zoom limits (fraction, so 0.1 = 10%, 32 = 3200%). */
  min?: number;
  max?: number;
}

export class PanZoom {
  /** Zoom percent, reactive — the ONLY thing the UI reads. */
  pct = $state(100);
  /** True while the view is at fit scale (drives the toolbar's Fit highlight). */
  fitted = $state(true);

  #container: HTMLElement;
  #image: HTMLImageElement;
  #min: number;
  #max: number;

  #scale = 1;
  #tx = 0;
  #ty = 0;
  #fitScale = 1;

  // Pan drag state.
  #panning = false;
  #startX = 0; // pointer origin minus current translate (pan math)
  #startY = 0;
  #downX = 0; // raw pointer-down position (drag-vs-click threshold)
  #downY = 0;
  #moved = false;
  #pointerId: number | null = null;

  #cleanup: Array<() => void> = [];

  constructor(container: HTMLElement, image: HTMLImageElement, opts: PanZoomOptions = {}) {
    this.#container = container;
    this.#image = image;
    this.#min = opts.min ?? 0.1;
    this.#max = opts.max ?? 32;

    image.style.transformOrigin = "0 0";
    image.style.position = "absolute";
    image.style.top = "0";
    image.style.left = "0";
    image.style.willChange = "transform";
    // Tailwind Preflight sets `img { max-width: 100%; height: auto }` globally,
    // which would CAP our explicit natural width to the container and break the
    // zoom math (the element renders smaller than the scale assumes). Opt out so
    // the width/height we set below take full effect.
    image.style.maxWidth = "none";
    image.style.maxHeight = "none";

    const onLoad = () => this.fit();
    image.addEventListener("load", onLoad);
    if (image.complete && image.naturalWidth) this.fit();

    const onWheel = (e: WheelEvent) => this.#onWheel(e);
    const onDown = (e: PointerEvent) => this.#onDown(e);
    const onMove = (e: PointerEvent) => this.#onMove(e);
    const onUp = (e: PointerEvent) => this.#onUp(e);
    // Wheel can't be passive — zoom must preventDefault to stop page scroll.
    container.addEventListener("wheel", onWheel, { passive: false });
    container.addEventListener("pointerdown", onDown);
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);

    // The stage resizes when toggling QuickLook ⇄ Fullscreen (and on window
    // resize). Refit if the user was at fit; otherwise just re-clamp the pan so
    // the image can't end up stranded off-screen.
    const ro = new ResizeObserver(() => {
      if (this.fitted) this.fit();
      else this.#apply();
    });
    ro.observe(container);

    this.#cleanup.push(
      () => image.removeEventListener("load", onLoad),
      () => container.removeEventListener("wheel", onWheel),
      () => container.removeEventListener("pointerdown", onDown),
      () => window.removeEventListener("pointermove", onMove),
      () => window.removeEventListener("pointerup", onUp),
      () => ro.disconnect(),
    );
  }

  destroy(): void {
    for (const fn of this.#cleanup) fn();
    this.#cleanup = [];
  }

  /** True if the press became a drag — lets the viewer suppress backdrop-close. */
  get didDrag(): boolean {
    return this.#moved;
  }

  /**
   * Convert a point in CONTAINER coordinates to the natural image pixel under
   * it, or null if that point is off the image. The eyedropper's bridge from
   * cursor to pixel — inverts the same translate+scale the image is drawn with.
   */
  toImagePixel(cx: number, cy: number): { x: number; y: number } | null {
    const x = Math.floor((cx - this.#tx) / this.#scale);
    const y = Math.floor((cy - this.#ty) / this.#scale);
    if (x < 0 || y < 0 || x >= this.#image.naturalWidth || y >= this.#image.naturalHeight) {
      return null;
    }
    return { x, y };
  }

  // ── Public controls (toolbar + keyboard) ─────────────────────────────────

  /** Fit the whole image in the viewport, centered. Never upscales past 100%. */
  fit(): void {
    const cw = this.#container.clientWidth;
    const ch = this.#container.clientHeight;
    const iw = this.#image.naturalWidth || 1;
    const ih = this.#image.naturalHeight || 1;
    this.#image.style.width = `${iw}px`;
    this.#image.style.height = `${ih}px`;

    this.#fitScale = Math.min(cw / iw, ch / ih, 1);
    this.#scale = this.#fitScale;
    this.#tx = (cw - iw * this.#scale) / 2;
    this.#ty = (ch - ih * this.#scale) / 2;
    this.#apply();
  }

  /** 1:1 — actual pixels — zoomed about the viewport center. */
  actualSize(): void {
    this.#zoomTo(1, this.#container.clientWidth / 2, this.#container.clientHeight / 2);
  }

  /** Step zoom about the viewport center (keyboard +/-). */
  zoomIn(): void {
    this.#zoomBy(1.25, this.#container.clientWidth / 2, this.#container.clientHeight / 2);
  }
  zoomOut(): void {
    this.#zoomBy(1 / 1.25, this.#container.clientWidth / 2, this.#container.clientHeight / 2);
  }

  // ── Internals ────────────────────────────────────────────────────────────

  /** Effective floor: honour the 10% limit, but never above the fit scale, so a
   *  very large image can always be zoomed out far enough to fit. */
  #floor(): number {
    return Math.min(this.#min, this.#fitScale);
  }

  #clampScale(s: number): number {
    return Math.min(Math.max(s, this.#floor()), this.#max);
  }

  /** Keep the image within the viewport: centre when smaller, edge-clamp when larger. */
  #clampPan(): void {
    const cw = this.#container.clientWidth;
    const ch = this.#container.clientHeight;
    const sw = (this.#image.naturalWidth || 1) * this.#scale;
    const sh = (this.#image.naturalHeight || 1) * this.#scale;

    this.#tx = sw <= cw ? (cw - sw) / 2 : Math.min(0, Math.max(cw - sw, this.#tx));
    this.#ty = sh <= ch ? (ch - sh) / 2 : Math.min(0, Math.max(ch - sh, this.#ty));
  }

  #apply(): void {
    this.#clampPan();
    this.#image.style.transform = `translate3d(${this.#tx}px, ${this.#ty}px, 0) scale(${this.#scale})`;
    this.pct = Math.round(this.#scale * 100);
    this.fitted = Math.abs(this.#scale - this.#fitScale) < 1e-3;
  }

  /** Zoom by `factor`, keeping the image point under (cx, cy) fixed. */
  #zoomBy(factor: number, cx: number, cy: number): void {
    this.#zoomTo(this.#clampScale(this.#scale * factor), cx, cy);
  }

  #zoomTo(next: number, cx: number, cy: number): void {
    next = this.#clampScale(next);
    if (next === this.#scale) return;
    // The image-local point currently under (cx, cy) — origin is top-left.
    const ix = (cx - this.#tx) / this.#scale;
    const iy = (cy - this.#ty) / this.#scale;
    this.#scale = next;
    // Solve for the translate that keeps that point under the cursor.
    this.#tx = cx - ix * next;
    this.#ty = cy - iy * next;
    this.#apply();
  }

  #onWheel(e: WheelEvent): void {
    e.preventDefault();
    const rect = this.#container.getBoundingClientRect();
    // Exponential so each notch is a consistent ratio; trackpads send small
    // deltas, wheels large — this feels the same on both.
    const factor = Math.exp(-e.deltaY * 0.0015);
    this.#zoomBy(factor, e.clientX - rect.left, e.clientY - rect.top);
  }

  #onDown(e: PointerEvent): void {
    if (e.button !== 0) return;
    this.#panning = true;
    this.#moved = false;
    this.#pointerId = e.pointerId;
    this.#downX = e.clientX;
    this.#downY = e.clientY;
    this.#startX = e.clientX - this.#tx;
    this.#startY = e.clientY - this.#ty;
    this.#image.style.cursor = "grabbing";
  }

  #onMove(e: PointerEvent): void {
    if (!this.#panning || e.pointerId !== this.#pointerId) return;
    // A press that travels more than a few px is a drag, not a click — used to
    // suppress the backdrop close-on-click.
    if (Math.hypot(e.clientX - this.#downX, e.clientY - this.#downY) > 3) this.#moved = true;
    this.#tx = e.clientX - this.#startX;
    this.#ty = e.clientY - this.#startY;
    this.#apply();
  }

  #onUp(e: PointerEvent): void {
    if (e.pointerId !== this.#pointerId) return;
    this.#panning = false;
    this.#pointerId = null;
    this.#image.style.cursor = "grab";
  }
}
