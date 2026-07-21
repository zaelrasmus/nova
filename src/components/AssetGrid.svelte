<script lang="ts">
    import { createVirtualizer } from "@tanstack/svelte-virtual";
    import AssetCard from "./AssetCard.svelte";    import {get} from "svelte/store";
    import { libraryManager } from "../routes/settings.svelte";
    import { assetLibrary } from "$lib/assets.svelte";


    // Manifest = layout source of truth (id, width, height, asset_type).
    const assets = $derived(assetLibrary.manifest);

    // User-controlled. The slider writes here
    let numColumns = $state(4);

    // Scroll container - passed to the virtualizer's getScrollElement
    let scrollEl = $state<HTMLDivElement | null>(null);

    // Measured container width, updated by ResizeObserver
    // Starts at 0 to supress layout until the first measurement.
    let containerWidth = $state(0);

    const GAP = 10; // px, gap between items in the grid


    $effect(() => {
      if (libraryManager.state.activeLibrary) {
        assetLibrary.load();
      }
    })

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

    const virtualizer = createVirtualizer<HTMLDivElement, HTMLDivElement>({
      count: assets.length,
      getScrollElement: () => scrollEl,
      lanes: numColumns,
      estimateSize: (index) => {
        const a = assets[index];
        const ratio = a?.width && a?.height ? a.width / a.height : 1;
        return columnWidth / ratio + GAP;
      },
      overscan: 6,
      getItemKey: (index) => assets[index]?.id ?? index,
    })

    $effect(() => {
      const count = assets.length;
      const lanes = numColumns;
      void columnWidth;
      void scrollEl;
      const instance = get(virtualizer);
      instance.setOptions({ count, lanes});
      instance.measure();
    })

    // Hydrate heavy rows for the visible window (+overscan). Deduped/cached by
       // the store, so this fires cheaply on every scroll tick.
       $effect(() => {
           const ids = $virtualizer
               .getVirtualItems()
               .map((item) => assets[item.index]?.id)
               .filter((id): id is string => !!id);
           // if (ids.length) assetLibrary.ensure(ids);
           const timer = setTimeout(() => {
                      if (ids.length) assetLibrary.ensure(ids);
                  }, 100);
                  return () => clearTimeout(timer);
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


<div class="flex flex-col h-full">
    <div class="flex items-center justify-between px-4 py-2 border-b border-neutral-800 bg-white shrink-0">
        <span class="text-xs text-neutral-400">{assets.length} assets</span>
        <div class="flex items-center gap-2">
            <span class="text-xs text-neutral-500">Columns</span>
            <input type="range" min="2" max="8" step="1" bind:value={numColumns}
                class="w-24 accent-neutral-400" />
            <span class="text-xs text-neutral-400 w-3 text-center">{numColumns}</span>
        </div>
    </div>

    {#if assetLibrary.isLoading && assets.length === 0}
            <div class="flex items-center justify-center h-32 text-sm text-neutral-500">Loading assets...</div>
        {:else if assetLibrary.error}
            <div class="flex items-center justify-center h-32 text-sm text-red-400">{assetLibrary.error}</div>
        {:else if assets.length === 0}
            <div class="flex items-center justify-center h-32 text-sm text-neutral-500">No assets in this library yet.</div>
        {:else}
            <div
                bind:this={scrollEl}
                class="relative flex-1 min-h-0 overflow-y-auto w-full
                       [scrollbar-width:thin] [scrollbar-color:theme(colors.neutral.700)_transparent] bg-white"
            >
                <div style="position: relative; width: 100%; height: {$virtualizer.getTotalSize()}px;">
                    {#each $virtualizer.getVirtualItems() as item (item.key)}

                        {@const light = assets[item.index]}
                        {@const heavy = assetLibrary.heavy.get(light.id)}
                        <AssetCard
                            {heavy}
                            style="width: {columnWidth}px; height: {item.size - GAP}px; left: {item.lane * (columnWidth + GAP)}px; transform: translateY({item.start}px);"
                            onClick={() => console.log("Selected asset:", light.id)}
                        />
                    {/each}
                </div>
            </div>
        {/if}
    </div>
