/**
 * Assemble the Chromium build into `dist/chromium/`.
 *
 * The two families share every line of JS, CSS and HTML — only the manifest
 * differs, and only in two places: how the background is declared (service
 * worker vs event page) and the `key` that pins the extension id.
 *
 * Kept as a copy step rather than a bundler because there is nothing to bundle.
 * It also means `extension/` stays directly loadable in Firefox, which is what
 * `about:debugging` points at.
 *
 *   node build-chromium.mjs
 */

import { cp, mkdir, rm, readFile, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const out = join(here, "dist", "chromium");

/** Everything the extension needs at runtime. The manifest is handled separately. */
const SHARED = ["background.js", "popup.html", "popup.js", "popup.css", "icons"];

await rm(out, { recursive: true, force: true });
await mkdir(out, { recursive: true });

for (const entry of SHARED) {
  await cp(join(here, entry), join(out, entry), { recursive: true });
}

const manifest = await readFile(join(here, "manifest.chromium.json"), "utf8");
await writeFile(join(out, "manifest.json"), manifest);

const { key } = JSON.parse(manifest);
console.log(`Built ${out}`);
console.log(key ? "Extension id is pinned by the manifest key." : "WARNING: no key — the id will float.");
console.log("Load it with chrome://extensions → Developer mode → Load unpacked.");
