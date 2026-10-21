/**
 * Appearance — the user-tunable half of the look (Settings › Appearance).
 *
 * The design itself lives in layout.css as `--nv-*` tokens; this module only
 * decides the handful of values a user may change, and writes them onto <html>
 * as custom properties and data attributes. Components never read these
 * preferences to style themselves: they use the Tailwind colours the tokens
 * back (`bg-brand`, `bg-pane`, `border-line`…), so a change here repaints the
 * whole app without a single component re-rendering.
 *
 * The exceptions are values that feed LAYOUT MATH rather than paint — the grid
 * gap and margins — which AssetGrid also reads as numbers, because a CSS
 * variable can't tell the virtualizer how wide a column is.
 */

/**
 * The accent marks exactly three things: selected, on, and "you are here".
 * Stored by NAME, never by value, so a palette tweak here reaches every install.
 */
export const ACCENTS = {
    indigo: { label: "Indigo", value: "oklch(0.68 0.16 277)" },
    blue: { label: "Blue", value: "oklch(0.62 0.19 259)" },
    violet: { label: "Violet", value: "oklch(0.68 0.18 305)" },
    teal: { label: "Teal", value: "oklch(0.72 0.12 195)" },
    amber: { label: "Amber", value: "oklch(0.78 0.15 70)" },
    rose: { label: "Rose", value: "oklch(0.68 0.19 12)" },
    mono: { label: "Mono", value: "oklch(0.86 0 0)" },
} as const;

export type AccentName = keyof typeof ACCENTS;

export interface AppearancePreferences {
    accent: AccentName;
    /** Layered: sidebar and inspector sit a shade above the grid. Flat: one tone. */
    paneTone: "layered" | "flat";
    /** Space between the grid and the pane edges, left/right (px). */
    gridMarginX: number;
    /** Space between the grid and the pane edges, top/bottom (px). */
    gridMarginY: number;
    /** Space between tiles (px). Also feeds the layout math in AssetGrid. */
    gridGap: number;
    tileRadius: number;
    /** Name and dimensions over a tile: never, on hover, or always. */
    tileInfo: "off" | "hover" | "always";
    /** The corner circle that appears on hover and toggles a tile in/out of the selection. */
    selectCircle: boolean;
    density: "comfortable" | "compact";
}

export const APPEARANCE_DEFAULTS: AppearancePreferences = {
    accent: "indigo",
    paneTone: "layered",
    gridMarginX: 20,
    gridMarginY: 20,
    gridGap: 8,
    tileRadius: 8,
    tileInfo: "hover",
    selectCircle: true,
    density: "comfortable",
};

/**
 * Slider bounds, shared by the settings UI and the clamp below. Margins start
 * at 4px, not 0: the selection ring is drawn 4px outside a tile, and the grid's
 * scroll container would clip it at the pane edge.
 */
export const APPEARANCE_LIMITS = {
    gridMarginX: { min: 4, max: 48 },
    gridMarginY: { min: 4, max: 48 },
    gridGap: { min: 2, max: 20 },
    tileRadius: { min: 0, max: 16 },
} as const;

type NumericKey = keyof typeof APPEARANCE_LIMITS;

/**
 * A number from settings.json, made safe to lay out with. The file is
 * user-editable, and a NaN gap would collapse every column to zero width.
 */
export function clampAppearance(key: NumericKey, value: unknown): number {
    const { min, max } = APPEARANCE_LIMITS[key];
    const n = typeof value === "number" && Number.isFinite(value) ? value : APPEARANCE_DEFAULTS[key];
    return Math.min(max, Math.max(min, Math.round(n)));
}

/**
 * Paint the preferences onto <html>. Through the CSSOM rather than a style
 * attribute: the CSP has no `style-src`, and `style.setProperty` is not an
 * inline style as far as that policy is concerned.
 */
export function applyAppearance(p: AppearancePreferences, root: HTMLElement = document.documentElement) {
    const s = root.style;
    s.setProperty("--nv-accent", (ACCENTS[p.accent] ?? ACCENTS[APPEARANCE_DEFAULTS.accent]).value);
    s.setProperty("--nv-grid-mx", `${clampAppearance("gridMarginX", p.gridMarginX)}px`);
    s.setProperty("--nv-grid-my", `${clampAppearance("gridMarginY", p.gridMarginY)}px`);
    s.setProperty("--nv-grid-gap", `${clampAppearance("gridGap", p.gridGap)}px`);
    s.setProperty("--nv-tile-radius", `${clampAppearance("tileRadius", p.tileRadius)}px`);
    root.dataset.panes = p.paneTone === "flat" ? "flat" : "layered";
    root.dataset.density = p.density === "compact" ? "compact" : "comfortable";
    root.dataset.tileInfo = p.tileInfo;
    root.dataset.selectCircle = p.selectCircle ? "on" : "off";
}
