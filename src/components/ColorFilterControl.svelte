<script lang="ts">
    import {
        assetLibrary,
        COLOR_PRESETS,
        DEFAULT_MIN_COVERAGE,
        accuracyToTolerance,
        toleranceToAccuracy,
        hexToRgb,
        rgbToHex,
    } from "$lib/assets.svelte";
    import { toast } from "svelte-sonner";

    interface Props {
        fieldClass: string;
        labelClass: string;
    }
    let { fieldClass, labelClass }: Props = $props();

    const color = $derived(assetLibrary.filters.color);
    const coverage = $derived(assetLibrary.colorCoverage);
    const unanalyzed = $derived(coverage ? coverage.total - coverage.analyzed : 0);

    // Local mirrors. `hex` drives both the native picker and the text field —
    // <input type="color"> gives us the saturation/value square plus hue strip for
    // free on every platform, so there's no hand-rolled picker to maintain.
    let hex = $state("#9E9E9E");
    let accuracy = $state(50);
    // UNCOMMENT (with its control below) to drive coverage by hand while testing.
    // let minCoverage = $state(DEFAULT_MIN_COVERAGE);

    $effect(() => {
        const c = assetLibrary.filters.color;
        if (c === null) return;
        hex = rgbToHex(c);
        accuracy = Math.round(toleranceToAccuracy(c.tolerance));
    });

    function apply() {
        const rgb = hexToRgb(hex);
        if (!rgb) return; // half-typed hex; wait for a valid one
        assetLibrary.setColorFilter({
            ...rgb,
            tolerance: accuracyToTolerance(accuracy),
            min_coverage: DEFAULT_MIN_COVERAGE,
            // min_coverage: minCoverage,
        });
    }

    function pickPreset(preset: string) {
        hex = preset;
        apply();
    }

    let analyzing = $state(false);
    async function analyze() {
        analyzing = true;
        try {
            const done = await assetLibrary.analyzeColors();
            toast.success(done > 0 ? `Analyzed ${done} images.` : "Everything is already analyzed.");
        } catch (e) {
            toast.error(typeof e === "string" ? e : "Color analysis failed.");
        } finally {
            analyzing = false;
        }
    }
</script>

<div class="flex items-center gap-1.5">
    <span class={labelClass}>Color</span>

    <input
        type="color"
        bind:value={hex}
        onchange={apply}
        aria-label="Filter by color"
        class="h-6 w-8 cursor-pointer rounded border border-neutral-300 bg-white p-0.5"
    />
    <input
        type="text"
        bind:value={hex}
        onchange={apply}
        spellcheck="false"
        aria-label="Color hex code"
        class="w-20 font-mono uppercase {fieldClass}"
    />

    <span class="flex items-center gap-0.5">
        {#each COLOR_PRESETS as preset (preset)}
            <button
                type="button"
                onclick={() => pickPreset(preset)}
                title={preset}
                aria-label="Filter by {preset}"
                style="background-color: {preset}"
                class="h-4 w-4 rounded-sm border border-neutral-400/60 transition-transform
                       hover:scale-125 {color && rgbToHex(color).toUpperCase() === preset.toUpperCase()
                    ? 'ring-2 ring-blue-600 ring-offset-1'
                    : ''}"
            ></button>
        {/each}
    </span>

    {#if color}
        <span class={labelClass}>Accuracy</span>
        <input
            type="range"
            min="0"
            max="100"
            step="1"
            bind:value={accuracy}
            onchange={apply}
            aria-label="Color match accuracy"
            class="w-20 accent-blue-600"
        />

        <!-- TESTING: uncomment (and the two lines in the script) to drive the
             coverage floor by hand instead of using DEFAULT_MIN_COVERAGE.
        <span class={labelClass}>Coverage</span>
        <input
            type="range" min="0" max="0.5" step="0.01"
            bind:value={minCoverage} onchange={apply}
            aria-label="Minimum color coverage"
            class="w-20 accent-blue-600"
        />
        <span class="{labelClass} w-8">{Math.round(minCoverage * 100)}%</span>
        -->

        <button
            type="button"
            onclick={() => assetLibrary.setColorFilter(null)}
            title="Clear color filter"
            class="px-1 text-neutral-500 hover:text-neutral-800">✕</button
        >
    {/if}

    <!-- A color filter can't match an un-analyzed asset. Say so, rather than
         silently returning fewer results than the library actually contains. -->
    {#if color && unanalyzed > 0}
        <span class="flex items-center gap-1 text-amber-700">
            {unanalyzed.toLocaleString()} not analyzed
            <button
                type="button"
                onclick={analyze}
                disabled={analyzing}
                class="rounded px-1.5 py-0.5 font-medium underline underline-offset-2
                       hover:bg-amber-100 disabled:no-underline disabled:opacity-60"
            >
                {analyzing ? "Analyzing…" : "Analyze now"}
            </button>
        </span>
    {/if}
</div>
