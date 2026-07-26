// Streams the manifest via Channel. Caches Heavy rows by id with batched loading + eviction.

import { invoke, Channel } from "@tauri-apps/api/core";
import { SvelteMap } from "svelte/reactivity";
import { thumbHashToDataURL } from "thumbhash";
import { selection } from "./selection.svelte";
import { fromRuleTree, isActive, toRuleTree, type RuleNode } from "./rules";
import type {
  ActionRun,
  QuickAction,
  QuickActionDraft,
  RenamePreview,
  RunPreview,
  RunSummary,
  Step,
  UndoSummary,
} from "./actions";

export interface AssetLightRow {
  id: string;
  width: number;
  height: number;
  asset_type: "image" | "audio" | "video" | "unknown";
  thumb_hash: string | null;
  is_animated: boolean;
  /** Display name — carried in the light row so name search filters instantly
   *  in the frontend, no backend round trip (the common half of the hybrid). */
  filename: string;
}

export interface AssetMetadata extends AssetLightRow {
  extension: string;
  dest_path: string;
  file_size: number;
  imported_date: string;
  creation_date: string;
  modified_date: string;

  /** User-authored. Never derived from the file. */
  notes: string | null;
  /** Where the asset came from, if the user recorded it. */
  source_url: string | null;

  thumb_path: string; // "" => no thumbnail; fallback to dest_path
}

/**
 * Partial update. An omitted key leaves that column alone; `""` clears it. The
 * inspector sends one field at a time as it's edited, so the difference matters.
 */
export interface AssetPatch {
  /**
   * The name WITHOUT its extension. Rust recomposes `{stem}.{extension}` from the
   * row's own extension, so the label and the format can never drift apart.
   *
   * Renaming mutates the database ONLY — the file stays `UUID.ext` on disk, which
   * is what makes it instant, reversible, and free of the failure modes a real
   * filesystem rename carries.
   */
  stem?: string;
  notes?: string;
  source_url?: string;
}

export interface FolderPatch {
  name?: string;
  notes?: string;
  /** Pin accent token. `""` clears it, like every other free-text field here. */
  color?: string;
}

/** "photo.jpg" -> "photo". The extension is shown separately and isn't editable. */
export function filenameStem(filename: string, extension: string): string {
  if (!extension) return filename;
  const suffix = `.${extension}`;
  return filename.toLowerCase().endsWith(suffix.toLowerCase())
    ? filename.slice(0, -suffix.length)
    : filename;
}

/** Which slice of the library is shown. A scope is a place, not a filter. */
export type ManifestScope =
  | { kind: "all" }
  | { kind: "folder"; id: string }
  | { kind: "uncategorized" }
  /** A smart folder: a place whose membership is a query. See lib/rules.ts. */
  | { kind: "smart"; id: string }
  /** A group of smart folders, browsed as the union of its members. */
  | { kind: "smart_group"; id: string };

/** Do two scopes name the same place? Scopes are rebuilt per click, so `===` won't do. */
export const sameScope = (a: ManifestScope, b: ManifestScope): boolean =>
  a.kind === b.kind &&
  (!("id" in a) || a.id === (b as { id: string }).id);

export type OrderBy =
  | "imported_date"
  | "creation_date"
  | "modified_date"
  | "filename"
  | "file_size"
  | "resolution"
  | "manual"
  | "added_date";

export interface Sort {
  order_by: OrderBy;
  is_ascending: boolean;
}

/** Mirrors Rust's DEFAULT_SORT — newest first. */
export const DEFAULT_SORT: Sort = { order_by: "imported_date", is_ascending: false };

/**
 * Sort dropdown options, in display order.
 *
 * `imported_date` is "Date imported", NOT "Date added" — those became two
 * different questions the moment folders could answer the second one precisely.
 * Imported = when it entered the library; Added = when it was filed into THIS
 * folder, which can be months later.
 *
 * `folderOnly` options are hidden outside a folder scope. They'd still work
 * (both scope-relative sorts fall back to an asset-level column), but offering
 * "Date added to folder" while browsing All assets promises a precision the
 * fallback doesn't have.
 */
export const ORDER_BY_LABELS: { value: OrderBy; label: string; folderOnly?: true }[] = [
  { value: "imported_date", label: "Date imported" },
  { value: "added_date", label: "Date added to folder", folderOnly: true },
  { value: "creation_date", label: "Date created" },
  { value: "modified_date", label: "Date modified" },
  { value: "filename", label: "Name" },
  { value: "file_size", label: "File size" },
  { value: "resolution", label: "Resolution" },
  { value: "manual", label: "Manual" },
];

/**
 * Types offered as a filter. Deliberately NOT the full AssetType union: import
 * drops `unknown` rows entirely, so offering it would be a filter that can only
 * ever return nothing.
 */
export type AssetTypeFilter = "image" | "video" | "audio";

/**
 * Shape of an asset. The broad variants overlap by design — an ultrawide image
 * is also horizontal, and 16:9 is both. See Shape in assets.rs.
 */
export type Shape =
  | { kind: "horizontal" }
  | { kind: "vertical" }
  | { kind: "square" }
  | { kind: "ultrawide" }
  | { kind: "panoramic_vertical" }
  | { kind: "ratio"; num: number; den: number; tolerance: number };

/** Which date column a date range applies to. Mirrors OrderBy's naming. */
export type DateField = "imported_date" | "creation_date" | "modified_date";

/**
 * Half-open instant range over one date column: `[from, until)`. Both bounds are
 * absolute UTC timestamps, produced from LOCAL calendar days by the helpers
 * below — the client is the only place that knows the user's timezone.
 */
export interface DateFilter {
  field: DateField;
  from: string | null;
  until: string | null;
}

/** Inclusive byte range; either end may be null. */
export interface SizeRange {
  min: number | null;
  max: number | null;
}

/**
 * Match assets containing a color close to (r, g, b). Tests every palette entry,
 * not just the most dominant — see ColorFilter in assets.rs.
 */
export interface ColorFilter {
  r: number;
  g: number;
  b: number;
  /** Max perceptual distance (ΔE) still counted a match. Inverse of "Accuracy". */
  tolerance: number;
  /** Min share of the image (0.0–1.0) the matching color must cover. */
  min_coverage: number;
}

/** How selected tags combine. `all` (AND) is the default and most common. */
export type TagMatchMode = "any" | "all" | "equals";

/**
 * Tag constraint. `include`/`exclude` hold tag IDS, not names, so a rename can't
 * change what a saved filter matches; a deleted tag's id simply matches nothing.
 * `untagged` matches assets with no tags — a pseudo-selection, never a real tag.
 */
export interface TagFilter {
  mode: TagMatchMode;
  include: string[];
  exclude: string[];
  untagged: boolean;
}

export const emptyTagFilter = (): TagFilter => ({
  mode: "all",
  include: [],
  exclude: [],
  untagged: false,
});

/** A tag filter that would narrow nothing — normalized back to `null`. */
export const isTagFilterEmpty = (t: TagFilter): boolean =>
  !t.untagged && t.include.length === 0 && t.exclude.length === 0;

/**
 * Which columns a text search looks in — the seven scope toggles. Field names
 * are camelCase to match the Rust `SearchScopes` (serde `rename_all`).
 */
export interface SearchScopes {
  name: boolean;
  extension: boolean;
  note: boolean;
  url: boolean;
  folderName: boolean;
  folderNote: boolean;
  tags: boolean;
}

/** Every scope on — the default a fresh search starts from. */
export const allScopes = (): SearchScopes => ({
  name: true,
  extension: true,
  note: true,
  url: true,
  folderName: true,
  folderNote: true,
  tags: true,
});

/** A live text search: query plus active scopes. Ephemeral — never saved. */
export interface TextSearch {
  query: string;
  scopes: SearchScopes;
}

/** Ephemeral narrowing of the current scope. Dimensions AND together. */
export interface FilterSet {
  asset_types: AssetTypeFilter[];
  shape: Shape | null;
  date: DateFilter | null;
  size: SizeRange | null;
  color: ColorFilter | null;
  tags: TagFilter | null;
  /** Live full-text search; `null` = none. Stripped before a filter is saved. */
  text: TextSearch | null;
}

/** A fresh empty set — a function, so no two call sites share one object. */
export const emptyFilters = (): FilterSet => ({
  asset_types: [],
  shape: null,
  date: null,
  size: null,
  color: null,
  tags: null,
  text: null,
});

/**
 * Accuracy slider (0–100) -> ΔE tolerance. Inverted: more accuracy means less
 * tolerance. The range is grounded in real colorimetry — ~2 ΔE is a
 * just-noticeable difference, ~10 a clearly different shade, ~40 a different
 * color family — so neither end of the slider is useless.
 */
