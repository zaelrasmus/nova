<script lang="ts">
    import {
        assetLibrary,
        ASSET_TYPE_LABELS,
        ORIENTATION_LABELS,
        type AssetTypeFilter,
        type Orientation,
    } from "$lib/assets.svelte";

    const filters = $derived(assetLibrary.filters);
    const active = $derived(assetLibrary.hasFilters);

    const isType = (v: AssetTypeFilter) => filters.asset_types.includes(v);
</script>

<!-- Always visible, even with nothing active. Filters hide data, so the controls
     that produced the current view must never be more than a glance away — an
     invisible active filter is the #1 "the app is broken" report in every DAM. -->
<div
    class="flex items-center gap-3 border-b border-neutral-800 px-4 py-1.5 text-xs
           {active ? 'bg-blue-50' : 'bg-white'}"
>
    <div class="flex items-center gap-1.5">
        <span class="text-neutral-500">Type</span>
        {#each ASSET_TYPE_LABELS as opt (opt.value)}
            <button
                type="button"
                onclick={() => assetLibrary.toggleAssetType(opt.value)}
                aria-pressed={isType(opt.value)}
                class="rounded px-2 py-0.5 font-medium transition-colors
                    {isType(opt.value)
                    ? 'bg-blue-600 text-white'
                    : 'bg-neutral-200 text-neutral-500 hover:bg-neutral-300'}"
            >
                {opt.label}
            </button>
        {/each}
    </div>

    <div class="flex items-center gap-1.5">
        <span class="text-neutral-500">Orientation</span>
        <select
            value={filters.orientation ?? ""}
            onchange={(e) =>
                assetLibrary.setOrientation((e.currentTarget.value || null) as Orientation | null)}
            aria-label="Filter by orientation"
            class="rounded border border-neutral-300 bg-white px-1.5 py-0.5 text-neutral-700
                   focus:outline-none focus-visible:ring-1 focus-visible:ring-neutral-400"
        >
            <option value="">Any</option>
            {#each ORIENTATION_LABELS as opt (opt.value)}
                <option value={opt.value}>{opt.label}</option>
            {/each}
        </select>
    </div>

    {#if active}
        <button
            type="button"
            onclick={() => assetLibrary.clearFilters()}
            class="ml-auto rounded px-2 py-0.5 font-medium text-blue-700
                   hover:bg-blue-100 hover:text-blue-900"
        >
            ✕ Clear filters
        </button>
    {/if}
</div>
