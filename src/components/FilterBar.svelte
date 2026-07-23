<script lang="ts">
    import {
        assetLibrary,
        ASSET_TYPE_LABELS,
        SHAPE_PRESETS,
        SHAPE_GROUPS,
        DATE_FIELD_LABELS,
        DATE_PRESETS,
        SIZE_UNITS,
        RATIO_TOLERANCE,
        dayRangeToInstants,
        presetToInstants,
        shapeKey,
        unitBytes,
        type AssetTypeFilter,
        type DateField,
        type SizeUnit,
    } from "$lib/assets.svelte";

    const filters = $derived(assetLibrary.filters);
    const active = $derived(assetLibrary.hasFilters);

    const isType = (v: AssetTypeFilter) => filters.asset_types.includes(v);

    const fieldClass =
        "rounded border border-neutral-300 bg-white px-1.5 py-0.5 text-neutral-700 " +
        "focus:outline-none focus-visible:ring-1 focus-visible:ring-neutral-400";
    const labelClass = "text-neutral-500";

    // ── Shape ────────────────────────────────────────────────────────────────
    // Custom ratio inputs, seeded from the active shape so reopening "Custom"
    // shows the numbers actually in effect rather than resetting to a default.
    let customNum = $state(16);
    let customDen = $state(9);

    // Tracked explicitly rather than derived from the shape alone: picking
    // "Custom…" while 16:9 is active applies 16:9, which shapeKey() would map
    // straight back to the "16:9" preset — snapping the dropdown shut before the
    // inputs could be used.
    let customMode = $state(false);
    const selectedShape = $derived(customMode ? "custom" : shapeKey(filters.shape));

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

    function applyCustomRatio() {
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
        if (key === "custom") return applyCustomRatio();
        const preset = SHAPE_PRESETS.find((p) => p.key === key);
        if (preset) assetLibrary.setShape(preset.shape);
    }

    // ── Date range ───────────────────────────────────────────────────────────
    let dateField = $state<DateField>("imported_date");
    // "" = any, a DATE_PRESETS key, or "custom". Local like customMode, and for
    // the same reason: the applied instants can't tell you which preset produced
    // them, and "Last 30 days" vs a hand-picked equivalent should look different.
    let datePreset = $state("");
    let dateFrom = $state("");
    let dateTo = $state("");

    $effect(() => {
        if (filters.date === null) {
            datePreset = "";
            dateFrom = "";
            dateTo = "";
        }
    });

    function applyDatePreset(key: string) {
        datePreset = key;
        if (key === "") return assetLibrary.setDateFilter(null);
        if (key === "custom") return applyCustomDates();
        const { from, until } = presetToInstants(key);
        assetLibrary.setDateFilter({ field: dateField, from, until });
    }

    function applyCustomDates() {
        const { from, until } = dayRangeToInstants(dateFrom || null, dateTo || null);
        // setDateFilter normalises an all-open range back to null.
        assetLibrary.setDateFilter({ field: dateField, from, until });
    }

    /** Re-apply whatever range is active against the newly chosen column. */
    function changeDateField() {
        if (datePreset === "") return;
        applyDatePreset(datePreset);
    }

    // ── Size range ───────────────────────────────────────────────────────────
    // NOTE: these are `number | null`, NOT strings. Svelte's bind:value on
    // <input type="number"> coerces through to_number(), so a string-typed state
    // silently becomes a number and any string method on it throws at runtime.
    let sizeMin = $state<number | null>(null);
    let sizeMax = $state<number | null>(null);
    let sizeUnit = $state<SizeUnit>("MB");

    $effect(() => {
        if (filters.size === null) {
            sizeMin = null;
            sizeMax = null;
        }
    });

    /** Display value -> bytes. Blank, negative and non-finite all mean "no bound". */
    function toBytes(value: number | null): number | null {
        if (value === null || !Number.isFinite(value) || value < 0) return null;
        return Math.round(value * unitBytes(sizeUnit));
    }

    function applySize() {
        assetLibrary.setSizeRange({ min: toBytes(sizeMin), max: toBytes(sizeMax) });
    }
</script>

<!-- Always visible, even with nothing active. Filters hide data, so the controls
     that produced the current view must never be more than a glance away — an
     invisible active filter is the #1 "the app is broken" report in every DAM. -->
<div
    class="flex flex-wrap items-center gap-x-4 gap-y-1.5 border-b border-neutral-800 px-4 py-1.5 text-xs
           {active ? 'bg-blue-50' : 'bg-white'}"
>
    <div class="flex items-center gap-1.5">
        <span class={labelClass}>Type</span>
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
        <span class={labelClass}>Shape</span>
        <select
            value={selectedShape}
            onchange={(e) => selectShape(e.currentTarget.value)}
            aria-label="Filter by shape"
            class={fieldClass}
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

        {#if selectedShape === "custom"}
            <input
                type="number"
                min="1"
                step="1"
                bind:value={customNum}
                onchange={applyCustomRatio}
                aria-label="Custom ratio width"
                class="w-12 {fieldClass}"
            />
            <span class={labelClass}>:</span>
            <input
                type="number"
                min="1"
                step="1"
                bind:value={customDen}
                onchange={applyCustomRatio}
                aria-label="Custom ratio height"
                class="w-12 {fieldClass}"
            />
        {/if}
    </div>

    <div class="flex items-center gap-1.5">
        <select
            bind:value={dateField}
            onchange={changeDateField}
            aria-label="Which date to filter on"
            class={fieldClass}
        >
            {#each DATE_FIELD_LABELS as opt (opt.value)}
                <option value={opt.value}>{opt.label}</option>
            {/each}
        </select>
        <select
            value={datePreset}
            onchange={(e) => applyDatePreset(e.currentTarget.value)}
            aria-label="Date range"
            class={fieldClass}
        >
            <option value="">Any time</option>
            {#each DATE_PRESETS as opt (opt.key)}
                <option value={opt.key}>{opt.label}</option>
            {/each}
            <option value="custom">Custom…</option>
        </select>

        {#if datePreset === "custom"}
            <!-- Explicit From/To labels: two bare date boxes give no clue which
                 end is which, and the arrow between them was doing too much work. -->
            <span class={labelClass}>From</span>
            <input
                type="date"
                bind:value={dateFrom}
                onchange={applyCustomDates}
                aria-label="From date"
                class={fieldClass}
            />
            <span class={labelClass}>To</span>
            <input
                type="date"
                bind:value={dateTo}
                onchange={applyCustomDates}
                aria-label="To date"
                class={fieldClass}
            />
        {/if}
    </div>

    <div class="flex items-center gap-1.5">
        <span class={labelClass}>Size</span>
        <input
            type="number"
            min="0"
            step="any"
            placeholder="min"
            bind:value={sizeMin}
            onchange={applySize}
            aria-label="Minimum size"
            class="w-16 {fieldClass}"
        />
        <span class={labelClass}>to</span>
        <input
            type="number"
            min="0"
            step="any"
            placeholder="max"
            bind:value={sizeMax}
            onchange={applySize}
            aria-label="Maximum size"
            class="w-16 {fieldClass}"
        />
        <select bind:value={sizeUnit} onchange={applySize} aria-label="Size unit" class={fieldClass}>
            {#each SIZE_UNITS as u (u.value)}
                <option value={u.value}>{u.value}</option>
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
