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
    <span class={legendClass}>Palette</span>

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
        <div class="flex h-7 w-full overflow-hidden rounded border border-neutral-700">
            {#each swatches as swatch, i (i)}
                <button
                    type="button"
                    onclick={() => (picked = i)}
                    title="{rgbToHex(swatch).toUpperCase()} · {Math.round(swatch.ratio * 100)}%"
                    aria-label="Select {rgbToHex(swatch)}"
                    aria-pressed={picked === i}
                    style="background-color: {rgbToHex(swatch)}; flex: {swatch.ratio} 0 0"
                    class="min-w-[10px] transition-shadow
                           {picked === i ? 'shadow-[inset_0_0_0_2px_white]' : ''}"
                ></button>
            {/each}
        </div>

        {#if current}
            <div class="mt-1.5 flex items-center gap-1.5">
                <button
                    type="button"
                    onclick={copy}
                    title="Copy {value}"
                    class="flex-1 truncate rounded border border-neutral-700 bg-neutral-900 px-2 py-1
                           text-left font-mono text-xs text-neutral-300 hover:border-neutral-500
                           hover:text-neutral-100"
                >
                    {value}
                </button>
                <span class="w-8 shrink-0 text-right text-[10px] text-neutral-500">
                    {Math.round(current.ratio * 100)}%
                </span>
            </div>

            <div class="mt-1.5 flex items-center gap-1.5">
                <div class="flex overflow-hidden rounded border border-neutral-700">
                    {#each COLOR_FORMATS as f (f)}
                        <button
                            type="button"
                            onclick={() => (format = f)}
                            aria-pressed={format === f}
                            class="px-1.5 py-0.5 text-[10px] font-medium transition-colors
                                {format === f
                                ? 'bg-neutral-700 text-neutral-100'
                                : 'text-neutral-500 hover:bg-neutral-800'}"
                        >
                            {f}
                        </button>
                    {/each}
                </div>
                <button
                    type="button"
                    onclick={findSimilar}
                    class="ml-auto rounded px-1.5 py-0.5 text-[10px] font-medium text-blue-400
                           hover:bg-neutral-800 hover:text-blue-300"
                >
                    Find similar
                </button>
            </div>
        {/if}
    {/if}
</div>
