<script lang="ts">
    import { SlidersHorizontal, Flame } from "@lucide/svelte";
    import { toast } from "svelte-sonner";
    import SortControl from "./SortControl.svelte";
    import QuickActionsMenu from "./actions/QuickActionsMenu.svelte";
    import { settings } from "../routes/settings.svelte";
    import { assetLibrary } from "$lib/assets.svelte";
    import { layout } from "$lib/layout.svelte";

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

    let emptying = $state(false);

    /**
     * The count is in the confirmation on purpose: "empty the Trash" is easy to
     * agree to, "delete 412 assets" is the same sentence with the consequence
     * spelled out.
     */
    async function emptyTrash() {
        const count = assetLibrary.trashCount;
        const ok = window.confirm(
            `Permanently delete ${count.toLocaleString()} ${count === 1 ? "asset" : "assets"}? ` +
                `The files are removed from disk. This can't be undone.`,
        );
        if (!ok) return;
        emptying = true;
        try {
            const purged = await assetLibrary.emptyTrash();
            toast.success(`Deleted ${purged.toLocaleString()} permanently`);
        } catch (e) {
            toast.error(typeof e === "string" ? e : "Couldn't empty the Trash.");
        } finally {
            emptying = false;
        }
    }

    // Local mirror so the slider drags smoothly; persisted on release (onchange).
    let numColumns = $state(settings.preferences.gridColumns);
    $effect(() => {
        numColumns = settings.preferences.gridColumns;
    });
</script>

<div class="flex items-center gap-3">
    <!-- Only inside the Trash. Emptying is the one irreversible action in the
         app, so it lives in the place you have to navigate to on purpose rather
         than sitting permanently in the toolbar of every view. -->
    {#if assetLibrary.scope.kind === "trash" && assetLibrary.trashCount > 0}
        <button
            type="button"
            onclick={emptyTrash}
            disabled={emptying}
            class="flex shrink-0 items-center gap-1.5 rounded border border-red-900/60 px-2 py-1
                   text-xs text-red-400 transition-colors hover:bg-red-950/40
                   disabled:opacity-50"
        >
            <Flame class="h-3.5 w-3.5" />
            {emptying ? "Emptying…" : `Empty Trash (${assetLibrary.trashCount.toLocaleString()})`}
        </button>
        <div class="h-4 w-px bg-neutral-800"></div>
    {/if}

    <!-- The way in to the filter bar. Shows a dot while filters are active, so a
         narrowed library is legible even with the bar closed — and it can't be
         closed while narrowed, since the bar force-shows in that state. -->
    <button
        type="button"
        onclick={() => (layout.filterBarOpen = !layout.filterBarOpen)}
        title="Filters"
        aria-label="Filters"
        aria-pressed={layout.filterBarOpen || assetLibrary.hasFilters}
        class="relative grid h-7 w-7 shrink-0 place-items-center rounded transition-colors
               {layout.filterBarOpen || assetLibrary.hasFilters
            ? 'bg-neutral-800 text-neutral-100'
            : 'text-neutral-500 hover:bg-neutral-800 hover:text-neutral-200'}"
    >
        <SlidersHorizontal class="h-4 w-4" />
        {#if assetLibrary.hasFilters}
            <span class="absolute right-1 top-1 h-1.5 w-1.5 rounded-full bg-blue-500"></span>
        {/if}
    </button>

    <!-- The verb, next to the controls that also act on the current view. -->
    <QuickActionsMenu />

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