export const TOLERANCE_MIN = 2;
export const TOLERANCE_MAX = 40;
export const accuracyToTolerance = (accuracy: number): number =>
  TOLERANCE_MAX - ((TOLERANCE_MAX - TOLERANCE_MIN) * accuracy) / 100;
export const toleranceToAccuracy = (tolerance: number): number =>
  ((TOLERANCE_MAX - tolerance) * 100) / (TOLERANCE_MAX - TOLERANCE_MIN);

/**
 * Fixed minimum coverage: a color must cover at least this share of an image to
 * count, so a handful of stray pixels doesn't make a photo "red".
 *
 * Exposed as a full filter dimension in the backend but pinned here, because one
 * knob (Accuracy) is what makes this feature approachable. Uncomment the coverage
 * control in FilterBar to drive it by hand while testing.
 */
export const DEFAULT_MIN_COVERAGE = 0.05;

/** Preset swatches, mirroring the picker's neutral + chromatic rows. */
export const COLOR_PRESETS = [
  "#000000",
  "#FFFFFF",
  "#9E9E9E",
  "#8D6E63",
  "#F48FB1",
  "#E53935",
  "#FB8C00",
  "#FDD835",
  "#43A047",
  "#00ACC1",
  "#1E88E5",
  "#8E24AA",
];

/** "#RRGGBB" -> channel triple. Returns null for anything malformed. */
export function hexToRgb(hex: string): { r: number; g: number; b: number } | null {
  const m = /^#?([0-9a-f]{6})$/i.exec(hex.trim());
  if (!m) return null;
  const n = parseInt(m[1], 16);
  return { r: (n >> 16) & 255, g: (n >> 8) & 255, b: n & 255 };
}

export const rgbToHex = (c: { r: number; g: number; b: number }): string =>
  "#" + [c.r, c.g, c.b].map((v) => v.toString(16).padStart(2, "0")).join("");

export const rgbToCss = (c: { r: number; g: number; b: number }): string =>
  `rgb(${c.r}, ${c.g}, ${c.b})`;

/** sRGB -> `hsl(H, S%, L%)`. Plain color-space math, no perceptual weighting. */
export function rgbToHsl(c: { r: number; g: number; b: number }): string {
  const [r, g, b] = [c.r / 255, c.g / 255, c.b / 255];
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const l = (max + min) / 2;
  const d = max - min;

  let h = 0;
  if (d !== 0) {
    if (max === r) h = ((g - b) / d) % 6;
    else if (max === g) h = (b - r) / d + 2;
    else h = (r - g) / d + 4;
    h *= 60;
    if (h < 0) h += 360;
  }
  const s = d === 0 ? 0 : d / (1 - Math.abs(2 * l - 1));
  return `hsl(${Math.round(h)}, ${Math.round(s * 100)}%, ${Math.round(l * 100)}%)`;
}

/** One color of an asset's palette, with the share of the image it covers. */
export interface PaletteSwatch {
  r: number;
  g: number;
  b: number;
  ratio: number;
}

/** Formats a swatch can be copied in. */
export const COLOR_FORMATS = ["HEX", "RGB", "HSL"] as const;
export type ColorFormat = (typeof COLOR_FORMATS)[number];

export function formatColor(c: PaletteSwatch, format: ColorFormat): string {
  if (format === "RGB") return rgbToCss(c);
  if (format === "HSL") return rgbToHsl(c);
  return rgbToHex(c).toUpperCase();
}

/** How much of the library has a color palette. */
export interface ColorCoverage {
  analyzed: number;
  total: number;
}

export const DATE_FIELD_LABELS: { value: DateField; label: string }[] = [
  { value: "imported_date", label: "Date added" },
  { value: "creation_date", label: "Date created" },
  { value: "modified_date", label: "Date modified" },
];

/** Size inputs carry a unit; the wire format is always bytes. */
export const SIZE_UNITS = [
  { value: "KB", bytes: 1024 },
  { value: "MB", bytes: 1024 * 1024 },
  { value: "GB", bytes: 1024 * 1024 * 1024 },
] as const;

export type SizeUnit = (typeof SIZE_UNITS)[number]["value"];

export const unitBytes = (unit: SizeUnit): number =>
  SIZE_UNITS.find((u) => u.value === unit)!.bytes;

/** Local midnight starting `offsetDays` from today (negative = in the past). */
function localMidnight(offsetDays: number): Date {
  const d = new Date();
  d.setHours(0, 0, 0, 0);
  d.setDate(d.getDate() + offsetDays);
  return d;
}

/**
 * `YYYY-MM-DD` (as shown in a date input, i.e. a LOCAL day) -> the instant that
 * local day begins. Parsing without a `Z` is what makes JS treat it as local.
 */
function localDayStart(day: string): Date {
  return new Date(`${day}T00:00:00`);
}

/**
 * Local day range -> the half-open instant range the backend compares against.
 * `until` is midnight of the day AFTER `toDay`, so `toDay` is fully included.
 * `toISOString()` emits exactly the format Rust's `stamp()` writes.
 */
export function dayRangeToInstants(
  fromDay: string | null,
  toDay: string | null,
): { from: string | null; until: string | null } {
  const until = toDay === null ? null : localDayStart(toDay);
  if (until) until.setDate(until.getDate() + 1);
  return {
    from: fromDay === null ? null : localDayStart(fromDay).toISOString(),
    until: until ? until.toISOString() : null,
  };
}

/**
 * Relative date presets. Ranges are inclusive calendar days ending today, so
 * "Last 7 days" is today plus the six before it. Resolved at click time — these
 * are ephemeral session filters, not stored rules.
 */
export const DATE_PRESETS = [
  { key: "today", label: "Today", fromOffset: 0 },
  { key: "yesterday", label: "Yesterday", fromOffset: -1, toOffset: -1 },
  { key: "7", label: "Last 7 days", fromOffset: -6 },
  { key: "30", label: "Last 30 days", fromOffset: -29 },
  { key: "90", label: "Last 90 days", fromOffset: -89 },
  { key: "365", label: "Last 365 days", fromOffset: -364 },
] as const;

/** A preset key -> the instant range it means right now. */
export function presetToInstants(key: string): { from: string | null; until: string | null } {
  const preset = DATE_PRESETS.find((p) => p.key === key);
  if (!preset) return { from: null, until: null };
  const toOffset = "toOffset" in preset ? preset.toOffset : 0;
  const until = localMidnight(toOffset + 1); // exclusive: start of the next day
  return { from: localMidnight(preset.fromOffset).toISOString(), until: until.toISOString() };
}

export const ASSET_TYPE_LABELS: { value: AssetTypeFilter; label: string }[] = [
  { value: "image", label: "Images" },
  { value: "video", label: "Videos" },
  { value: "audio", label: "Audio" },
];

/**
 * Ratio match tolerance, in ratio units: 0.02 on 16:9 (1.778) accepts
 * 1.758–1.798, so a 1918×1080 crop still counts as 16:9. Exact equality would
 * make the fixed presets miss almost everything real.
 */
export const RATIO_TOLERANCE = 0.02;

const ratio = (num: number, den: number): Shape => ({
  kind: "ratio",
  num,
  den,
  tolerance: RATIO_TOLERANCE,
});

/**
 * Shape dropdown entries. `key` is the <select> value — a flat string, because
 * a ratio can't be an option value on its own. Grouped for the UI.
 */
export const SHAPE_PRESETS: { key: string; label: string; group: string; shape: Shape }[] = [
  { key: "horizontal", label: "Horizontal", group: "General", shape: { kind: "horizontal" } },
  { key: "vertical", label: "Vertical", group: "General", shape: { kind: "vertical" } },
  { key: "square", label: "Square", group: "General", shape: { kind: "square" } },
  { key: "ultrawide", label: "Ultrawide", group: "Panoramic", shape: { kind: "ultrawide" } },
  {
    key: "panoramic_vertical",
    label: "Panoramic vertical",
    group: "Panoramic",
    shape: { kind: "panoramic_vertical" },
  },
  { key: "16:9", label: "16:9", group: "Fixed ratio", shape: ratio(16, 9) },
  { key: "9:16", label: "9:16", group: "Fixed ratio", shape: ratio(9, 16) },
  { key: "3:2", label: "3:2", group: "Fixed ratio", shape: ratio(3, 2) },
  { key: "2:3", label: "2:3", group: "Fixed ratio", shape: ratio(2, 3) },
  { key: "4:3", label: "4:3", group: "Fixed ratio", shape: ratio(4, 3) },
  { key: "3:4", label: "3:4", group: "Fixed ratio", shape: ratio(3, 4) },
];

/** Preset groups in display order, for <optgroup>. */
export const SHAPE_GROUPS = ["General", "Panoramic", "Fixed ratio"] as const;

