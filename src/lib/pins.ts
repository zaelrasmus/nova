import type { PinColor } from "./assets.svelte";

/**
 * Resolve a pin's accent token to its CSS variable.
 *
 * The database stores the token name ('blue'), never a colour value, so this is
 * the single place a token becomes something a browser can paint — retinting for
 * a light theme means editing `--pin-*` in layout.css and nothing else.
 *
 * An undyed pin is deliberately still legible: it falls back to a neutral grey
 * and leans on its position and tooltip, rather than disappearing.
 */
export function pinColorVar(color: PinColor | null): string {
    return color ? `var(--pin-${color})` : "var(--pin-none)";
}
