<script lang="ts">
    import { useAssets } from "$lib/queries.svelte";
    import { convertFileSrc } from "@tauri-apps/api/core";
    import { libraryManager } from "../routes/settings.svelte";
    import { createVirtualizer } from "@tanstack/svelte-virtual";
    import AssetCard from "./AssetCard.svelte";

    import { join, dirname } from "@tauri-apps/api/path";

    interface AssetMetadata {
        id: string;
        asset_type: "image" | "audio" | "video" | "unknown";
        filename: string;
        extension: string;
        dest_path: string;
        imported_date: string;
        creation_date: string;
        modified_date: string;

        // TODO: Add to the SELECT query in assets.rs once ready.
        // Required for accurate masonry height estimation in AssetGrid.
        width?: number;
        height?: number;
    }

    const assetsQuery = useAssets();

    const datas = $derived((assetsQuery.data as any[]) ?? []);

    const assets = $derived(datas.slice(0, 500));

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

    // User-controlled. The slider writes here
    let numColumns = $state(4);

    // Scroll container - passed to the virtualizer's getScrollElement
    let scrollEl = $state<HTMLDivElement | null>(null);

    // Measured container width, updated by ResizeObserver
    // Starts at 0 to supress layout until the first measurement.
    let containerWidth = $state(0);

    const GAP = 10; // px, gap between items in the grid

    // Measures the scroll container width reactively so the virtualizer
    // can recalculate dimension on window/panel resize.
    $effect(() => {
        if (!scrollEl) return;

        const observer = new ResizeObserver(([entry]) => {
            containerWidth = entry.contentRect.width;
        });

        observer.observe(scrollEl);
        return () => observer.disconnect();
    });

    const columnWidth = $derived(
        containerWidth > 0 ? (containerWidth - GAP * (numColumns - 1)) / numColumns : 200, // safe fallback before first measuremnt
    );

    const virtualizer = $derived.by(() => {
        return createVirtualizer<HTMLDivElement, HTMLDivElement>({
            count: assets.length,
            getScrollElement: () => scrollEl,
            lanes: numColumns,
            estimateSize: (index) => {
                const asset = assets[index];

                // TODO: uses asset.width / asset.height
                // Uses a default 1:1 ratio

                const aspectRatio =
                    asset.width && asset.height ? asset.width / asset.height : 1;

                return columnWidth / aspectRatio + GAP;
            },
            overscan: 3,
            getItemKey: (index) => assets[index].id,
        });
    });

    // ── FUTURE: Pragmatic Drag and Drop (Atlassian) ───────────────────────────
    // When you're ready to implement drag-to-reorder:
    //
    // 2. In each AssetCard, attach draggable() with { assetId: asset.id }
    // 3. Here, attach a dropTargetForElements() on the scrollEl
    // 4. On drop, invoke a Tauri command to persist the new order in
    //    the folder_assets.position column (already in your schema).
    //
    // The virtualizer handles rendering — DnD only updates the data order.
    // ────────────────────────────────────────────────────────────────────────
</script>

<!-- ── Toolbar ──────────────────────────────────────────────────────────────
     Column slider: controls numColumns, which drives the masonry layout.
     Min 2, max 8 — same range as Eagle.
──────────────────────────────────────────────────────────────────────────── -->
<div class="flex items-center justify-between px-4 py-2 border-b border-neutral-800 bg-white">
    <span class="text-xs text-neutral-400">
        {assets.length} assets
    </span>

    <div class="flex items-center gap-2">
        <span class="text-xs text-neutral-500">Columns</span>
        <input
            type="range"
            min="2"
            max="8"
            step="1"
            defaultValue="4"
            bind:value={numColumns}
            class="w-24 accent-neutral-400"
        />
        <span class="text-xs text-neutral-400 w-3 text-center">{numColumns}</span>
    </div>
</div>

<!-- ── Loading / error states ─────────────────────────────────────────────── -->
{#if assetsQuery.isLoading}
    <div class="flex items-center justify-center h-32 text-sm text-neutral-500">
        Loading assets...
    </div>
{:else if assetsQuery.isError}
    <div class="flex items-center justify-center h-32 text-sm text-red-400">
        Failed to load assets.
    </div>
{:else if assets.length === 0}
    <div class="flex items-center justify-center h-32 text-sm text-neutral-500">
        No assets in this library yet.
    </div>
{:else}
    {console.log(assets.slice(0, 10))}
    <div
        bind:this={scrollEl}
        class="relative overflow-y-auto w-full h-full
           [scrollbar-width:thin] [scrollbar-color:theme(colors.neutral.700)_transparent] bg-white"
    >
        <!--
      TanStack Virtual standard pattern:
      - Outer div: total virtual height (so the scrollbar thumb is sized correctly)
      - Inner items: absolutely positioned with translateY from the virtualizer
    -->
        <div
            style="background-color: white; width: full; height: {$virtualizer.getTotalSize()}px; position: relative;"
        >
            {#each $virtualizer.getVirtualItems() as item (item.key)}
                {@const asset = assets[item.index]}

                <AssetCard
                    {asset}
                    style="
            width: {columnWidth}px;
            height: {item.size - GAP}px;
            left: {item.lane * (columnWidth + GAP)}px;
            transform: translateY({item.start}px);
          "
                    onClick={() => {
                        // TODO: open asset inspector / viewer
                        console.log("Selected asset:", asset.id);
                    }}
                />
            {/each}
        </div>
    </div>
{/if}
