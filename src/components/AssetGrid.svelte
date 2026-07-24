<script lang="ts">
    import { untrack } from "svelte";
    import { createVirtualizer } from "@tanstack/svelte-virtual";
    import AssetCard from "./AssetCard.svelte";
    import SortControl from "./SortControl.svelte";
    import FilterBar from "./FilterBar.svelte";
    import {get} from "svelte/store";
    import { libraryManager, settings } from "../routes/settings.svelte";
    import { assetLibrary, type AssetLightRow } from "$lib/assets.svelte";
    import { selection } from "$lib/selection.svelte";


    // Manifest = layout source of truth (id, width, height, asset_type).
    const assets = $derived(assetLibrary.manifest);

    // User-controlled column count, persisted in preferences. Local mirror so the
    // slider drags smoothly; we persist on release (onchange). A $effect re-syncs
    // it if settings hydrate from disk after this component mounts.
    let numColumns = $state(settings.preferences.gridColumns);
    $effect(() => {
        numColumns = settings.preferences.gridColumns;
    });

    // Scroll container - passed to the virtualizer's getScrollElement
    let scrollEl = $state<HTMLDivElement | null>(null);

    // Measured container width, updated by ResizeObserver
    // Starts at 0 to supress layout until the first measurement.
    let containerWidth = $state(0);

    const GAP = 10; // px, gap between items in the grid


    $effect(() => {
      if (!libraryManager.state.activeLibrary) return;
      // The active library is the ONLY dependency here. `untrack` is load-bearing:
      // these calls both read and write assetLibrary state (setScope reads the
      // current sort, then load() writes it), so tracking them would invalidate
      // this effect from inside its own body and re-run it forever — which showed
      // up as the grid flickering between "Loading assets..." and "No assets yet"
      // while every manifest load cancelled the one before it.
      untrack(() => {
        // New library: drop every cached row (asset ids are library-scoped, so
        // carrying them over would mix two libraries), reset to the full view, and
        // refresh the folder tree — a stale folder scope would show nothing.
        // Thumbnails are generated on-view (below) as items scroll into the
        // window — no eager pass over the whole library.
        assetLibrary.clearCaches();
        assetLibrary.setScope({ kind: "all" });
        assetLibrary.loadFolders();
        assetLibrary.loadSavedFilters();
        assetLibrary.loadColorCoverage();
      });
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
    function runHydrate() {
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
    }

    let hydrateTimer: ReturnType<typeof setTimeout>;
    function scheduleHydrate(delay = 100) {
        clearTimeout(hydrateTimer);
        hydrateTimer = setTimeout(runHydrate, delay);
    }

    // Scroll-driven: a plain DOM listener, no reactive virtualizer read. We fire on
    // `scrollend` (fires the instant momentum settles) so a window fills with zero
    // wait when you stop — a debounce could only *guess* at the stop and always add
    // that lag. Nothing generates mid-scroll (placeholders only, no wasted decode).
    // Fall back to a short debounce on webviews without `scrollend`.
    $effect(() => {
        const el = scrollEl;
        if (!el) return;
        const hasScrollEnd = "onscrollend" in window;
        const onScrollEnd = () => runHydrate();
        const onScroll = () => scheduleHydrate(100);
        if (hasScrollEnd) {
            el.addEventListener("scrollend", onScrollEnd, { passive: true });
        } else {
            el.addEventListener("scroll", onScroll, { passive: true });
        }
        return () => {
            el.removeEventListener("scrollend", onScrollEnd);
            el.removeEventListener("scroll", onScroll);
        };
    });

    // Manifest/layout-driven: hydrate the initial window after load/import and
    // when the column count changes (which shifts what's visible).
    $effect(() => {
        void assets.length;
        void numColumns;
        scheduleHydrate();
    });


    // ── Selection ────────────────────────────────────────────────────────────
    // The store owns the RULES; this component owns the ORDER. Ids are read
    // lazily per interaction rather than kept in a $derived, because the manifest
    // streams in chunks — a derived id list would be rebuilt on every chunk, and
    // that's O(n²) over a large library. Clicks are rare; one map() is nothing.
    const idsNow = () => assets.map((a) => a.id);
    const mods = (e: MouseEvent | PointerEvent) => ({
        ctrl: e.ctrlKey || e.metaKey, // metaKey = Cmd on macOS
        shift: e.shiftKey,
    });

    // Ctrl+A / Escape. Window-level so they work wherever focus sits in the grid.
    $effect(() => {
        const onKey = (e: KeyboardEvent) => {
            // Never steal keys from a text field — the filter bar sits directly
            // above this grid and Ctrl+A there must still mean "select all text".
            const t = e.target as HTMLElement | null;
            if (t && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName))) return;

            if (e.key === "Escape") {
                selection.clear();
            } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "a") {
                e.preventDefault();
                // `assets` is read here, when the handler RUNS, not when this
                // effect does — so it never re-registers the listener, and
                // "select all" always means all currently *visible* rows.
                selection.selectAllAssets(idsNow());
            }
        };
        window.addEventListener("keydown", onKey);
        return () => window.removeEventListener("keydown", onKey);
    });

    // ── FUTURE: Pragmatic Drag and Drop (Atlassian) ───────────────────────────
    // Selection is already the payload: `selection.assetIds` on dragstart, plus
    // `selection.cancelPendingCollapse()` so the press that began the drag never
    // collapses a multi-selection on release.
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
        <!-- Say "of N" when filtered, so a narrowed view never looks like a small
             library. `manifest` is the filtered set, so N comes from the store. -->
        <span class="text-xs text-neutral-400">
            {assets.length} assets{assetLibrary.hasFilters ? " (filtered)" : ""}
            {#if selection.assetCount > 0}
                <span class="text-blue-600">· {selection.assetCount} selected</span>
            {/if}
        </span>
        <div class="flex items-center gap-3">
            <SortControl />
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
                    onchange={() => settings.set("gridColumns", numColumns)}
                    class="w-24 accent-neutral-400" />
                <span class="text-xs text-neutral-400 w-3 text-center">{numColumns}</span>
            </div>
        </div>
    </div>

    <div class="shrink-0">
        <FilterBar />
    </div>

    {#if assetLibrary.isLoading && assets.length === 0}
            <div class="flex items-center justify-center h-32 text-sm text-neutral-500">Loading assets...</div>
        {:else if assetLibrary.error}
            <div class="flex items-center justify-center h-32 text-sm text-red-400">{assetLibrary.error}</div>
        {:else if assets.length === 0 && assetLibrary.hasFilters}
            <!-- Distinguish "narrowed to nothing" from "nothing imported" — the
                 fix for one is a click, for the other it's an import. -->
            <div class="flex flex-col items-center justify-center gap-2 h-32 text-sm text-neutral-500">
                <span>No assets match these filters.</span>
                <button
                    type="button"
                    onclick={() => assetLibrary.clearFilters()}
                    class="rounded px-2 py-0.5 text-xs font-medium text-blue-700 hover:bg-blue-100"
                >
                    ✕ Clear filters
                </button>
            </div>
        {:else if assets.length === 0}
            <div class="flex items-center justify-center h-32 text-sm text-neutral-500">No assets in this library yet.</div>
        {:else}
            <!-- Clicking the gaps between cards deselects, the way it does in
                 every file manager. Cards stop this by being the closest
                 [role=option]; the check runs on the bubbled event. -->
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <div
                bind:this={scrollEl}
                onpointerdown={(e) => {
                    if (!(e.target as HTMLElement).closest('[role="option"]')) selection.clear();
                }}
                class="relative flex-1 min-h-0 overflow-y-auto w-full
                       [scrollbar-width:thin] [scrollbar-color:theme(colors.neutral.700)_transparent] bg-white"
            >
                <!-- svelte-ignore a11y_no_noninteractive_element_to_interactive_role -->
                <div
                    role="listbox"
                    aria-multiselectable="true"
                    aria-label="Assets"
                    style="position: relative; width: 100%; height: {$virtualizer.getTotalSize()}px;"
                >
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
                            selected={selection.has(light.id)}
                            onPointerDown={(e) =>
                                selection.pointerDownAsset(idsNow(), item.index, mods(e))}
                            onClick={() => selection.clickAsset(light.id)}
                            onActivate={() => selection.selectOnlyAsset(light.id)}
                        />
                    {/each}
                </div>
            </div>
        {/if}
    </div>
