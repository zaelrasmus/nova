<script lang="ts">
    import { settings } from "../../routes/settings.svelte";
    import { Label } from "$components/ui/label";

    const qualityOptions = [
        { value: "auto", label: "Auto", desc: "Lossless for graphics, lossy for photos" },
        { value: "lossy", label: "Lossy", desc: "Smallest files" },
        { value: "lossless", label: "Lossless", desc: "Pixel-perfect, larger" },
    ] as const;
</script>

<div class="flex flex-col gap-6">
    <div class="flex flex-col gap-2">
        <div class="flex flex-col gap-0.5">
            <Label class="text-sm text-neutral-200">Thumbnail quality</Label>
            <p class="text-xs text-neutral-500">
                How grid thumbnails are encoded. Applies to newly imported assets.
            </p>
        </div>
        <div class="inline-flex w-fit overflow-hidden rounded-md border border-neutral-700">
            {#each qualityOptions as opt}
                <button
                    type="button"
                    title={opt.desc}
                    onclick={() => settings.set("thumbnailQuality", opt.value)}
                    class="px-3 py-1.5 text-xs transition-colors
                           {settings.preferences.thumbnailQuality === opt.value
                        ? 'bg-neutral-700 text-neutral-100'
                        : 'bg-neutral-900 text-neutral-400 hover:bg-neutral-800'}"
                >
                    {opt.label}
                </button>
            {/each}
        </div>
    </div>
</div>
