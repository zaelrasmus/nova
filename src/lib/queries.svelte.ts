// DEAD CODE — safe to delete this whole file.
//
// Left over from an early svelte-query experiment, superseded by the streaming
// manifest in `assets.svelte.ts`. Two independent reasons it cannot work:
//
//   * nothing imports `useAssets`;
//   * `fetch_assets` is no longer registered in `lib.rs`'s `generate_handler!`,
//     so calling it would reject at runtime with "command not found".
//
// Not marked ANTICIPATED/FUTURE and documents no planned design — this is
// accidental cruft, not scaffolding. Deleting it also drops the last use of
// @tanstack/svelte-query, which could then leave package.json.

import { createQuery } from "@tanstack/svelte-query";
import { invoke } from "@tauri-apps/api/core";
import { libraryManager } from "../routes/settings.svelte";

export function useAssets() {
  return createQuery(() => ({
    queryKey: ["assets", libraryManager.state.activeLibrary],
    queryFn: async () =>  await invoke("fetch_assets"),
    enabled: !!libraryManager.state.activeLibrary,
  }));
}