/**
 * Pixel size -> the most specific shape word that applies, for read-only display
 * (the inspector). Deliberately reuses the FILTER's vocabulary so the panel says
 * "Ultrawide" about exactly the assets the Ultrawide filter would return.
 *
 * Thresholds mirror Shape::push_predicate in assets.rs — keep them in step.
 */
export function describeShape(width: number, height: number): string {
  if (width <= 0 || height <= 0) return "—";
  if (width === height) return "Square";
  if (width >= height * 2) return "Ultrawide";
  if (height >= width * 2) return "Panoramic vertical";
  return width > height ? "Horizontal" : "Vertical";
}

/**
 * Active shape -> <select> key. A ratio that matches no preset is "custom", which
 * is what keeps the custom inputs open after a reload.
 */
export function shapeKey(shape: Shape | null): string {
  if (!shape) return "";
  if (shape.kind !== "ratio") return shape.kind;
  const preset = SHAPE_PRESETS.find(
    (p) => p.shape.kind === "ratio" && p.shape.num === shape.num && p.shape.den === shape.den,
  );
  return preset ? preset.key : "custom";
}

/**
 * A named, reusable rule set applied as a LENS — it narrows whatever scope
 * you're currently in, so it has no sort and no place in the folder tree.
 *
 * Stored in `rule_sets` with `kind = 'filter'`. A smart folder is the same row
 * with `kind = 'smart'`: same document, same compiler, different affordance.
 */
export interface SavedFilter {
  id: string;
  name: string;
  position: number;
  /** The stored rule tree. See lib/rules.ts. */
  rules: RuleNode;
}

/**
 * The same stored row as a `SavedFilter`, used as a PLACE.
 *
 * A smart folder owns a persisted sort (under `view_settings` key `smart:<id>`)
 * and can be grouped and pinned; a filter owns none of that. That difference —
 * place versus lens — is the whole reason both exist in the UI when they share
 * one table, one document format and one compiler underneath.
 */
export interface SmartFolder {
  id: string;
  name: string;
  notes: string | null;
  group_id: string | null;
  position: number;
  rules: RuleNode;
  color: PinColor | null;
  pin_position: number | null;
}

/** Partial update; omitted fields are left alone. */
export interface SmartFolderPatch {
  name?: string;
  notes?: string;
  rules?: RuleNode;
}

/**
 * A sidebar container for smart folders that is ALSO a place: clicking one
 * browses the union of its members.
 *
 * It owns a sort (under `view_settings` key `smartgroup:<id>`) — every mode
 * except manual, which a union of several independently-ordered folders has no
 * answer for.
 */
export interface SmartFolderGroup {
  id: string;
  name: string;
  notes: string | null;
  position: number;
}

/**
 * Accent colours a pinned folder can wear. Token names, not hex — the values
 * live in layout.css as `--pin-*`, so a theme change retints every pin without
 * touching the database. Mirrors `PIN_COLORS` in assets.rs; the backend rejects
 * anything not in this list.
 */
export const PIN_COLORS = [
  "slate",
  "blue",
  "cyan",
  "emerald",
  "lime",
  "amber",
  "rose",
  "violet",
] as const;

export type PinColor = (typeof PIN_COLORS)[number];

/**
 * Which kind of thing a pin points at. Mirrors `PinKind` in assets.rs, whose
 * spelling is pinned by `pin_wire_tests` — it crosses three boundaries (the SQL
 * literal, serde, and this file), so it is not a name to guess at.
 */
export type PinKind = "folder" | "smart";

/**
 * One entry in the sidebar's shortlist.
 *
 * Folders and smart folders share ONE order the user arranges freely, which is
 * why this is a single list with a discriminant rather than two lists rendered
 * back to back — those would interleave by accident of independent numbering
 * rather than by choice.
 */
export interface PinnedItem {
  kind: PinKind;
  id: string;
  name: string;
  color: PinColor | null;
  position: number;
}

export interface Folder {
  id: string;
  name: string;
  parent_id: string | null;
  position: number;
  order_by: OrderBy;
  is_ascending: boolean;
  notes: string | null;
  /** When the folder entered this library (RFC 3339, UTC). */
  created_at: string;
  /** Sidebar accent token. Survives unpinning, so re-pinning restores the look. */
  color: PinColor | null;
  /** Rank among pinned folders. `null` = not pinned. */
  pin_position: number | null;
}

/** Exact totals for a set of assets, computed by the DB rather than estimated. */
export interface SelectionSummary {
  count: number;
  total_bytes: number;
}

/** How many of the queried assets sit in one folder. Absent folders mean zero. */
export interface FolderMembership {
  folder_id: string;
  count: number;
}

/**
 * A tag with its live usage count. Globally unique by name (case-insensitive), a
 * lens on assets — never on folders. `group_id`/`color`/`is_starred` are carried
 * from the schema but only exercised from T2 on.
 */
export interface Tag {
  id: string;
  name: string;
  color: string | null;
  group_id: string | null;
  is_starred: boolean;
  position: number;
  usage: number;
  /** Most recent application (RFC 3339), or null if never used. Drives recency. */
  last_used: string | null;
}

/** How many of a selection carry one tag. Absent tags mean zero. */
export interface TagUsage {
  tag_id: string;
  count: number;
}

/** A tag group with its tag count. Pure organization; a tag has at most one. */
export interface TagGroup {
  id: string;
  name: string;
  color: string | null;
  position: number;
  tag_count: number;
}

/**
 * Membership of a folder across a selection.
 *
 * The "some" state is what makes batch editing safe: it says the folder holds a
 * SUBSET, so acting on it must be an add/remove delta rather than an overwrite.
 * Tags will reuse this exact shape when that feature lands.
 */
export type TriState = "all" | "some" | "none";

export const triStateOf = (count: number, total: number): TriState =>
  count === 0 ? "none" : count >= total ? "all" : "some";

/** What a folder holds, counting its whole subtree. Computed on demand. */
export interface FolderStats {
  asset_count: number;
  total_bytes: number;
  descendant_folders: number;
}

// ThumbHash (base64) -> data URL, memoized (cards mount/unmount on scroll).
const thumbUrlCache = new Map<string, string>();
export function thumbHashUrl(hash: string | null): string | null {
  if (!hash) return null;
  let url = thumbUrlCache.get(hash);
  if (!url) {
    const bin = atob(hash);
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    url = thumbHashToDataURL(bytes);
    thumbUrlCache.set(hash, url);
  }
  return url;
}

// Cap on hydrated heavy rows kept in memory. A few sccreenfuls of slacks.
const MAX_HEAVY = 600;

class AssetLibrary {
  /** Layout source of truth: light rows for every asset, sort order from Rust. */

  manifest = $state<AssetLightRow[]>([]);
  isLoading = $state(false);
  error = $state<string | null>(null);

  /**
   * Instant name-filter — the frontend half of the search hybrid. A name-only
   * search filters the already-loaded manifest in memory (0ms, no round trip)
   * instead of going through the backend FTS. `null` = inactive. Every other
   * search goes through `filters.text` and is already baked into `manifest`.
   */
  #nameFilter = $state<string | null>(null);

  /** The manifest the GRID renders: `manifest`, narrowed by the instant name
   *  filter when one is active. Every consumer reads THIS, so selection, count,
   *  and layout all see the same filtered set. */
  get displayed(): AssetLightRow[] {
    const q = this.#nameFilter;
    if (q === null) return this.manifest;
    const needle = q.toLowerCase();
    return this.manifest.filter((r) => r.filename.toLowerCase().includes(needle));
  }

  /** True while the instant name filter is hiding some rows — for the "filtered"
   *  hint, since this narrowing isn't part of `filters`/`hasFilters`. */
  get nameFiltering(): boolean {
    return this.#nameFilter !== null;
  }

  /** Set/clear the instant name filter. Blank normalises to `null`. */
  setNameFilter(query: string | null): void {
    this.#nameFilter = query && query.trim() !== "" ? query.trim() : null;
  }

  /** The slice of the library currently shown (drives which folder is active). */
  scope = $state<ManifestScope>({ kind: "all" });
  /** The sort the CURRENT manifest was built with — always what the query did. */
  sort = $state<Sort>({ ...DEFAULT_SORT });
  /**
   * Ephemeral narrowing of the current scope. Never persisted (see FilterSet in
   * assets.rs), but DOES survive switching folders within a session — filtering
   * to images and then browsing folders is a normal workflow. That persistence
   * is exactly why the UI must keep an always-visible clear affordance.
   */
  filters = $state<FilterSet>(emptyFilters());
  /** Flat folder list for the tree UI, refreshed on library switch + import. */
  folders = $state<Folder[]>([]);
  /**
   * folder id -> name, for rendering rules that reference folders by id.
   *
   * Lives here so the three places that summarise a rule set agree, and so a
   * folder rename shows up in every one of them at once.
   */
  get folderNames(): ReadonlyMap<string, string> {
    return new Map(this.folders.map((f) => [f.id, f.name]));
  }

