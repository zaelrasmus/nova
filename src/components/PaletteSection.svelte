<script lang="ts">
    import { toast } from "svelte-sonner";
    import {
        assetLibrary,
        COLOR_FORMATS,
        DEFAULT_MIN_COVERAGE,
        accuracyToTolerance,
        formatColor,
        rgbToHex,
        type ColorFormat,
        type PaletteSwatch,
    } from "$lib/assets.svelte";
    import Segmented from "./controls/Segmented.svelte";

    interface Props {
        assetId: string;
        legendClass: string;
    }
    let { assetId, legendClass }: Props = $props();

    /**
     * Accuracy "Find similar" starts from — the midpoint of the slider, ~ΔE 21.
     * Loose enough to return a useful set rather than only near-identical images;
     * the user tightens it in the filter bar from there.
     */
    const FIND_SIMILAR_ACCURACY = 50;

    let swatches = $state<PaletteSwatch[]>([]);
    let loading = $state(true);
    let picked = $state(0);
    let format = $state<ColorFormat>("HEX");

    $effect(() => {
        const id = assetId;
        let cancelled = false;
        loading = true;
        assetLibrary
            .fetchPalette(id)
            .then((rows) => {
                if (cancelled) return; // a newer selection superseded this fetch
                swatches = rows;
                picked = 0;
                loading = false;
            })
            .catch(() => {
                if (cancelled) return;
                swatches = [];
                loading = false;
            });
        return () => {
            cancelled = true;
        };
    });

    const current = $derived(swatches[picked]);
    const value = $derived(current ? formatColor(current, format) : "");

    async function copy() {
        if (!value) return;
        try {
            // navigator.clipboard works in the Tauri webview (secure context), so
            // this needs no plugin.
            await navigator.clipboard.writeText(value);
            toast.success(`Copied ${value}`);
        } catch {
            toast.error("Couldn't copy to the clipboard.");
        }
    }

    /**
     * Hand this color to the filter bar. Deliberately does NOT change the scope:
     * a filter narrows wherever you already are, so searching from inside a
     * folder searches that folder.
     */
    function findSimilar() {
        if (!current) return;
        assetLibrary.setColorFilter({
            r: current.r,
            g: current.g,
            b: current.b,
            tolerance: accuracyToTolerance(FIND_SIMILAR_ACCURACY),
            min_coverage: DEFAULT_MIN_COVERAGE,
        });
    }
</script>

<div>
    <div class="relative">
        <span class={legendClass}>Palette</span>
        {#if current}
            <button
                type="button"
                onclick={findSimilar}
                class="absolute -top-0.5 right-0 rounded-[5px] px-1.5 py-0.5 text-[11.5px] font-medium
                       text-brand-text transition-colors hover:bg-brand-soft"
            >
                Find similar
            </button>
        {/if}
    </div>

    {#if loading}
        <p class="text-xs text-neutral-600">Reading colors…</p>
    {:else if swatches.length === 0}
        <!-- A color filter can't match this asset either; say so rather than
             showing an empty strip that looks like "no colors in this image". -->
        <p class="text-xs text-neutral-600">
            Not analyzed yet. Use <span class="text-neutral-500">Analyze now</span> in the filter bar.
        </p>
    {:else}
        <!-- Width tracks coverage, so the bar reads as a composition rather than
             a row of equal chips: eight swatches with no weighting would imply
             the 4% color matters as much as the 40% one. -->
        <div class="flex h-7 w-full gap-0.5 overflow-hidden rounded-lg">
            {#each swatches as swatch, i (i)}
                <button
                    type="button"
                    onclick={() => (picked = i)}
                    title="{rgbToHex(swatch).toUpperCase()} · {Math.round(swatch.ratio * 100)}%"
                    aria-label="Select {rgbToHex(swatch)}"
                    aria-pressed={picked === i}
                    style="background-color: {rgbToHex(swatch)}; flex: {swatch.ratio} 0 0"
                    class="min-w-2 transition-shadow
                           {picked === i
                        ? 'shadow-[inset_0_0_0_2px_rgb(255_255_255/0.9),inset_0_0_0_3px_rgb(0_0_0/0.35)]'
                        : ''}"
                ></button>
            {/each}
        </div>

        {#if current}
            <div class="mt-2 flex items-center gap-2">
                <span
                    class="h-4 w-4 shrink-0 rounded-[5px] shadow-[inset_0_0_0_1px_rgb(255_255_255/0.15)]"
                    style="background-color: {rgbToHex(current)}"
                ></span>
                <button
                    type="button"
                    onclick={copy}
                    title="Copy {value}"
                    class="-mx-1 min-w-0 truncate rounded px-1 py-0.5 text-left font-mono text-xs
                           font-medium text-neutral-200 transition-colors hover:bg-wash hover:text-neutral-50"
                >
                    {value}
                </button>
                <span class="shrink-0 text-[11px] tabular-nums text-neutral-600">
                    {Math.round(current.ratio * 100)}%
                </span>
                <div class="ml-auto">
                    <Segmented
                        value={format}
                        options={COLOR_FORMATS.map((f) => ({ value: f, label: f }))}
                        label="Color format"
                        onchange={(f) => (format = f)}
                    />
                </div>
            </div>
        {/if}
    {/if}
</div>
