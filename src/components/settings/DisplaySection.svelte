<script lang="ts">
    import { settings } from "../../routes/settings.svelte";
    import { assetLibrary } from "$lib/assets.svelte";
    import { Label } from "$components/ui/label";
    import { toast } from "svelte-sonner";

    const toggleGifs = () =>
        settings.set("animateGifsInGrid", !settings.preferences.animateGifsInGrid);

    // Values match ThumbMode::from_setting on the Rust side.
    const QUALITIES = [
        { value: "auto", label: "Auto", hint: "Lossless for flat/graphic art, lossy for photos" },
        { value: "lossy", label: "Lossy", hint: "Smallest files, slight quality loss" },
        { value: "lossless", label: "Lossless", hint: "Exact quality, largest files" },
    ] as const;

    // Local mirror so the label tracks the drag smoothly; we persist on release
    // (onchange) rather than on every input tick to avoid a disk write per pixel.
    let lossyQuality = $state(settings.preferences.thumbnailLossyQuality);

    let isRebuilding = $state(false);

    async function rebuild() {
        isRebuilding = true;
        try {
            const count = await assetLibrary.rebuildThumbnails(
                settings.preferences.thumbnailQuality,
                settings.preferences.thumbnailLossyQuality,
            );
            toast.success(`Rebuilt ${count} thumbnail${count === 1 ? "" : "s"}.`);
        } catch (e) {
            toast.error(typeof e === "string" ? e : "Failed to rebuild thumbnails.");
        } finally {
            isRebuilding = false;
        }
    }
</script>

<div class="flex flex-col gap-6">
    <!-- Animate GIFs in grid -->
    <div class="flex items-center justify-between gap-8">
        <div class="flex flex-col gap-0.5">
            <Label class="text-sm text-neutral-200">Animate GIFs in grid</Label>
            <p class="text-xs text-neutral-500">
                Play animated files in the grid instead of a static thumbnail.
            </p>
        </div>
        <button
            type="button"
            role="switch"
            aria-label="Animate GIFs in grid"
            aria-checked={settings.preferences.animateGifsInGrid}
            onclick={toggleGifs}
            class="relative inline-flex h-6 w-11 items-center rounded-full transition-colors
                   {settings.preferences.animateGifsInGrid ? 'bg-blue-600' : 'bg-neutral-700'}"
        >
            <span
                class="inline-block h-4 w-4 transform rounded-full bg-white transition-transform
                       {settings.preferences.animateGifsInGrid ? 'translate-x-6' : 'translate-x-1'}"
            ></span>
        </button>
    </div>

    <!-- Thumbnail quality -->
    <div class="flex flex-col gap-2">
        <div class="flex flex-col gap-0.5">
            <Label class="text-sm text-neutral-200">Thumbnail quality</Label>
            <p class="text-xs text-neutral-500">
                How grid thumbnails are encoded. Applies to newly generated thumbnails —
                use “Rebuild” to re-encode the current library.
            </p>
        </div>
        <div class="inline-flex w-fit rounded-md border border-neutral-700 p-0.5">
            {#each QUALITIES as q}
                <button
                    type="button"
                    title={q.hint}
                    aria-pressed={settings.preferences.thumbnailQuality === q.value}
                    onclick={() => settings.set("thumbnailQuality", q.value)}
                    class="rounded px-3 py-1 text-xs font-medium transition-colors
                        {settings.preferences.thumbnailQuality === q.value
                        ? 'bg-blue-600 text-white'
                        : 'text-neutral-400 hover:text-neutral-200'}"
                >
                    {q.label}
                </button>
            {/each}
        </div>
    </div>

    <!-- Lossy quality slider (irrelevant to Lossless mode) -->
    {#if settings.preferences.thumbnailQuality !== "lossless"}
        <div class="flex flex-col gap-2">
            <div class="flex items-center justify-between gap-8">
                <div class="flex flex-col gap-0.5">
                    <Label class="text-sm text-neutral-200">Lossy quality</Label>
                    <p class="text-xs text-neutral-500">
                        Lower = smaller files, more compression artifacts. Higher = closer to
                        the original, larger files. Applies to Lossy (and Auto's photo path).
                    </p>
                </div>
                <span class="w-8 shrink-0 text-right text-sm tabular-nums text-neutral-300">
                    {lossyQuality}
                </span>
            </div>
            <input
                type="range"
                min="20"
                max="100"
                step="1"
                bind:value={lossyQuality}
                onchange={() => settings.set("thumbnailLossyQuality", lossyQuality)}
                class="w-full accent-blue-600"
            />
        </div>
    {/if}

    <!-- Rebuild thumbnails -->
    <div class="flex items-center justify-between gap-8">
        <div class="flex flex-col gap-0.5">
            <Label class="text-sm text-neutral-200">Rebuild thumbnails</Label>
            <p class="text-xs text-neutral-500">
                Clear and regenerate every thumbnail in the active library with the
                current quality.
            </p>
        </div>
        <button
            type="button"
            onclick={rebuild}
            disabled={isRebuilding}
            class="shrink-0 rounded-md bg-neutral-800 px-3 py-1.5 text-xs font-medium text-neutral-100
                   transition-colors hover:bg-neutral-700 disabled:cursor-not-allowed disabled:opacity-50"
        >
            {isRebuilding ? "Rebuilding…" : "Rebuild"}
        </button>
    </div>
</div>
