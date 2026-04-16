<script lang="ts">
    import { useAssets } from "$lib/queries.svelte";
    import { convertFileSrc } from "@tauri-apps/api/core";

    interface Asset {
        id: string;
        filename: string;
    }

    const assetsQuery = useAssets();

    const assetsData = $derived((assetsQuery.data as any[]) ?? []);
    const topAssets = $derived(assetsData.slice(0, 10));
    const totalCount = $derived(assetsData.length);
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
