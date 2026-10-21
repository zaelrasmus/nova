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
    class="nv-menu fixed z-[91] w-[200px]"
    style="left: {left}px; top: {top}px"
>
    <p class="nv-menu-label truncate">{pin.name}</p>

    <button type="button" role="menuitem" onclick={unpin} class="nv-menu-item">
        <PinOff class="h-3.5 w-3.5 text-neutral-500" /> Unpin from sidebar
    </button>

    <!-- Picking a colour deliberately does NOT close the menu — trying two
         greens against each other is the normal way to choose one. -->
    <div class="flex flex-wrap items-center gap-1.5 px-2 py-2">
        {#each PIN_COLORS as color (color)}
            <button
                type="button"
                title={color}
                aria-label="Accent {color}"
                aria-pressed={pin.color === color}
                onclick={() => assetLibrary.setPinColor(pin.kind, pin.id, color as PinColor)}
                class="h-4 w-4 rounded-full border transition-transform hover:scale-110
                       {pin.color === color
                    ? 'outline-2 outline-offset-2 outline-neutral-200'
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
                ? 'outline-2 outline-offset-2 outline-neutral-200'
                : ''}"
        ></button>
    </div>
</div>
