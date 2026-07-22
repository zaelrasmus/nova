<script lang="ts">
    import { createVirtualizer } from "@tanstack/svelte-virtual";
    import AssetCard from "./AssetCard.svelte";    import {get} from "svelte/store";
    import { libraryManager, settings } from "../routes/settings.svelte";
    import { assetLibrary, type AssetLightRow } from "$lib/assets.svelte";


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
        // Reset to the full view + refresh the folder tree on library change (a
        // stale folder filter from a previous library would show nothing).
        // Thumbnails are generated on-view (below) as items scroll into the
        // window — no eager pass over the whole library.
        assetLibrary.setFilter({ kind: "all" });
        assetLibrary.loadFolders();
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

    // Hydrate heavy rows + generate thumbnails for the visible window (+overscan).
    //
    // CRITICAL: read the virtualizer via get(virtualizer), NOT $virtualizer.
    // Reading $virtualizer reactively inside an effect self-invalidates (calling
    // getVirtualItems() re-notifies the store, re-running the effect faster than
    // the debounce, so the timer is cleared forever and this never runs). Instead
    // we trigger off the scroll DOM event and off manifest changes.
    let hydrateTimer: ReturnType<typeof setTimeout>;
    function scheduleHydrate() {
        clearTimeout(hydrateTimer);
        hydrateTimer = setTimeout(() => {
            const rows = get(virtualizer)
                .getVirtualItems()
                .map((item) => assets[item.index])
                .filter((r): r is AssetLightRow => !!r);
            if (!rows.length) return;
            assetLibrary.ensure(rows.map((r) => r.id)); // hydrate heavy rows for the window
            // Only images still missing a thumbnail need generation.
            const needIds = rows
                .filter((r) => r.asset_type === "image" && r.thumb_hash === null)
                .map((r) => r.id);
            if (needIds.length) {
                assetLibrary.ensureThumbnails(
                    needIds,
                    settings.preferences.thumbnailQuality,
                    settings.preferences.thumbnailLossyQuality,
                );
            }
        }, 100);
    }

    // Scroll-driven: a plain DOM listener, no reactive virtualizer read.
    $effect(() => {
        const el = scrollEl;
        if (!el) return;
        const onScroll = () => scheduleHydrate();
        el.addEventListener("scroll", onScroll, { passive: true });
        return () => el.removeEventListener("scroll", onScroll);
    });

    // Manifest/layout-driven: hydrate the initial window after load/import and
    // when the column count changes (which shifts what's visible).
    $effect(() => {
        void assets.length;
        void numColumns;
        scheduleHydrate();
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
        <div class="flex items-center gap-3">
            <button
                type="button"
                onclick={() =>
                    settings.set("animateGifsInGrid", !settings.preferences.animateGifsInGrid)}
                title="Animate GIFs in the grid"
                aria-pressed={settings.preferences.animateGifsInGrid}
                class="rounded px-2 py-0.5 text-xs font-medium transition-colors
                       {settings.preferences.animateGifsInGrid
                    ? 'bg-blue-600 text-white'
                    : 'bg-neutral-200 text-neutral-500 hover:bg-neutral-300'}"
            >
                GIF
            </button>
            <div class="flex items-center gap-2">
                <span class="text-xs text-neutral-500">Columns</span>
                <input type="range" min="2" max="8" step="1" bind:value={numColumns}
                    class="w-24 accent-neutral-400" />
                <span class="text-xs text-neutral-400 w-3 text-center">{numColumns}</span>
            </div>
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
                            assetType={light.asset_type}
                            thumbHash={light.thumb_hash}
                            isAnimated={light.is_animated}
                            animate={settings.preferences.animateGifsInGrid}
                            {heavy}
                            style="width: {columnWidth}px; height: {item.size - GAP}px; left: {item.lane * (columnWidth + GAP)}px; transform: translateY({item.start}px);"
                            onClick={() => console.log("Selected asset:", light.id)}
                        />
                    {/each}
                </div>
            </div>
        {/if}
    </div>
