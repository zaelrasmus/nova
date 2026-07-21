<script lang="ts">
    import { settings } from "../../routes/settings.svelte";
    import { Label } from "$components/ui/label";

    const toggle = () =>
        settings.set("animateGifsInGrid", !settings.preferences.animateGifsInGrid);
</script>

<div class="flex flex-col gap-6">
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
            onclick={toggle}
            class="relative inline-flex h-6 w-11 items-center rounded-full transition-colors
                   {settings.preferences.animateGifsInGrid ? 'bg-blue-600' : 'bg-neutral-700'}"
        >
            <span
                class="inline-block h-4 w-4 transform rounded-full bg-white transition-transform
                       {settings.preferences.animateGifsInGrid ? 'translate-x-6' : 'translate-x-1'}"
            ></span>
        </button>
    </div>
</div>
