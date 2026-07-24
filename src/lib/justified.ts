// Justified-rows layout — the Flickr / Google Photos / Eagle grid.
//
// Items are packed left-to-right IN ORDER into full-width rows that share a
// height, so aspect ratios are preserved with no cropping and no ragged column
// bottoms. The property that matters for Nova: because rows fill by index,
// visual reading order EQUALS index order, which is what lets manual drag-to-
// reorder land exactly where it's dropped. Waterfall (shortest-lane) can't
// promise that; this can.
//
// Pure and framework-free so the math is testable on its own and the grid can
// virtualize on top of it.

export interface JustifiedItem {
  /** Index into the source list. */
  index: number;
  /** Offset from the row's left edge, in px. */
  left: number;
  /** Rendered width, in px (height is the row's height). */
  width: number;
}

export interface JustifiedRow {
  top: number;
  height: number;
  items: JustifiedItem[];
}

export interface JustifiedLayout {
  rows: JustifiedRow[];
  totalHeight: number;
}

/** Keeps a pathological ratio (a 1px-tall sliver, a corrupt 0) from wrecking a row. */
const MIN_RATIO = 0.2;
const MAX_RATIO = 5;

/**
 * Lay `ratios` (each width/height) into justified rows.
 *
 * `targetHeight` is the height rows aim for BEFORE the fit-to-width scaling — it
 * sets roughly how big thumbnails are and how many land per row. A row is closed
 * once its items, at the target height, would overflow the width; it's then
 * scaled so they fill it exactly. The final short row is left at the target
 * height and left-aligned rather than stretched, so one leftover image doesn't
 * blow up to full width.
 */
export function computeJustified(
  ratios: number[],
  containerWidth: number,
  targetHeight: number,
  gap: number,
): JustifiedLayout {
  const rows: JustifiedRow[] = [];
  if (containerWidth <= 0 || ratios.length === 0) {
    return { rows, totalHeight: 0 };
  }

  let top = 0;
  let rowStart = 0; // index of the first item in the row being built
  let ratioSum = 0;

  const clamp = (r: number) =>
    Number.isFinite(r) && r > 0 ? Math.min(Math.max(r, MIN_RATIO), MAX_RATIO) : 1;

  const emit = (start: number, end: number, sum: number, height: number) => {
    const items: JustifiedItem[] = [];
    let left = 0;
    for (let i = start; i < end; i++) {
      const w = clamp(ratios[i]) * height;
      items.push({ index: i, left, width: w });
      left += w + gap;
    }
    rows.push({ top, height, items });
    top += height + gap;
  };

  for (let i = 0; i < ratios.length; i++) {
    ratioSum += clamp(ratios[i]);
    const count = i - rowStart + 1;
    const gaps = gap * (count - 1);
    // Width this row would occupy at the target height.
    const naturalWidth = ratioSum * targetHeight + gaps;

    if (naturalWidth >= containerWidth) {
      // Scale the row down so its items fill the width exactly.
      const height = (containerWidth - gaps) / ratioSum;
      emit(rowStart, i + 1, ratioSum, height);
      rowStart = i + 1;
      ratioSum = 0;
    }
  }

  // Trailing partial row: keep target height, left-aligned (don't upscale).
  if (rowStart < ratios.length) {
    emit(rowStart, ratios.length, ratioSum, targetHeight);
  }

  // `top` overshoots by one gap after the last row; trim it so the scroll height
  // is exact.
  return { rows, totalHeight: Math.max(0, top - gap) };
}

/**
 * The rows intersecting a scroll window `[scrollTop, scrollTop + viewport]`,
 * padded by `overscan` px each side. A binary search for the first visible row
 * keeps this O(log n + visible) rather than scanning the whole layout each
 * scroll tick.
 */
export function visibleRows(
  layout: JustifiedLayout,
  scrollTop: number,
  viewport: number,
  overscan: number,
): JustifiedRow[] {
  const { rows } = layout;
  if (rows.length === 0) return rows;

  const min = scrollTop - overscan;
  const max = scrollTop + viewport + overscan;

  // First row whose bottom is past the top of the window.
  let lo = 0;
  let hi = rows.length - 1;
  let first = rows.length;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (rows[mid].top + rows[mid].height >= min) {
      first = mid;
      hi = mid - 1;
    } else {
      lo = mid + 1;
    }
  }

  const out: JustifiedRow[] = [];
  for (let i = first; i < rows.length && rows[i].top <= max; i++) {
    out.push(rows[i]);
  }
  return out;
}
