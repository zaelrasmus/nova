<script lang="ts">
    import { assetLibrary, PIN_COLORS, type PinColor, type PinKind } from "$lib/assets.svelte";
    import { pinColorVar } from "$lib/pins";

    /**
     * The accent picker for a pinned item. Shared by the folder context menu and
     * the inspector, so a colour means the same thing and looks the same in both.
     *
     * Takes a kind and an ID, not the row itself: every caller would otherwise
     * have to re-fetch after each pick to keep the selected ring honest. The
     * current colour is read from `assetLibrary.pins`, which is the one list
     * that already spans both kinds.
     */
    interface Props {
        kind: PinKind;
        id: string;
    }

    const { kind, id }: Props = $props();

    const pin = $derived(assetLibrary.pins.find((p) => p.kind === kind && p.id === id));

    const swatchClass =
        "h-4 w-4 rounded-full border transition-transform hover:scale-110 " +
        "aria-pressed:ring-2 aria-pressed:ring-neutral-300 aria-pressed:ring-offset-1 " +
        "aria-pressed:ring-offset-neutral-900";

    function pick(color: PinColor | null) {
        void assetLibrary.setPinColor(kind, id, color);
    }
</script>

{#if pin}
    <div class="flex flex-wrap items-center gap-1">
        {#each PIN_COLORS as color (color)}
            <button
                type="button"
                title={color}
                aria-label="Accent {color}"
                aria-pressed={pin.color === color}
                onclick={() => pick(color)}
                class={swatchClass}
                style="border-color: {pinColorVar(color)}; background-color: {pinColorVar(color)}"
            ></button>
        {/each}
        <button
            type="button"
            title="No accent"
            aria-label="No accent"
            aria-pressed={pin.color === null}
            onclick={() => pick(null)}
            class="{swatchClass} border-neutral-600"
        ></button>
    </div>
{/if}
