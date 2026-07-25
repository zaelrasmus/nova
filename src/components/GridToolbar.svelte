<script lang="ts">
    import SortControl from "./SortControl.svelte";
    import { settings } from "../routes/settings.svelte";
    import { assetLibrary } from "$lib/assets.svelte";

    /**
     * View controls for the grid pane header.
     *
     * LAYOUT NOTE: these used to be a row inside AssetGrid, stacked above the
     * SearchBar and the FilterBar — three bars of chrome permanently eating the
     * asset area. They now live in the pane header (one 44px row) and AssetGrid
     * is nothing but assets.
     *
     * The two derivations below are duplicated from AssetGrid on purpose: both
     * read from the same stores, so there's no shared state to thread through —
     * only the same two lines.
     */
    const manualSort = $derived(assetLibrary.sort.order_by === "manual");
    const effectiveView = $derived(manualSort ? "justified" : settings.preferences.gridView);
    const isJustified = $derived(effectiveView === "justified");

    // Local mirror so the slider drags smoothly; persisted on release (onchange).
    let numColumns = $state(settings.preferences.gridColumns);
    $effect(() => {
        numColumns = settings.preferences.gridColumns;
    });
</script>

<div class="flex items-center gap-3">
    <SortControl />

    <!-- View switcher. Waterfall is disabled while sorting manually, because
         reading order is required for exact reorder; the title says why rather
         than the button just being dead. -->
    <div class="flex overflow-hidden rounded border border-neutral-700">
        <button
            type="button"
            onclick={() => settings.set("gridView", "waterfall")}
            disabled={manualSort}
            title={manualSort
                ? "Waterfall can't reorder exactly — Manual sort uses Justified"
                : "Waterfall view"}
            aria-pressed={effectiveView === "waterfall"}
            class="px-2 py-0.5 text-xs font-medium transition-colors disabled:opacity-40
                   {effectiveView === 'waterfall'
                ? 'bg-blue-600 text-white'
                : 'bg-neutral-900 text-neutral-400 hover:bg-neutral-800'}"
        >
            Waterfall
        </button>
        <button
            type="button"
            onclick={() => settings.set("gridView", "justified")}
            title="Justified rows"
            aria-pressed={effectiveView === "justified"}
            class="border-l border-neutral-700 px-2 py-0.5 text-xs font-medium transition-colors
                   {effectiveView === 'justified'
                ? 'bg-blue-600 text-white'
                : 'bg-neutral-900 text-neutral-400 hover:bg-neutral-800'}"
        >
            Justified
        </button>
    </div>

    <button
        type="button"
        onclick={() => settings.set("animateGifsInGrid", !settings.preferences.animateGifsInGrid)}
        title="Animate GIFs in the grid"
        aria-pressed={settings.preferences.animateGifsInGrid}
        class="rounded px-2 py-0.5 text-xs font-medium transition-colors
               {settings.preferences.animateGifsInGrid
            ? 'bg-blue-600 text-white'
            : 'bg-neutral-800 text-neutral-400 hover:bg-neutral-700'}"
    >
        GIF
    </button>

    <div class="flex items-center gap-2">
        <!-- In justified view the slider sets thumbnail SIZE (via target row
             height), not a fixed column count — the label follows. -->
        <span class="text-xs text-neutral-500">{isJustified ? "Size" : "Columns"}</span>
        <input
            type="range"
            min="2"
            max="8"
            step="1"
            bind:value={numColumns}
            onchange={() => settings.set("gridColumns", numColumns)}
            class="w-20 accent-neutral-400"
        />
        {#if !isJustified}
            <span class="w-3 text-center text-xs text-neutral-500">{numColumns}</span>
        {/if}
    </div>
</div>
