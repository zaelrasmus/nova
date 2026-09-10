/**
 * Stage `nova-connector` as a Tauri sidecar so it ships inside the installer.
 *
 * Without this the connector only exists in `target/`, which is fine on a
 * developer's machine and useless everywhere else: a tester installing the MSI
 * or NSIS package would get Nova with no launcher, so the extension could talk
 * to a running Nova but never start one.
 *
 * Runs from `beforeBuildCommand`, before Nova is compiled at all. tauri-build
 * validates that declared `externalBin` files EXIST every time Nova's build
 * script runs — which is why the connector is its OWN crate: `-p nova-connector`
 * never triggers that build script, so the sidecar can be produced before
 * anything demands it. As a second binary in Nova's package this was a circle
 * with no entry point.
 *
 * Sidecars must carry the target triple in their filename. Tauri strips it again
 * when installing, so the connector lands beside `nova.exe` — which is exactly
 * where it looks for Nova.
 */

import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const tauriDir = join(root, "src-tauri");

/** Ask rustc rather than hard-coding: this has to be right on every machine. */
function hostTriple() {
  const out = execFileSync("rustc", ["-vV"], { encoding: "utf8" });
  const match = out.match(/^host:\s*(\S+)$/m);
  if (!match) throw new Error("Could not determine the host target triple from `rustc -vV`");
  return match[1];
}

const triple = hostTriple();
const exe = process.platform === "win32" ? ".exe" : "";

console.log(`[connector] building for ${triple}`);
execFileSync("cargo", ["build", "--release", "-p", "nova-connector"], {
  cwd: tauriDir,
  stdio: "inherit",
});

const built = join(tauriDir, "target", "release", `nova-connector${exe}`);
const outDir = join(tauriDir, "binaries");
const staged = join(outDir, `nova-connector-${triple}${exe}`);

mkdirSync(outDir, { recursive: true });
copyFileSync(built, staged);
console.log(`[connector] staged ${staged}`);