  /** Named filter combinations for this library, refreshed on library switch. */
  savedFilters = $state<SavedFilter[]>([]);
  smartFolders = $state<SmartFolder[]>([]);
  smartFolderGroups = $state<SmartFolderGroup[]>([]);

  /**
   * A rule tree that overrides the flat dimensions for the next query.
   *
   * Only set by applying a saved filter the bar can't draw. Cleared by every
   * dimension change (see `setFilters`), so it can never linger as an invisible
   * constraint the user has no control for.
   */
  #rulesOverride = $state<RuleNode | null>(null);
  /**
   * Every tag in the library with its usage count, alphabetical. Refreshed on
   * library switch and after any tag mutation, so usage counts and the inspector
   * always read from one authoritative list.
   */
  tags = $state<Tag[]>([]);
  /** Tag groups for the Tag Manager. Refreshed alongside `tags`. */
  tagGroups = $state<TagGroup[]>([]);
  /**
   * How many images have a color palette; null until first read. Surfaced in the
   * UI because a color filter cannot match an un-analyzed asset, and a filter
   * that quietly under-reports is worse than one that admits what it can't see.
   */
  colorCoverage = $state<ColorCoverage | null>(null);

  /**
   * Cache-buster for regenerated thumbnails. A rebuild reuses the same
   * `id.webp` path with new bytes, so the webview would serve the stale cached
   * image; bumping this after a rebuild is appended to thumbnail URLs (see
   * AssetCard) to force a refetch. No effect on freshly generated thumbnails.
   */
  thumbVersion = $state(0);

  /**
   * Background thumbnail-generation progress for the UI indicator; null when
   * idle. Only multi-chunk runs (Rebuild / large pending) are surfaced — on-view
   * batches are single-chunk and finish before a bar would help.
   */
  thumbProgress = $state<{ current: number; total: number } | null>(null);

  /** Heavy rows keyed by id, hydrated per visible window. Reactive. */
  heavy = new SvelteMap<string, AssetMetadata>();

  /** Ids with an in-flight heavy-row hydration request, to avoid re-requesting. */
  #pending = new Set<string>();
  /** Bumped per manifest load (scope/sort change) — guards manifest streaming. */
  #loadToken = 0;
  /**
   * Bumped ONLY on a library switch — guards the heavy-row and thumbnail caches.
   * Separate from #loadToken because a sort change reorders the same library:
   * cached rows stay valid, so re-sorting must not throw away hydration work.
   */
  #libraryToken = 0;

  /** id -> manifest index, for O(1) row patching (rebuilt on each load). */
  #indexById = new Map<string, number>();
  /** Ids with an in-flight on-view thumbnail request, to avoid re-requesting. */
  #thumbRequested = new Set<string>();
  /**
   * On-view generation queue (front = highest priority). Each request pushes the
   * current window to the FRONT so what's on screen generates first, while windows
   * scrolled past stay queued behind it — keeping the pipeline full. Drained by
   * `#pumpThumbnails` with up to `#THUMB_CONCURRENCY` batches in flight, which
   * keeps the CPU busy across IPC/DB round-trips without letting a fast scroll
   * flood SQLite with concurrent writers.
   */
  #thumbQueue: string[] = [];
  #thumbInFlight = 0;
  #thumbMode = "auto";
  #thumbQuality = 80;
  #thumbProgressHideTimer: ReturnType<typeof setTimeout> | undefined;
  /** Max concurrent generation batches, and ids per batch. */
  static #THUMB_CONCURRENCY = 3;
  static #THUMB_BATCH = 32;

