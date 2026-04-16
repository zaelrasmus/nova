<script lang="ts">
    import { useAssets } from "$lib/queries.svelte";
    import { convertFileSrc } from "@tauri-apps/api/core";
    import { libraryManager } from "../routes/settings.svelte";

    import { join, dirname } from "@tauri-apps/api/path";

    interface Asset {
        id: string;
        filename: string;
    }

    const assetsQuery = useAssets();

    const assetsData = $derived((assetsQuery.data as any[]) ?? []);
    const topAssets = $derived(assetsData.slice(0, 10));
    const totalCount = $derived(assetsData.length);

    // Estado para la ruta base física
    let baseDir = $state("");

    // Reaccionamos al cambio de librería para calcular el directorio padre
    $effect(() => {
        if (libraryManager.state.activeLibrary) {
            dirname(libraryManager.state.activeLibrary).then((path) => {
                baseDir = path;
            });
        }
    });
</script>

<div class="p-6">
    <h2>Assets: {totalCount}</h2>

    {#if assetsQuery.isLoading}
        <p>Cargando assets desde SQLite...</p>
    {:else if assetsQuery.isError}
        <p class="text-red-500">Error: {assetsQuery.error.message}</p>
    {:else}
        <div class="grid grid-cols-5 gap-4">
            {#each topAssets as asset (asset.id)}
                <div class="p-2 border border-gray-700 rounded">
                    <p class="truncate text-xs">{asset.filename}</p>
                </div>
            {/each}
        </div>

        {#if topAssets.length === 0 && !assetsQuery.isLoading}
            <p class="text-gray-500">No se encontraron imágenes.</p>
        {/if}
    {/if}
</div>
<h3>Mostrando las imagenes</h3>
<div class="grid grid-cols-4 gap-4 mt-10">
    {#each topAssets as asset (asset.id)}
        <div
            class="aspect-square bg-gray-900 rounded overflow-hidden border text-white border-gray-800"
        >
            {#if baseDir}
                <img
                    src={convertFileSrc(asset.dest_path)}
                    alt={asset.filename}
                    class="w-full h-full object-cover"
                    loading="lazy"
                />
                {console.log(asset.dest_path)}
            {:else}
                <div class="animate-pulse bg-gray-800 w-full h-full"></div>
            {/if}
        </div>
    {/each}
</div>
