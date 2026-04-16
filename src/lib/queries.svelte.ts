import { createQuery } from "@tanstack/svelte-query";
import { invoke } from "@tauri-apps/api/core";
import { libraryManager } from "../routes/settings.svelte";

export function useAssets() {
  return createQuery(() => ({
    queryKey: ["assets", libraryManager.state.activeLibrary],
    queryFn: async () => {
      console.log("Fetcheando assets para:", libraryManager.state.activeLibrary);
      return await invoke("fetch_assets");
    },
    enabled: !!libraryManager.state.activeLibrary,
  }));
}
