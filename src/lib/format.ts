// Display formatters. Pure, no reactivity — anything that turns stored data into
// something a human reads belongs here so the inspector, the grid and future
// panels all phrase it identically.

const SIZE_STEPS = ["B", "KB", "MB", "GB", "TB"] as const;

/**
 * Bytes -> "4.7 MB". Binary units (1024), matching the filter bar's SIZE_UNITS,
 * so a file shown as "4.7 MB" is actually caught by a "max 5 MB" filter.
 */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  const step = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), SIZE_STEPS.length - 1);
  const value = bytes / 1024 ** step;
  // Whole bytes never need decimals; above that, one is enough to be useful
  // without implying precision the number doesn't have.
  return `${step === 0 ? value : value.toFixed(1)} ${SIZE_STEPS[step]}`;
}

/**
 * Stored RFC 3339 instant -> local date and time. Every timestamp in the DB is
 * UTC (see `stamp()` in assets.rs); the user reads them in their own zone, which
 * is the same conversion the date filters do at the other end.
 */
export function formatTimestamp(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "—";
  return d.toLocaleString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

/** Greatest common divisor, for reducing a pixel size to a readable ratio. */
function gcd(a: number, b: number): number {
  while (b) [a, b] = [b, a % b];
  return a;
}

/**
 * 1920x1080 -> "16:9". Ratios reduce to huge numbers for odd crops (1913x1074),
 * so anything that doesn't land on a small, recognisable pair falls back to a
 * decimal — "1.78:1" reads; "1913:1074" doesn't.
 */
export function formatAspectRatio(width: number, height: number): string {
  if (width <= 0 || height <= 0) return "—";
  const d = gcd(width, height);
  const [w, h] = [width / d, height / d];
  if (w <= 50 && h <= 50) return `${w}:${h}`;
  return `${(width / height).toFixed(2)}:1`;
}
