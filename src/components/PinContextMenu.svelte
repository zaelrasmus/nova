<script lang="ts">
    import { PinOff } from "@lucide/svelte";
    import { assetLibrary, PIN_COLORS, type PinColor, type PinnedItem } from "$lib/assets.svelte";
    import { pinColorVar } from "$lib/pins";

    /**
     * Right-click menu for a pin, whichever kind it points at.
     *
     * Only unpin and accent, because those are the two things every pin has.
     * A folder's structural actions (rename, subfolder, delete) stay in
     * FolderContextMenu on the tree, where the multi-selection they can act on
     * actually lives — offering "delete" from a shortcut is also a good way for
     * someone to destroy a folder they meant to unpin.
     */
    interface Props {
        pin: PinnedItem;
        x: number;
        y: number;
        onclose: () => void;
    }

    const { pin, x, y, onclose }: Props = $props();

    const MENU_W = 200;
    const MENU_H = 120;

    const left = $derived(Math.min(x, window.innerWidth - MENU_W - 8));
    const top = $derived(Math.min(y, window.innerHeight - MENU_H - 8));

    // Same invariant as FolderContextMenu: do the work FIRST and close LAST.
    // Props are lazy getters, and the parent closes this by nulling the state
    // they read from, so a prop read after `onclose()` throws.
    function unpin() {
        void assetLibrary.setPinned(pin.kind, pin.id, false);
        onclose();
    }
</script>

<svelte:window
    onkeydown={(e) => {
        if (e.key === "Escape") onclose();
    }}
/>

<button
    type="button"
    tabindex="-1"
    aria-label="Close menu"
    class="fixed inset-0 z-[90] cursor-default"
    onclick={onclose}
    oncontextmenu={(e) => {
        e.preventDefault();
        onclose();
    }}
></button>

<div
    role="menu"
    tabindex="-1"
    class="fixed z-[91] w-[200px] rounded-lg border border-neutral-800 bg-neutral-900 p-1 shadow-2xl"
    style="left: {left}px; top: {top}px"
>
    <p class="truncate px-2 py-1.5 text-[11px] font-semibold text-neutral-500">{pin.name}</p>

    <button
        type="button"
        role="menuitem"
        onclick={unpin}
        class="flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-sm text-neutral-300
               hover:bg-neutral-800"
    >
        <PinOff class="h-3.5 w-3.5" /> Unpin from sidebar
    </button>

    <!-- Picking a colour deliberately does NOT close the menu — trying two
         greens against each other is the normal way to choose one. -->
    <div class="flex flex-wrap items-center gap-1 px-2 py-2">
        {#each PIN_COLORS as color (color)}
            <button
                type="button"
                title={color}
                aria-label="Accent {color}"
                aria-pressed={pin.color === color}
                onclick={() => assetLibrary.setPinColor(pin.kind, pin.id, color as PinColor)}
                class="h-4 w-4 rounded-full border transition-transform hover:scale-110
                       {pin.color === color
                    ? 'ring-2 ring-neutral-300 ring-offset-1 ring-offset-neutral-900'
                    : ''}"
                style="border-color: {pinColorVar(color)}; background-color: {pinColorVar(color)}"
            ></button>
        {/each}
        <button
            type="button"
            title="No accent"
            aria-label="No accent"
            aria-pressed={pin.color === null}
            onclick={() => assetLibrary.setPinColor(pin.kind, pin.id, null)}
            class="h-4 w-4 rounded-full border border-neutral-600 transition-transform
                   hover:scale-110 {pin.color === null
                ? 'ring-2 ring-neutral-300 ring-offset-1 ring-offset-neutral-900'
                : ''}"
        ></button>
    </div>
</div>
