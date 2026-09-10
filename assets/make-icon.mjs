/**
 * Generates `assets/icon.svg`, the single source every Nova icon is built from.
 *
 * Written as a generator rather than hand-authored SVG because the artwork is
 * radial: sixteen vertices whose coordinates are trigonometry, and a hand-typed
 * approximation of them looks subtly lopsided at 1024px in a way that is
 * miserable to correct by nudging numbers.
 *
 * Design constraints, in priority order:
 *
 *  1. Legible at 16px. That is the taskbar and the favicon, and it is where
 *     most icons die. The core disc carries the mark at that size; the rays are
 *     texture that only resolves once it is bigger.
 *  2. Not the four-pointed AI sparkle. That shape now reads as "generative
 *     feature button" (Copilot, Gemini, every toolbar in 2026), which is the
 *     wrong promise for a media library. Eight rays with a defined circular
 *     core reads astronomical instead - which is what a nova is.
 *  3. Nova's own palette. Blue-400 through blue-600 on near-black, the same
 *     range the app already uses for accents.
 */

import { execFileSync } from "node:child_process";
import { copyFileSync, rmSync, writeFileSync } from "node:fs";

const SIZE = 1024;
const C = SIZE / 2;

// Ray geometry. Long rays on the cardinal axes, short on the diagonals: the
// cardinal cross is what survives downscaling, so it gets the reach.
const R_LONG = 452;
const R_SHORT = 246;
// Where the rays meet. Larger = fatter, blunter rays. 150 is the point where
// the cardinal rays are still ~11% of the canvas wide at the base, which is
// about 1.8px at 16px - thin, but not gone.
const R_INNER = 150;
const R_CORE = 104;

const rad = (deg) => ((deg - 90) * Math.PI) / 180;
const pt = (deg, r) => [C + Math.cos(rad(deg)) * r, C + Math.sin(rad(deg)) * r];
const fmt = (n) => Number(n.toFixed(2));

const points = [];
for (let i = 0; i < 8; i++) {
  points.push(pt(i * 45, i % 2 === 0 ? R_LONG : R_SHORT));
  points.push(pt(i * 45 + 22.5, R_INNER));
}
const star = points.map(([x, y], i) => `${i ? "L" : "M"}${fmt(x)},${fmt(y)}`).join("") + "Z";

// 22.5% corner radius is the platform convention for a rounded-square app icon
// and is what keeps it from reading as a web favicon dropped into a dock.
const RADIUS = Math.round(SIZE * 0.225);

const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${SIZE}" height="${SIZE}" viewBox="0 0 ${SIZE} ${SIZE}">
  <defs>
    <linearGradient id="plate" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0%" stop-color="#161b26"/>
      <stop offset="100%" stop-color="#080a10"/>
    </linearGradient>
    <radialGradient id="halo" cx="50%" cy="50%" r="50%">
      <stop offset="0%" stop-color="#3b82f6" stop-opacity="0.7"/>
      <stop offset="55%" stop-color="#2563eb" stop-opacity="0.22"/>
      <stop offset="100%" stop-color="#2563eb" stop-opacity="0"/>
    </radialGradient>
    <radialGradient id="ray" cx="50%" cy="50%" r="50%">
      <stop offset="0%" stop-color="#ffffff"/>
      <stop offset="22%" stop-color="#dbeafe"/>
      <stop offset="55%" stop-color="#60a5fa"/>
      <stop offset="100%" stop-color="#2563eb"/>
    </radialGradient>
    <radialGradient id="core" cx="50%" cy="50%" r="50%">
      <stop offset="0%" stop-color="#ffffff"/>
      <stop offset="70%" stop-color="#ffffff"/>
      <stop offset="100%" stop-color="#e0edff"/>
    </radialGradient>
    <radialGradient id="bloom" cx="50%" cy="50%" r="50%">
      <stop offset="0%" stop-color="#ffffff" stop-opacity="0.95"/>
      <stop offset="45%" stop-color="#dbeafe" stop-opacity="0.5"/>
      <stop offset="100%" stop-color="#93c5fd" stop-opacity="0"/>
    </radialGradient>
  </defs>

  <rect width="${SIZE}" height="${SIZE}" rx="${RADIUS}" ry="${RADIUS}" fill="url(#plate)"/>
  <!-- A single hairline along the top edge. Catches the light the way a
       physical object would and stops the plate reading as flat black. -->
  <rect x="1" y="1" width="${SIZE - 2}" height="${SIZE - 2}" rx="${RADIUS - 1}" ry="${RADIUS - 1}"
        fill="none" stroke="#ffffff" stroke-opacity="0.07" stroke-width="2"/>

  <circle cx="${C}" cy="${C}" r="430" fill="url(#halo)"/>
  <path d="${star}" fill="url(#ray)"/>
  <circle cx="${C}" cy="${C}" r="205" fill="url(#bloom)"/>
  <circle cx="${C}" cy="${C}" r="${R_CORE}" fill="url(#core)"/>
</svg>
`;

const root = new URL("..", import.meta.url);
const at = (p) => new URL(p, root).pathname.replace(/^\/(?=[A-Za-z]:)/, "");

writeFileSync(at("assets/icon.svg"), svg);
console.log(`Wrote assets/icon.svg (${svg.length} bytes)`);

// The same artwork has to reach three places, and hand-copying them is how an
// app ends up with a new icon in the taskbar and the old one in the browser
// toolbar. Do the whole job or none of it.
console.log("Generating platform icons...");
execFileSync("bun", ["run", "tauri", "icon", "assets/icon.svg"], {
  cwd: at(""),
  stdio: ["ignore", "ignore", "inherit"],
  shell: process.platform === "win32",
});

// Nova is desktop-only; `tauri icon` emits the mobile sets unconditionally.
for (const dir of ["src-tauri/icons/android", "src-tauri/icons/ios"]) {
  rmSync(at(dir), { recursive: true, force: true });
}

const copies = [
  ["src-tauri/icons/128x128.png", "static/favicon.png"],
  ["src-tauri/icons/32x32.png", "extension/icons/32.png"],
  ["src-tauri/icons/128x128.png", "extension/icons/128.png"],
];
for (const [from, to] of copies) {
  copyFileSync(at(from), at(to));
  console.log(`  ${to}`);
}

console.log("\nDone. The app icon is embedded at build time - run 'bun run tauri build'.");
