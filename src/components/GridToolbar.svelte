<script lang="ts">
    import {
        SlidersHorizontal,
        Flame,
        LayoutDashboard,
        Rows3,
        Grid3x3,
        Square,
        Eye,
    } from "@lucide/svelte";
    import { toast } from "svelte-sonner";
    import SortControl from "./SortControl.svelte";
    import QuickActionsMenu from "./actions/QuickActionsMenu.svelte";
    import { settings } from "../routes/settings.svelte";
    import { assetLibrary } from "$lib/assets.svelte";
    import { layout } from "$lib/layout.svelte";
    import Segmented from "./controls/Segmented.svelte";
    import Range from "./controls/Range.svelte";
    import Switch from "./controls/Switch.svelte";
    import Popover from "./controls/Popover.svelte";

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

    // The size slider runs the other way from the setting it writes: right means
    // BIGGER thumbnails, i.e. fewer columns. `gridColumns` stays the stored unit
    // (it is what the layout math wants); only the control is flipped, so the
    // slider reads as "size" in both views.
    const SIZE_BASE = 10; // 2..8 columns  <->  8..2 on the slider
    const sizeValue = $derived(SIZE_BASE - settings.preferences.gridColumns);

    // Written straight into the store while dragging so the grid resizes live,
    // and persisted once on release.
    const resize = (v: number) => (settings.preferences.gridColumns = SIZE_BASE - v);
    const commitSize = (v: number) => settings.set("gridColumns", SIZE_BASE - v);

    const inFolder = $derived(assetLibrary.scope.kind === "folder");
    const iconButton = "grid h-7 w-7 shrink-0 place-items-center rounded-md transition-colors";

    function toggleSubfolders() {
        if (assetLibrary.scope.kind !== "folder") return;
        assetLibrary
            .setIncludeSubfolders(assetLibrary.scope.id, !assetLibrary.includesSubfolders)
            .catch((e) => toast.error(typeof e === "string" ? e : "Failed to update the folder."));
    }
</script>

<div class="flex items-center gap-1">
    <!-- Only inside the Trash. Emptying is the one irreversible action in the
         app, so it lives in the place you have to navigate to on purpose rather
         than sitting permanently in the toolbar of every view. -->
    {#if assetLibrary.scope.kind === "trash" && assetLibrary.trashCount > 0}
        <button
            type="button"
            onclick={emptyTrash}
            disabled={emptying}
            class="mr-1 flex h-7 shrink-0 items-center gap-1.5 rounded-md px-2.5 text-xs font-medium
                   text-red-400 ring-1 ring-inset ring-red-900/60 transition-colors
                   hover:bg-red-950/40 disabled:opacity-50"
        >
            <Flame class="h-3.5 w-3.5" />
            {emptying ? "Emptying…" : `Empty Trash (${assetLibrary.trashCount.toLocaleString()})`}
        </button>
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
        class="relative {iconButton}
               {layout.filterBarOpen || assetLibrary.hasFilters
            ? 'bg-wash-strong text-neutral-100'
            : 'text-neutral-500 hover:bg-wash hover:text-neutral-200'}"
    >
        <SlidersHorizontal class="h-4 w-4" />
        {#if assetLibrary.hasFilters}
            <span
                class="absolute right-1 top-1 h-1.5 w-1.5 rounded-full bg-brand
                       shadow-[0_0_0_2px_var(--nv-canvas)]"
            ></span>
        {/if}
    </button>

    <!-- The verb, next to the controls that also act on the current view. -->
    <QuickActionsMenu />

    <SortControl />

    <div class="mx-1.5 h-4 w-px shrink-0 bg-line-strong" aria-hidden="true"></div>

    <!-- View switcher. Waterfall is disabled while sorting manually, because
         reading order is required for exact reorder; the title says why rather
         than the button just being dead. -->
    <Segmented
        label="Layout"
        value={effectiveView}
        options={[
            {
                value: "waterfall",
                label: "Waterfall",
                icon: LayoutDashboard,
                disabled: manualSort,
                title: manualSort
                    ? "Waterfall can't reorder exactly — Manual sort uses Justified"
                    : "Waterfall",
            },
            { value: "justified", label: "Justified rows", icon: Rows3 },
        ]}
        onchange={(v) => settings.set("gridView", v)}
    />

    <!-- Thumbnail size. In justified view it sets the target row height rather
         than a column count; either way, right is bigger. -->
    <div class="flex shrink-0 items-center gap-1.5 px-1.5 text-neutral-500" title="Thumbnail size">
        <Grid3x3 class="h-3 w-3" />
        <Range
            label="Thumbnail size"
            value={sizeValue}
            min={SIZE_BASE - 8}
            max={SIZE_BASE - 2}
            oninput={resize}
            oncommit={commitSize}
            class="w-20"
        />
        <Square class="h-3.5 w-3.5" />
    </div>

    <!-- Display options. These were three always-visible blue pills (GIF, #,
         Subfolders); they're occasional switches, so they share one popover and
         the toolbar keeps the accent for what is actually selected. -->
    <Popover label="Display options" width="w-72">
        {#snippet trigger({ open, toggle })}
            <button
                type="button"
                onclick={toggle}
                title="Display options"
                aria-label="Display options"
                aria-expanded={open}
                class="{iconButton}
                       {open
                    ? 'bg-wash-strong text-neutral-100'
                    : 'text-neutral-500 hover:bg-wash hover:text-neutral-200'}"
            >
                <Eye class="h-4 w-4" />
            </button>
        {/snippet}
        {#snippet children()}
            <p class="nv-menu-label">Display</p>
            <!-- Per folder, like its sort: a series folder can read as one list of
                 chapters while every other folder keeps showing only its own items. -->
            <div class="flex items-center gap-3 rounded-md px-2 py-2 {inFolder ? '' : 'opacity-45'}">
                <div class="min-w-0 flex-1">
                    <p class="text-[13px] text-neutral-200">Include subfolders</p>
                    <p class="text-[11px] text-neutral-500">
                        {inFolder
                            ? "Show this folder's whole tree as one list. Remembered per folder."
                            : "Only inside a folder."}
                    </p>
                </div>
                <Switch
                    label="Include subfolders"
                    checked={inFolder && assetLibrary.includesSubfolders}
                    disabled={!inFolder}
                    onchange={toggleSubfolders}
                />
            </div>
            <div class="flex items-center gap-3 rounded-md px-2 py-2">
                <div class="min-w-0 flex-1">
                    <p class="text-[13px] text-neutral-200">Animate GIFs</p>
                    <p class="text-[11px] text-neutral-500">Play animated files in the grid.</p>
                </div>
                <Switch
                    label="Animate GIFs"
                    checked={settings.preferences.animateGifsInGrid}
                    onchange={(v) => settings.set("animateGifsInGrid", v)}
                />
            </div>
            <!-- Ordinal badges only mean something in a human-chosen order, so the
                 switch is inert outside Manual sort; the hint says why. -->
            <div class="flex items-center gap-3 rounded-md px-2 py-2 {manualSort ? '' : 'opacity-45'}">
                <div class="min-w-0 flex-1">
                    <p class="text-[13px] text-neutral-200">Number items</p>
                    <p class="text-[11px] text-neutral-500">
                        {manualSort
                            ? "Show each item's place in the manual order."
                            : "Only in Manual sort."}
                    </p>
                </div>
                <Switch
                    label="Number items"
                    checked={settings.preferences.showOrdinals}
                    disabled={!manualSort}
                    onchange={(v) => settings.set("showOrdinals", v)}
                />
            </div>
        {/snippet}
    </Popover>
</div>
