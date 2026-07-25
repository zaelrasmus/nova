<script lang="ts">
    import { assetLibrary, PIN_COLORS, type PinColor } from "$lib/assets.svelte";
    import { pinColorVar } from "$lib/pins";

    /**
     * The accent picker for a pinned folder. Shared by the context menu and the
     * inspector, so a colour means the same thing and looks the same in both.
     *
     * Takes an ID, not a folder: every caller would otherwise have to re-fetch
     * the row after each pick to keep the selected ring honest, and the context
     * menu deliberately holds a frozen snapshot of its target. Looking the row up
     * from the store means this is always showing what's actually stored.
     */
    interface Props {
        folderId: string;
    }

    const { folderId }: Props = $props();

    const folder = $derived(assetLibrary.folders.find((f) => f.id === folderId));

    const swatchClass =
        "h-4 w-4 rounded-full border transition-transform hover:scale-110 " +
        "aria-pressed:ring-2 aria-pressed:ring-neutral-300 aria-pressed:ring-offset-1 " +
        "aria-pressed:ring-offset-neutral-900";

    function pick(color: PinColor | null) {
        void assetLibrary.setFolderColor(folderId, color);
    }
</script>

{#if folder}
    <div class="flex flex-wrap items-center gap-1">
        {#each PIN_COLORS as color (color)}
            <button
                type="button"
                title={color}
                aria-label="Accent {color}"
                aria-pressed={folder.color === color}
                onclick={() => pick(color)}
                class={swatchClass}
                style="border-color: {pinColorVar(color)}; background-color: {pinColorVar(color)}"
            ></button>
        {/each}
        <button
            type="button"
            title="No accent"
            aria-label="No accent"
            aria-pressed={folder.color === null}
            onclick={() => pick(null)}
            class="{swatchClass} border-neutral-600"
        ></button>
    </div>
{/if}