  /**
   * (Re)load the manifest for `scope` with `sort`. Both are stored so the UI can
   * always show what the current view actually did.
   *
   * The heavy-row cache is deliberately NOT cleared here: a scope or sort change
   * reorders or narrows the same library, and rows are keyed by id, so every
   * cached row stays valid. Only a library switch invalidates them —
   * `clearCaches()`.
   */
  async load(
    scope: ManifestScope = this.scope,
    sort: Sort = this.sort,
    filters: FilterSet = this.filters,
    { quiet = false }: { quiet?: boolean } = {},
  ): Promise<void> {
    // The ASSET selection is a subset of what's on screen. A sort change reorders
    // the very same rows, so it survives one (ids stay valid; indices are
    // recomputed from the manifest at click time). Anything that changes WHICH
    // rows exist drops it — otherwise the inspector shows an asset the user can't
    // see, and a bulk action mutates invisible rows.
    //
    // `clearAssets`, not `clear`: a selected folder is not a claim about the
    // manifest, and navigating INTO a folder is precisely when it gets selected.
    //
    // Identity comparison on `filters` is exactly right: every setter builds a
    // new object, and a sort-only reload passes `this.filters` straight through.
    if (!sameScope(scope, this.scope) || filters !== this.filters) selection.clearAssets();

    this.scope = scope;
    this.sort = sort;
    this.filters = filters;
    const token = ++this.#loadToken;
    this.isLoading = true;
    this.error = null;
    // Queued windows describe the OLD order, so drop them; the grid re-requests
    // its visible window as soon as the new manifest paints.
    this.#thumbQueue = [];

    // `quiet` keeps the OLD rows on screen and swaps them in one shot when the
    // stream finishes — a refresh of the SAME view (reorder, membership change,
    // import-into-current) must never blank the grid to "Loading…". A plain
    // load (scope/library change) still streams progressively into an emptied
    // manifest, because there you navigated away and a blank is expected.
    const streamed: AssetLightRow[] = [];
    if (!quiet) {
      this.manifest = [];
      this.#indexById.clear();
    }

    // Quiet loads buffer into `streamed` and swap once the old rows can be
    // replaced — but only UNTIL the swap. Channel messages can arrive AFTER the
    // invoke promise resolves (see the append branch), so once we've swapped we
    // must keep appending live, or those late rows are dropped and the grid ends
    // up empty. `swapped` gates that handoff.
    let swapped = false;

    const appendLive = (chunk: AssetLightRow[]) => {
      // Build the id->index map incrementally, in lock-step with the manifest.
      const base = this.manifest.length;
      for (let i = 0; i < chunk.length; i++) this.#indexById.set(chunk[i].id, base + i);
      // Push in place (reactive under $state) — O(chunk), not the O(n) full-array
      // copy `slice()` did per chunk (which was O(n²) over a large library).
      this.manifest.push(...chunk);
    };

    try {
      const channel = new Channel<AssetLightRow[]>();
      channel.onmessage = (chunk) => {
        if (token !== this.#loadToken) return; // a newer load superseded this one
        // Quiet AND not yet swapped → hold the chunk; the old rows stay visible.
        // Everything else (non-quiet, or a late chunk after the swap) appends
        // straight to the live manifest.
        if (quiet && !swapped) {
          streamed.push(...chunk);
        } else {
          appendLive(chunk);
        }
      };
      // The one place the UI's flat dimensions become the rule tree the engine
      // actually speaks. `#rulesOverride` wins when a saved filter said
      // something the filter bar can't draw — see `applySavedFilter`.
      // `$state.snapshot` before crossing the IPC boundary, as every other
      // invoke in this file does: what goes over the wire must be plain data,
      // not reactive proxies.
      const plain = $state.snapshot(filters) as FilterSet;
      const wire = {
        rules: this.#rulesOverride
          ? ($state.snapshot(this.#rulesOverride) as RuleNode)
          : toRuleTree(plain),
        text: plain.text,
      };
      await invoke("stream_manifest", {
        query: { scope, filters: wire, sort },
        onChunk: channel,
      });
      // Swap in whatever streamed before the invoke resolved. Zero chunks is a
      // legitimate empty result set — the swap must still happen so a no-match
      // search shows empty rather than keeping stale rows. Late chunks now flow
      // through `appendLive`.
      if (quiet && token === this.#loadToken) {
        this.#swapManifest(streamed);
        swapped = true;
      }
    } catch (e) {
      if (token === this.#loadToken) {
        this.error = typeof e === "string" ? e : "Failed to load assets.";
      }
    } finally {
      if (token === this.#loadToken) this.isLoading = false;
    }
  }

  /** Replace the manifest and its index map atomically (no empty frame). */
  #swapManifest(rows: AssetLightRow[]): void {
    this.#indexById.clear();
    for (let i = 0; i < rows.length; i++) this.#indexById.set(rows[i].id, i);
    this.manifest = rows;
  }

  /** Refresh the CURRENT view without blanking the grid. */
  reload(): Promise<void> {
    return this.load(this.scope, this.sort, this.filters, { quiet: true });
  }

  /**
   * Drop every cached row. Valid ONLY on a library switch — asset ids are
   * library-scoped, so carrying them over would mix two libraries' data.
   * Bumping `#libraryToken` also invalidates in-flight hydration + thumbnail work.
   */
  clearCaches(): void {
    this.#libraryToken++;
    this.heavy.clear();
    this.#pending.clear();
    this.#thumbRequested.clear();
    this.#thumbQueue = [];
    // Asset ids are library-scoped, so a carried-over selection would point at
    // rows in the library we just left.
    selection.clear();
    // Session view state rather than a cache, but it resets with the library for
    // the same reason filters aren't persisted: landing in a fresh library that
    // silently shows a subset reads as "my import didn't work".
    this.filters = emptyFilters();
    this.#nameFilter = null;
  }

  /**
   * Switch the visible slice. Reads that scope's persisted sort first, so the
   * sort control never lies about what the query did — one extra round trip, and
   * the alternative (render a guess, then correct it) flickers.
   */
  async setScope(scope: ManifestScope): Promise<void> {
    let sort: Sort;
    try {
      sort = await invoke<Sort>("fetch_sort", { scope });
    } catch (e) {
      console.error("Failed to read persisted sort; keeping the current one:", e);
      // Read AFTER the await on purpose: a synchronous `this.sort` read here would
      // become a dependency of any $effect that calls setScope, and load() writes
      // `sort` — that pair is an effect loop. Post-await reads aren't tracked.
      sort = this.sort;
    }
    await this.load(scope, sort);
  }

  /** Whether any filter dimension is active — drives the "Clear" affordance. */
  get hasFilters(): boolean {
    const f = this.filters;
    return (
      (this.#rulesOverride !== null && isActive(this.#rulesOverride)) ||
      f.asset_types.length > 0 ||
      f.shape !== null ||
      f.date !== null ||
      f.size !== null ||
      f.color !== null ||
      f.tags !== null ||
      f.text !== null
    );
  }

  /**
   * Set (or clear) the live text search and reload. A blank query normalises to
   * `null`, so an empty search bar isn't a filter — same rule as the range
   * dimensions. The scopes ride along so a scope toggle re-runs the search.
   */
  setSearch(text: TextSearch | null): Promise<void> {
    const normalised = text && text.query.trim() !== "" ? text : null;
    return this.setFilters({ ...this.filters, text: normalised });
  }

  /**
   * Replace the whole filter set and reload. Filters are never persisted.
   *
   * Quiet reload — a filter change narrows the SAME scope, so the old rows stay
   * on screen and the new set swaps in one frame. Without this, live search
   * blanked the grid on every keystroke (each load emptied `manifest` before
   * restreaming), which read as a black flash.
   */
  setFilters(filters: FilterSet, rules: RuleNode | null = null): Promise<void> {
    // Defaulting to null is what makes the flat dimensions authoritative: every
    // control funnels through here, so touching any of them drops a tree the bar
    // couldn't have produced rather than silently ANDing with it.
    this.#rulesOverride = rules;
    return this.load(this.scope, this.sort, filters, { quiet: true });
  }

  clearFilters(): Promise<void> {
    return this.setFilters(emptyFilters());
  }

  /** Add/remove one type. Types within the dimension OR together. */
  toggleAssetType(type: AssetTypeFilter): Promise<void> {
    const current = this.filters.asset_types;
    const asset_types = current.includes(type)
      ? current.filter((t) => t !== type)
      : [...current, type];
    return this.setFilters({ ...this.filters, asset_types });
  }

  /** Constrain by shape; `null` clears the dimension. */
  setShape(shape: Shape | null): Promise<void> {
    return this.setFilters({ ...this.filters, shape });
  }

  /**
   * Constrain by date range. A range with both ends open is no constraint at
   * all, so it normalises to `null` — otherwise `hasFilters` would light the
   * bar up and offer a "Clear" for a filter that isn't filtering anything.
   */
  setDateFilter(date: DateFilter | null): Promise<void> {
    const normalised = date && date.from === null && date.until === null ? null : date;
    return this.setFilters({ ...this.filters, date: normalised });
  }

  /** Constrain by byte size. Both ends open normalises to `null`, as above. */
  setSizeRange(size: SizeRange | null): Promise<void> {
    const normalised = size && size.min === null && size.max === null ? null : size;
    return this.setFilters({ ...this.filters, size: normalised });
  }

  /** Constrain by dominant color; `null` clears the dimension. */
  setColorFilter(color: ColorFilter | null): Promise<void> {
    return this.setFilters({ ...this.filters, color });
  }

  /**
   * Constrain by tags. A filter that would narrow nothing (no includes, no
   * excludes, not untagged) normalises to `null`, same as the range filters —
   * so switching mode with nothing selected doesn't light up the bar.
   */
  setTagFilter(tags: TagFilter | null): Promise<void> {
    const normalised = tags && isTagFilterEmpty(tags) ? null : tags;
    return this.setFilters({ ...this.filters, tags: normalised });
  }

  /**
   * Refresh how many images have been color-analyzed. Cheap (two COUNTs), and
   * re-read after an analysis run so the notice reflects reality.
   */
  async loadColorCoverage(): Promise<void> {
    try {
      this.colorCoverage = await invoke<ColorCoverage>("color_coverage");
    } catch (e) {
      console.error("Failed to read color coverage:", e);
      this.colorCoverage = null;
    }
  }

  /**
   * One asset's palette, most-covering first. Empty means "not analyzed yet".
   *
   * Not cached: it's eight rows behind an indexed lookup, and a cache would go
   * stale the moment "Analyze colors" runs.
   */
  fetchPalette(assetId: string): Promise<PaletteSwatch[]> {
    return invoke<PaletteSwatch[]>("fetch_palette", { assetId });
  }

  /** Backfill palettes for un-analyzed images. Resolves with the count done. */
  async analyzeColors(): Promise<number> {
    const count = await invoke<number>("analyze_colors");
    await this.loadColorCoverage();
    return count;
  }

  // ── Saved filters ──────────────────────────────────────────────────────────

  /** Refresh this library's saved filters. Non-fatal on failure. */
  async loadSavedFilters(): Promise<void> {
    try {
      this.savedFilters = await invoke<SavedFilter[]>("fetch_saved_filters");
    } catch (e) {
      console.error("Failed to load saved filters:", e);
      this.savedFilters = [];
    }
  }

  /** The active lens as the engine sees it: a tree, never the flat view model. */
  #currentRules(): { rules: RuleNode | null; text: null } {
    // `text` is deliberately null, not omitted: the live search box is a typed
    // query, not a durable lens. It structurally can't reach storage now that
    // it lives outside the tree, and sending null says so at the boundary too.
    return { rules: this.#rulesOverride ?? toRuleTree($state.snapshot(this.filters)), text: null };
  }

  /** Store the CURRENT filter set under `name`. */
  async saveCurrentFilters(name: string): Promise<void> {
    await invoke("create_saved_filter", { name, filters: this.#currentRules() });
    await this.loadSavedFilters();
  }

  /**
   * Apply a saved filter to the current scope — it's a lens, so the scope and
   * sort are untouched. The stored tree is deep-copied so tweaking the filters
   * afterwards doesn't quietly rewrite the saved definition.
   *
   * A tree the filter bar can't display (nested groups, `none`, text
   * conditions) is applied to the QUERY regardless — dropping conditions we
   * can't draw would show more assets than the user asked for. The bar's own
   * controls just read as empty until Phase 2's rule editor can show them.
   */
  applySavedFilter(id: string): Promise<void> {
    const saved = this.savedFilters.find((f) => f.id === id);
    if (!saved) return Promise.resolve();
    const rules = $state.snapshot(saved.rules) as RuleNode;
    const dimensions = fromRuleTree(rules);
    // Representable: let the bar own it, so its controls show what's active and
    // the user can adjust one dimension without losing the rest. Otherwise keep
    // the tree as the override — the query must be exact even when the bar has
    // no control that can display it.
    return dimensions
      ? this.setFilters({ ...emptyFilters(), ...dimensions })
      : this.setFilters(emptyFilters(), rules);
  }

  async renameSavedFilter(id: string, name: string): Promise<void> {
    await invoke("rename_saved_filter", { id, name });
    await this.loadSavedFilters();
  }

  /** Overwrite a saved filter with whatever is active now. */
  async updateSavedFilter(id: string): Promise<void> {
    await invoke("update_saved_filter", { id, filters: this.#currentRules() });
    await this.loadSavedFilters();
  }

  async deleteSavedFilter(id: string): Promise<void> {
    await invoke("delete_saved_filter", { id });
    await this.loadSavedFilters();
  }

  // ── Smart folders ──────────────────────────────────────────────────────────
  //
  // Same stored rows as saved filters, used as PLACES: a smart folder's tree
  // becomes the scope predicate, so a lens still applies on top of it.

  /** Refresh this library's smart folders. Non-fatal on failure. */
  async loadSmartFolders(): Promise<void> {
    try {
      this.smartFolders = await invoke<SmartFolder[]>("fetch_smart_folders");
    } catch (e) {
      console.error("Failed to load smart folders:", e);
      this.smartFolders = [];
    }
  }

  async createSmartFolder(name: string, rules: RuleNode): Promise<SmartFolder> {
    const created = await invoke<SmartFolder>("create_smart_folder", { name, rules });
    await this.loadSmartFolders();
    return created;
  }

  /**
   * Patch a smart folder. Reloads the manifest when the edited folder is the
   * one on screen — changing its rules changes what it contains, and leaving
   * the old rows up would show assets that no longer belong here.
   */
  async updateSmartFolder(id: string, patch: SmartFolderPatch): Promise<void> {
    await invoke("update_smart_folder", { id, patch });
    await this.loadSmartFolders();
    if (this.scope.kind === "smart" && this.scope.id === id) await this.reload();
  }

  /**
   * Delete a smart folder, leaving it if we're standing in it.
   *
   * Same rule as deleting the folder you're browsing: the scope has to go
   * somewhere, and "All assets" is the only place guaranteed to exist.
   */
  async deleteSmartFolder(id: string): Promise<void> {
    await invoke("delete_smart_folder", { id });
    await this.loadSmartFolders();
    if (this.scope.kind === "smart" && this.scope.id === id) {
      await this.setScope({ kind: "all" });
    }
  }

  /** Live "Found N items" for the rule editor. Callers debounce. */
  countMatching(rules: RuleNode): Promise<number> {
    return invoke<number>("count_matching", { rules });
  }

  // ── Smart folder groups ────────────────────────────────────────────────────

  async loadSmartFolderGroups(): Promise<void> {
    try {
      this.smartFolderGroups = await invoke<SmartFolderGroup[]>("fetch_smart_folder_groups");
    } catch (e) {
      console.error("Failed to load smart folder groups:", e);
      this.smartFolderGroups = [];
    }
  }

  async createSmartFolderGroup(name: string): Promise<void> {
    await invoke("create_smart_folder_group", { name });
    await this.loadSmartFolderGroups();
  }

  async renameSmartFolderGroup(id: string, name: string): Promise<void> {
    await invoke("rename_smart_folder_group", { id, name });
    await this.loadSmartFolderGroups();
  }

  /**
   * Delete a group. Its members are ungrouped, never deleted — so this leaves
   * the scope only when you were browsing the group itself.
   */
  async deleteSmartFolderGroup(id: string): Promise<void> {
    await invoke("delete_smart_folder_group", { id });
    await this.loadSmartFolderGroups();
    await this.loadSmartFolders();
    if (this.scope.kind === "smart_group" && this.scope.id === id) {
      await this.setScope({ kind: "all" });
    }
  }

  /** Move a smart folder into a group; `null` ungroups it. */
  async setSmartFolderGroup(id: string, groupId: string | null): Promise<void> {
    await invoke("set_smart_folder_group", { id, groupId });
    await this.loadSmartFolders();
    // A group's contents just changed, so its union did too.
    if (this.scope.kind === "smart_group") await this.reload();
  }

  /** Change the sort for the CURRENT scope, persist it, and reload. */
  async setSort(sort: Sort): Promise<void> {
    const scope = this.scope;
    try {
      await invoke("set_sort", { scope, sort });
    } catch (e) {
      // Non-fatal: apply it for this session even if persistence failed.
      console.error("Failed to persist sort:", e);
    }
    await this.load(scope, sort);
  }

  /**
   * Drop a block of assets at a new spot in the CURRENT scope's manual order.
   * `afterId` is the asset the block lands behind, null for the head.
   *
   * Only valid when the active sort is manual — a reorder writes a rank, and a
   * rank is invisible under any other sort, so the caller gates on that. The
   * scope decides which rank column is written (see the Rust side); this just
   * passes the scope the manifest was built with.
   */
  async reorderAssets(movedIds: string[], afterId: string | null): Promise<void> {
    if (movedIds.length === 0) return;

    // Apply the new order to the manifest IMMEDIATELY, before the round trip.
    // The persisted order is exactly this array's order, so the optimistic view
    // is what the query would return anyway — showing it now makes the reorder
    // feel instant AND lets the user see the true (re-packed) result at once,
    // instead of a flash followed by a jump when a reload lands.
    const moved = new Set(movedIds);
    const kept = this.manifest.filter((r) => !moved.has(r.id));
    // Moved rows keep their manifest order, matching how Rust orders the block.
    const movedRows = this.manifest.filter((r) => moved.has(r.id));
    const at = afterId ? kept.findIndex((r) => r.id === afterId) + 1 : 0;
    const next = [...kept.slice(0, at), ...movedRows, ...kept.slice(at)];
    this.#swapManifest(next);

    try {
      await invoke("reorder_assets", { scope: this.scope, movedIds, afterId });
    } catch (e) {
      // The optimistic order is now a lie — pull the truth back from the DB.
      await this.reload();
      throw e;
    }
  }

  /** Refresh the folder tree for the active library. Non-fatal on failure. */
  async loadFolders(): Promise<void> {
    try {
      this.folders = await invoke<Folder[]>("fetch_folders");
    } catch (e) {
      console.error("Failed to load folders:", e);
      this.folders = [];
    }
  }

  /** Create a folder (root when `parentId` is null) and refresh the tree. */
  async createFolder(name: string, parentId: string | null = null): Promise<void> {
    await invoke<Folder>("create_folder", { name, parentId });
    await this.loadFolders();
  }

  renameFolder(id: string, name: string): Promise<void> {
    return this.updateFolder(id, { name });
  }

  /**
   * Exact count and total size for a selection. A real query rather than a sum
   * over cached rows: the heavy cache holds a bounded window, so summing it
   * would silently under-report the moment a selection outgrows the screen.
   */
  fetchSelectionSummary(assetIds: string[]): Promise<SelectionSummary> {
    return invoke<SelectionSummary>("selection_summary", { assetIds });
  }

  /** Per-folder membership counts for a selection; drives the tri-state UI. */
  fetchFolderMembership(assetIds: string[]): Promise<FolderMembership[]> {
    return invoke<FolderMembership[]>("folder_membership", { assetIds });
  }

  // ── Tags ────────────────────────────────────────────────────────────────

  /** Refresh the tag list for the active library. Non-fatal on failure. */
  async loadTags(): Promise<void> {
    try {
      this.tags = await invoke<Tag[]>("fetch_tags");
    } catch (e) {
      console.error("Failed to load tags:", e);
      this.tags = [];
    }
  }

  /**
   * Find-or-create a tag by name (case-insensitive), returning its id. The
   * backend reuses an existing row rather than duplicating, so "Red" typed twice
   * is one tag. Refreshes the list so a newly created tag appears everywhere.
   */
  async ensureTag(name: string): Promise<string> {
    const id = await invoke<string>("ensure_tag", { name });
    await this.loadTags();
    return id;
  }

  async renameTag(id: string, name: string): Promise<void> {
    await invoke("rename_tag", { id, name });
    await this.loadTags();
  }

  /** Delete a tag globally (removes every assignment). */
  async deleteTag(id: string): Promise<void> {
    await invoke("delete_tag", { id });
    await this.loadTags();
    await this.#reloadIfTagFiltered();
  }

  /** Assign a tag to assets. Reloads the tag list so usage counts stay honest. */
  async assignTag(tagId: string, assetIds: string[]): Promise<void> {
    await invoke("assign_tag", { tagId, assetIds });
    await this.loadTags();
    await this.#reloadIfTagFiltered();
  }

  async unassignTag(tagId: string, assetIds: string[]): Promise<void> {
    await invoke("unassign_tag", { tagId, assetIds });
    await this.loadTags();
    await this.#reloadIfTagFiltered();
  }

  /**
   * Reload the manifest when a tag filter is active — editing an asset's tags can
   * move it in or out of the current view. Skipped otherwise so the common
   * "tag while browsing unfiltered" path doesn't re-stream the whole library.
   *
   * This is `reload()`, which passes the SAME `filters` object, so `load()` keeps
   * the selection (its clear only fires on a scope/filters CHANGE). That's what
   * lets a multi-tag bulk session keep its selection between edits. Rows the edit
   * pushed out of a filtered view stay selected-but-hidden until the next
   * selection change — an acceptable seam for the filter-and-edit-at-once combo.
   */
  async #reloadIfTagFiltered(): Promise<void> {
    if (this.filters.tags !== null) await this.reload();
  }

  /** Per-tag counts across a selection; drives the inspector's tri-state. */
  fetchTagUsage(assetIds: string[]): Promise<TagUsage[]> {
    return invoke<TagUsage[]>("tag_usage_for_assets", { assetIds });
  }

  // ── Tag Manager: tag attributes ───────────────────────────────────────────

  async setTagColor(id: string, color: string | null): Promise<void> {
    await invoke("set_tag_color", { id, color });
    await this.loadTags();
  }

  async setTagStarred(id: string, starred: boolean): Promise<void> {
    await invoke("set_tag_starred", { id, starred });
    await this.loadTags();
  }

  async setTagGroup(id: string, groupId: string | null): Promise<void> {
    await invoke("set_tag_group", { id, groupId });
    await this.loadTags();
    await this.loadTagGroups();
  }

  /** Merge `source` into `target`, then refresh. Reloads the manifest if a tag
      filter references the vanished source. Irreversible — confirm before calling. */
  async mergeTags(source: string, target: string): Promise<void> {
    await invoke("merge_tags", { source, target });
    await this.loadTags();
    await this.#reloadIfTagFiltered();
  }

  // ── Tag Manager: groups ───────────────────────────────────────────────────

  async loadTagGroups(): Promise<void> {
    try {
      this.tagGroups = await invoke<TagGroup[]>("fetch_tag_groups");
    } catch (e) {
      console.error("Failed to load tag groups:", e);
      this.tagGroups = [];
    }
  }

  async createTagGroup(name: string): Promise<string> {
    const id = await invoke<string>("create_tag_group", { name });
    await this.loadTagGroups();
    return id;
  }

  async renameTagGroup(id: string, name: string): Promise<void> {
    await invoke("rename_tag_group", { id, name });
    await this.loadTagGroups();
  }

  async setTagGroupColor(id: string, color: string | null): Promise<void> {
    await invoke("set_tag_group_color", { id, color });
    await this.loadTagGroups();
  }

  /** Delete a group. Its tags survive, ungrouped (FK SET NULL). */
  async deleteTagGroup(id: string): Promise<void> {
    await invoke("delete_tag_group", { id });
    await this.loadTagGroups();
    await this.loadTags();
  }

  /**
   * Aggregate one folder's subtree. A separate call made when a folder is
   * selected — never folded into `loadFolders`, since listing N folders would
   * then run N recursive CTEs.
   */
  fetchFolderStats(folderId: string): Promise<FolderStats> {
    return invoke<FolderStats>("folder_stats", { folderId });
  }

  /**
   * Apply a partial folder update. Omitted keys are left alone; `""` clears a
   * field — see FolderPatch in assets.rs. Reloads the tree because a rename
   * changes the sibling ordering (`ORDER BY parent_id, position, name`).
   */
  async updateFolder(id: string, patch: FolderPatch): Promise<void> {
    await invoke("update_folder", { id, patch });
    await this.loadFolders();
  }

  /**
   * Apply a partial asset update, then refresh the cached row from what the DB
   * actually stored — without this, scrolling away and back would show the old
   * name, since the grid renders from `heavy`, not from the inspector.
   */
  async updateAsset(id: string, patch: AssetPatch): Promise<void> {
    const row = await invoke<AssetMetadata>("update_asset", { id, patch });
    if (this.heavy.has(id)) this.heavy.set(id, row);

    // A rename moves the row under a filename sort. Reload once, here on commit —
    // never per keystroke, which would re-stream the manifest as the user types.
    if (patch.stem !== undefined && this.sort.order_by === "filename") {
      await this.reload();
    }
  }

  /**
   * Delete a folder (cascades to subfolders + memberships; assets are kept). If
   * the active view was the deleted folder or one of its now-gone descendants,
   * fall back to the full library.
   */
  async deleteFolders(ids: string[]): Promise<void> {
    if (ids.length === 0) return;
    await invoke("delete_folders", { ids });
    await this.loadFolders();
    // Checking membership of the RELOADED list also catches descendants that
    // went with a deleted parent, which `ids` alone doesn't name.
    const active = this.scope;
    if (active.kind === "folder" && !this.folders.some((f) => f.id === active.id)) {
      await this.setScope({ kind: "all" });
    }
  }

  async moveFolder(id: string, newParentId: string | null): Promise<void> {
    await invoke("move_folder", { id, newParentId });
    await this.loadFolders();
  }

  /**
   * Place a folder under `newParentId`, immediately after `afterId`
   * (null = first among its new siblings). The drop-between-rows half of tree
   * drag & drop; `moveFolder` appends instead.
   *
   * The scope deliberately doesn't change: moving a folder rearranges the tree,
   * not what's on screen. Its assets are unaffected, so the manifest is too.
   */
  async reorderFolder(
    id: string,
    newParentId: string | null,
    afterId: string | null,
  ): Promise<void> {
    await invoke("reorder_folder", { id, newParentId, afterId });
    await this.loadFolders();
  }

  // ── Pinned folders ──────────────────────────────────────────────────────
  //
  // A pin is a shortcut, not a place: none of these touch the tree, the scope,
  // or what a folder contains. `loadFolders` is the only refresh they need,
  // because pin state rides on the folder rows the sidebar already reads.

  /**
   * The pinned list, in the user's order, across folders AND smart folders.
   *
   * Loaded rather than derived: it spans two tables, and merging them here would
   * mean re-implementing the shared rank comparison the backend already does.
   */
  pins = $state<PinnedItem[]>([]);

  async loadPins(): Promise<void> {
    try {
      this.pins = await invoke<PinnedItem[]>("fetch_pins");
    } catch (e) {
      console.error("Failed to load pins:", e);
      this.pins = [];
    }
  }

  async setPinned(kind: PinKind, id: string, pinned: boolean): Promise<void> {
    await invoke("set_pinned", { kind, id, pinned });
    await this.loadPins();
    // The tree's pin badges and the smart folder list read from their own
    // caches, so both need to hear about it.
    if (kind === "folder") await this.loadFolders();
    else await this.loadSmartFolders();
  }

  /** Drag-to-reorder across the whole pinned list. A null `after` means first. */
  async reorderPin(
    kind: PinKind,
    id: string,
    after: { kind: PinKind; id: string } | null,
  ): Promise<void> {
    await invoke("reorder_pin", {
      kind,
      id,
      afterKind: after?.kind ?? null,
      afterId: after?.id ?? null,
    });
    await this.loadPins();
  }

  /** Set or clear a pin's accent. `null` clears it. */
  async setPinColor(kind: PinKind, id: string, color: PinColor | null): Promise<void> {
    await invoke("set_pin_color", { kind, id, color });
    await this.loadPins();
    if (kind === "folder") await this.loadFolders();
    else await this.loadSmartFolders();
  }

  /** A few of a rule set's current matches, for the sidebar preview. */
  previewMatches(rules: RuleNode, limit = 9): Promise<AssetLightRow[]> {
    return invoke<AssetLightRow[]>("preview_matches", { rules, limit });
  }

  // ── Quick actions ─────────────────────────────────────────────────────────

  quickActions = $state<QuickAction[]>([]);
  /** Run history, newest first. Backs the menu's "Undo" entry. */
  actionRuns = $state<ActionRun[]>([]);

  /** tag id -> name, for rendering steps that reference tags by id. */
  get tagNames(): ReadonlyMap<string, string> {
    return new Map(this.tags.map((t) => [t.id, t.name]));
  }

  async loadQuickActions(): Promise<void> {
    try {
      this.quickActions = await invoke<QuickAction[]>("fetch_quick_actions");
    } catch (e) {
      console.error("Failed to load quick actions:", e);
      this.quickActions = [];
    }
  }

  async loadActionRuns(): Promise<void> {
    try {
      this.actionRuns = await invoke<ActionRun[]>("fetch_action_runs");
    } catch (e) {
      console.error("Failed to load the run history:", e);
      this.actionRuns = [];
    }
  }

  async createQuickAction(draft: QuickActionDraft): Promise<void> {
    await invoke<QuickAction>("create_quick_action", { draft });
    await this.loadQuickActions();
  }

  async updateQuickAction(id: string, draft: QuickActionDraft): Promise<void> {
    await invoke("update_quick_action", { id, draft });
    await this.loadQuickActions();
  }

  async deleteQuickAction(id: string): Promise<void> {
    await invoke("delete_quick_action", { id });
    await this.loadQuickActions();
    // A run outlives the action that produced it (ON DELETE SET NULL), so the
    // history is still there and still undoable — but its `action_id` changed.
    await this.loadActionRuns();
  }

  /** The dry run behind the confirmation dialog. Never mutates. */
  previewActionRun(actionId: string, assetIds: string[]): Promise<RunPreview> {
    return invoke<RunPreview>("preview_action_run", { actionId, assetIds });
  }

  /**
   * Render a rename step against real assets, for the editor's pattern box.
   *
   * Safe to call on every keystroke: a half-typed pattern comes back as
   * `error`, not as a rejected promise.
   */
  previewRename(step: Step, assetIds: string[], limit = 3): Promise<RenamePreview> {
    return invoke<RenamePreview>("preview_rename", { step, assetIds, limit });
  }

  /**
   * Apply an action to a selection snapshot.
   *
   * `assetIds` is passed in by the caller and never read from `selection` here:
   * the ids must be the ones the user saw when they triggered the run, not the
   * ones that survive whatever the run itself does to the current view.
   */
  async runQuickAction(actionId: string, assetIds: string[]): Promise<RunSummary> {
    const summary = await invoke<RunSummary>("run_quick_action", { actionId, assetIds });
    await this.#afterActionWrite();
    return summary;
  }

  async undoActionRun(runId: string): Promise<UndoSummary> {
    const summary = await invoke<UndoSummary>("undo_action_run", { runId });
    await this.#afterActionWrite();
    return summary;
  }

  /**
   * Refresh what a run can have changed.
   *
   * A step can now move assets between folders, so the manifest is re-streamed
   * whenever the current view is anything NARROWER than the whole library — a
   * folder can lose members, Uncategorized can gain them, and a smart folder's
   * rules can reference the tags just edited. In unfiltered "All" nothing can
   * leave the view, so the common "tag while browsing everything" path still
   * doesn't re-stream a 100k-asset library.
   */
  async #afterActionWrite(): Promise<void> {
    await this.loadTags();
    await this.loadActionRuns();
    if (this.scope.kind !== "all" || this.hasFilters) await this.reload();
  }

  /** Add assets to a folder; reload the manifest if the change affects the view. */
  async addAssetsToFolder(folderId: string, assetIds: string[]): Promise<void> {
    await invoke("add_assets_to_folder", { folderId, assetIds });
    const active = this.scope;
    if (active.kind === "uncategorized" || (active.kind === "folder" && active.id === folderId)) {
      await this.reload();
    }
  }

  /**
   * Move assets from one folder to another in a single transaction.
   *
   * `sourceFolderId` is null outside a folder scope ("All", "Uncategorized"),
   * where there is no membership to move FROM and this degrades to an add.
   *
   * Reloads whenever either end of the move is what's on screen — the source
   * because rows left it, the target because rows arrived, and "uncategorized"
   * because gaining a first membership is exactly what removes an asset from it.
   */
  async moveAssetsToFolder(
    sourceFolderId: string | null,
    targetFolderId: string,
    assetIds: string[],
  ): Promise<void> {
    await invoke("move_assets_to_folder", { sourceFolderId, targetFolderId, assetIds });
    const active = this.scope;
    if (
      active.kind === "uncategorized" ||
      (active.kind === "folder" && (active.id === targetFolderId || active.id === sourceFolderId))
    ) {
      await this.reload();
    }
  }

  async removeAssetsFromFolder(folderId: string, assetIds: string[]): Promise<void> {
    await invoke("remove_assets_from_folder", { folderId, assetIds });
    const active = this.scope;
    // "uncategorized" too: dropping an asset's last membership is exactly what
    // makes it appear there.
    if (active.kind === "uncategorized" || (active.kind === "folder" && active.id === folderId)) {
      await this.reload();
    }
  }

  /**
   * Rebuild every thumbnail in the active library with `mode` (clears the cache,
   * then regenerates). Resolves with the count once done; rows are patched in
   * place by the `thumbnail-progress` listener as batches complete, so no reload
   * is needed. Regenerated files keep the same `id.webp` path, so `thumbVersion`
   * is bumped afterward to bust the webview's image cache (see AssetCard).
   */
  async rebuildThumbnails(mode: string, quality: number): Promise<number> {
    const count = await invoke<number>("rebuild_thumbnails", { settings: { mode, quality } });
    // Files were rewritten in place under the same id.webp paths — bump the
    // version so on-screen thumbnails refetch instead of showing the cache.
    this.thumbVersion++;
    return count;
  }

  /**
   * On-view thumbnail generation: request thumbnails for the given ids (the
   * caller passes the current visible window, images still missing one). The
   * window is pushed to the FRONT of the queue (so it generates first), then the
   * queue is drained by `#pumpThumbnails` with up to `#THUMB_CONCURRENCY` batches
   * in flight — full throughput without oversubscribing SQLite. Ids already queued
   * or in flight are skipped; the backend also filters `WHERE thumb_hash IS NULL`,
   * so this stays idempotent. `thumbnail-progress` patches rows in via
   * `applyThumbnails`. Non-fatal on failure.
   */
  ensureThumbnails(ids: string[], mode: string, quality: number): void {
    this.#thumbMode = mode;
    this.#thumbQuality = quality;
    // Current window to the front; dedup against what's already queued/in flight.
    const queued = new Set(this.#thumbQueue);
    const fresh = ids.filter((id) => !this.#thumbRequested.has(id) && !queued.has(id));
    if (fresh.length) this.#thumbQueue = [...fresh, ...this.#thumbQueue];
    this.#pumpThumbnails();
  }

  /** Drain the generation queue, keeping up to `#THUMB_CONCURRENCY` in flight. */
  #pumpThumbnails(): void {
    const MAX = AssetLibrary.#THUMB_CONCURRENCY;
    const SIZE = AssetLibrary.#THUMB_BATCH;
    while (this.#thumbInFlight < MAX && this.#thumbQueue.length > 0) {
      // Guarded by the LIBRARY token: a sort change reorders the same assets, so
      // generation in flight is still wanted and must keep draining.
      const token = this.#libraryToken;
      const batch = this.#thumbQueue.splice(0, SIZE);
      batch.forEach((id) => this.#thumbRequested.add(id));
      this.#thumbInFlight++;
      invoke<number>("generate_thumbnails_for_ids", {
        ids: batch,
        settings: { mode: this.#thumbMode, quality: this.#thumbQuality },
      })
        .catch((e) => console.error("Thumbnail generation request failed:", e))
        .finally(() => {
          batch.forEach((id) => this.#thumbRequested.delete(id));
          this.#thumbInFlight--;
          // Keep draining, unless a library switch superseded this run.
          if (token === this.#libraryToken) this.#pumpThumbnails();
        });
    }
  }

  /**
   * Patch freshly-generated thumbnails into their rows in place — O(batch), no
   * manifest reload (which would flash the grid). Sets `thumb_hash` so the
   * ThumbHash placeholder appears, and, if the heavy row is cached, updates its
   * `thumb_path` so the real thumbnail loads immediately (no re-fetch needed).
   * Uncached rows pick up `thumb_path` on their next hydration.
   */
  /**
   * Feed a `thumbnail-progress` event to the UI indicator. Ignores small
   * single-chunk (≤64) runs — those are on-view batches that finish instantly —
   * and auto-hides shortly after completion (a fresh batch cancels the hide).
   */
  reportThumbProgress(current: number, total: number): void {
    if (total <= 64) return; // on-view batch; not worth a progress bar
    clearTimeout(this.#thumbProgressHideTimer);
    this.thumbProgress = { current, total };
    if (current >= total) {
      this.#thumbProgressHideTimer = setTimeout(() => {
        this.thumbProgress = null;
      }, 800);
    }
  }

  applyThumbnails(ready: { id: string; thumb_hash: string; thumb_path: string }[]): void {
    for (const r of ready) {
      const idx = this.#indexById.get(r.id);
      if (idx !== undefined) this.manifest[idx].thumb_hash = r.thumb_hash; // deep-reactive
      const heavy = this.heavy.get(r.id);
      if (heavy) {
        this.heavy.set(r.id, { ...heavy, thumb_hash: r.thumb_hash, thumb_path: r.thumb_path });
      }
    }
  }

  /** Hydrate heavy rows for the given ids (visible window + overscan). */
    async ensure(ids: string[]): Promise<void> {
      // Library token, not load token: heavy rows survive a scope/sort change.
      const token = this.#libraryToken;
      const missing = ids.filter((id) => !this.heavy.has(id) && !this.#pending.has(id));
      if (missing.length) {
        missing.forEach((id) => this.#pending.add(id));
        try {
          const rows = await invoke<AssetMetadata[]>("fetch_assets_by_ids", { ids: missing });
          // A library switch may have superseded this request; dropping the stale
          // rows avoids polluting the new library's cache (T1.2).
          if (token !== this.#libraryToken) return;
          for (const row of rows) this.heavy.set(row.id, row);
        } catch (e) {
          console.error("Asset hydration failed:", e);
        } finally {
          missing.forEach((id) => this.#pending.delete(id));
        }
      }
      if (token === this.#libraryToken) this.#evict(ids);
    }

    #evict(keep: string[]): void {
      if (this.heavy.size <= MAX_HEAVY) return;
      const keepSet = new Set(keep);
      for (const id of this.heavy.keys()) {
        // SvelteMap iterates in insertion order → oldest, non-visible first.
        if (this.heavy.size <= MAX_HEAVY) break;
        if (!keepSet.has(id)) this.heavy.delete(id);
      }
    }
  }

  export const assetLibrary = new AssetLibrary();
