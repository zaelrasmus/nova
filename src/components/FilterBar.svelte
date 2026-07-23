<script lang="ts">
    import {
        assetLibrary,
        ASSET_TYPE_LABELS,
        SHAPE_PRESETS,
        SHAPE_GROUPS,
        RATIO_TOLERANCE,
        shapeKey,
        type AssetTypeFilter,
    } from "$lib/assets.svelte";

    const filters = $derived(assetLibrary.filters);
    const active = $derived(assetLibrary.hasFilters);

    const isType = (v: AssetTypeFilter) => filters.asset_types.includes(v);

    // Custom ratio inputs. Seeded from the active shape so reopening "Custom"
    // shows the numbers actually in effect rather than resetting to a default.
    let customNum = $state(16);
    let customDen = $state(9);

    // Tracked explicitly rather than derived from the shape alone: picking
    // "Custom…" while 16:9 is active applies 16:9, which shapeKey() would map
    // straight back to the "16:9" preset — snapping the dropdown shut before the
    // inputs could be used.
    let customMode = $state(false);
    const selected = $derived(customMode ? "custom" : shapeKey(filters.shape));

    $effect(() => {
        const s = filters.shape;
        if (s === null) {
            customMode = false; // cleared from elsewhere (Clear filters, library switch)
            return;
        }
        if (s.kind === "ratio") {
            customNum = s.num;
            customDen = s.den;
        }
    });

    function applyCustom() {
        // A zero or negative side has no meaning; ignore rather than sending a
        // predicate that can only match nothing.
        if (customNum > 0 && customDen > 0) {
            assetLibrary.setShape({
                kind: "ratio",
                num: customNum,
                den: customDen,
                tolerance: RATIO_TOLERANCE,
            });
        }
    }

    function selectShape(key: string) {
        customMode = key === "custom";
        if (key === "") return assetLibrary.setShape(null);
        if (key === "custom") return applyCustom();
        const preset = SHAPE_PRESETS.find((p) => p.key === key);
        if (preset) assetLibrary.setShape(preset.shape);
    }
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
        <span class="text-neutral-500">Shape</span>
        <select
            value={selected}
            onchange={(e) => selectShape(e.currentTarget.value)}
            aria-label="Filter by shape"
            class="rounded border border-neutral-300 bg-white px-1.5 py-0.5 text-neutral-700
                   focus:outline-none focus-visible:ring-1 focus-visible:ring-neutral-400"
        >
            <option value="">Any</option>
            {#each SHAPE_GROUPS as group (group)}
                <optgroup label={group}>
                    {#each SHAPE_PRESETS.filter((p) => p.group === group) as opt (opt.key)}
                        <option value={opt.key}>{opt.label}</option>
                    {/each}
                </optgroup>
            {/each}
            <option value="custom">Custom…</option>
        </select>

        {#if selected === "custom"}
            <span class="flex items-center gap-1">
                <input
                    type="number"
                    min="1"
                    step="1"
                    bind:value={customNum}
                    onchange={applyCustom}
                    aria-label="Custom ratio width"
                    class="w-12 rounded border border-neutral-300 bg-white px-1 py-0.5 text-neutral-700
                           focus:outline-none focus-visible:ring-1 focus-visible:ring-neutral-400"
                />
                <span class="text-neutral-500">:</span>
                <input
                    type="number"
                    min="1"
                    step="1"
                    bind:value={customDen}
                    onchange={applyCustom}
                    aria-label="Custom ratio height"
                    class="w-12 rounded border border-neutral-300 bg-white px-1 py-0.5 text-neutral-700
                           focus:outline-none focus-visible:ring-1 focus-visible:ring-neutral-400"
                />
            </span>
        {/if}
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
