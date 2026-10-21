<!--
  Settings › Display. Thumbnail encoding and grid presentation.

  Changing quality only affects NEW thumbnails, so this section also owns the
  "rebuild all" action — the only way to apply a changed setting to a library
  that already has them. A rebuild wipes `thumbnails/` and re-generates, holding
  the exclusive side of the backend's `thumb_gen` lock.
-->
<script lang="ts">
    import { settings, VIEWER_BACKGROUNDS } from "../../routes/settings.svelte";
    import { assetLibrary } from "$lib/assets.svelte";
    import { Label } from "$components/ui/label";
    import Segmented from "$components/controls/Segmented.svelte";
    import Switch from "$components/controls/Switch.svelte";
    import Range from "$components/controls/Range.svelte";
    import { toast } from "svelte-sonner";

    // Values match ThumbMode::from_setting on the Rust side.
    const QUALITIES = [
        { value: "auto", label: "Auto", hint: "Lossless for flat/graphic art, lossy for photos" },
        { value: "lossy", label: "Lossy", hint: "Smallest files, slight quality loss" },
        { value: "lossless", label: "Lossless", hint: "Exact quality, largest files" },
    ] as const;

    const BACKGROUNDS = VIEWER_BACKGROUNDS.map((b) => ({
        value: b,
        label: b[0].toUpperCase() + b.slice(1),
    }));

    // Local mirror so the label tracks the drag smoothly; we persist on release
    // (Range's oncommit) rather than on every input tick to avoid a disk write per pixel.
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

{#snippet row(label: string, hint: string)}
    <div class="flex flex-col gap-0.5">
        <Label class="text-sm text-neutral-200">{label}</Label>
        <p class="text-xs text-neutral-500">{hint}</p>
    </div>
{/snippet}

<div class="flex flex-col gap-6">
    <!-- Animate GIFs in grid -->
    <div class="flex items-center justify-between gap-8">
        {@render row(
            "Animate GIFs in grid",
            "Play animated files in the grid instead of a static thumbnail.",
        )}
        <Switch
            label="Animate GIFs in grid"
            checked={settings.preferences.animateGifsInGrid}
            onchange={(v) => settings.set("animateGifsInGrid", v)}
        />
    </div>

    <!-- Ordinal badges -->
    <div class="flex items-center justify-between gap-8">
        {@render row(
            "Number items in manual order",
            "Show each item's place (1, 2, 3…) when a view is sorted manually.",
        )}
        <Switch
            label="Number items in manual order"
            checked={settings.preferences.showOrdinals}
            onchange={(v) => settings.set("showOrdinals", v)}
        />
    </div>

    <!-- Viewer background -->
    <div class="flex items-center justify-between gap-8">
        {@render row(
            "Viewer background",
            "The backdrop the viewer opens with. Press B in the viewer to try another for that viewing only.",
        )}
        <Segmented
            label="Viewer background"
            value={settings.preferences.viewerBackground}
            options={BACKGROUNDS}
            onchange={(v) => settings.set("viewerBackground", v)}
        />
    </div>

    <!-- Next folder in the viewer -->
    <div class="flex items-center justify-between gap-8">
        {@render row(
            "Continue into the next folder",
            "At a folder's last item, the viewer offers the next folder beside it — the next chapter. Also with →.",
        )}
        <Switch
            label="Continue into the next folder"
            checked={settings.preferences.viewerNextFolder}
            onchange={(v) => settings.set("viewerNextFolder", v)}
        />
    </div>

    <!-- Thumbnail quality -->
    <div class="flex items-center justify-between gap-8">
        {@render row(
            "Thumbnail quality",
            "How grid thumbnails are encoded. Applies to newly generated thumbnails — use “Rebuild” to re-encode the current library.",
        )}
        <Segmented
            label="Thumbnail quality"
            value={settings.preferences.thumbnailQuality}
            options={QUALITIES.map((q) => ({ value: q.value, label: q.label, title: q.hint }))}
            onchange={(v) => settings.set("thumbnailQuality", v)}
        />
    </div>

    <!-- Lossy quality slider (irrelevant to Lossless mode) -->
    {#if settings.preferences.thumbnailQuality !== "lossless"}
        <div class="flex flex-col gap-2">
            <div class="flex items-center justify-between gap-8">
                {@render row(
                    "Lossy quality",
                    "Lower = smaller files, more compression artifacts. Higher = closer to the original, larger files. Applies to Lossy (and Auto's photo path).",
                )}
                <span class="w-8 shrink-0 text-right text-sm tabular-nums text-neutral-300">
                    {lossyQuality}
                </span>
            </div>
            <Range
                label="Lossy quality"
                bind:value={lossyQuality}
                min={20}
                max={100}
                oncommit={(v) => settings.set("thumbnailLossyQuality", v)}
                class="w-full"
            />
        </div>
    {/if}

    <!-- Rebuild thumbnails -->
    <div class="flex items-center justify-between gap-8">
        {@render row(
            "Rebuild thumbnails",
            "Clear and regenerate every thumbnail in the active library with the current quality.",
        )}
        <button
            type="button"
            onclick={rebuild}
            disabled={isRebuilding}
            class="shrink-0 rounded-md bg-raised-2 px-3 py-1.5 text-xs font-medium text-neutral-100
                   ring-1 ring-inset ring-line-strong transition-colors hover:bg-neutral-700
                   disabled:cursor-not-allowed disabled:opacity-50"
        >
            {isRebuilding ? "Rebuilding…" : "Rebuild"}
        </button>
    </div>
</div>
