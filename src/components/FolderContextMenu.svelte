<script lang="ts">
    import { Pin, PinOff, FolderPlus, Pencil, Trash2 } from "@lucide/svelte";
    import { assetLibrary, type Folder } from "$lib/assets.svelte";
    import PinSwatches from "./PinSwatches.svelte";

    /**
     * Right-click menu for a folder. Shared by the tree and the pinned list, so
     * "pin this" means the same thing and looks the same wherever you invoke it.
     *
     * Pin and colour are handled here because they're identical everywhere. The
     * structural actions are passed in, because they aren't: deleting from the
     * tree can take a multi-selection with it, and the pinned list has no such
     * concept.
     */
    interface Props {
        folder: Folder;
        /** Viewport coordinates of the click that opened the menu. */
        x: number;
        y: number;
        onclose: () => void;
        onNewSubfolder?: () => void;
        onRename?: () => void;
        onDelete?: () => void;
    }

    const { folder, x, y, onclose, onNewSubfolder, onRename, onDelete }: Props = $props();

    const pinned = $derived(folder.pin_position !== null);

    const MENU_W = 200;
    const MENU_H = 240;

    // Flip toward the window rather than off it. A menu opened near the bottom
    // edge is the common case for the last pin in a full list.
    const left = $derived(Math.min(x, window.innerWidth - MENU_W - 8));
    const top = $derived(Math.min(y, window.innerHeight - MENU_H - 8));

    /**
     * INVARIANT: every handler does its work FIRST and closes LAST.
     *
     * Svelte 5 props are lazy getters, not values, and the parent closes this
     * menu by nulling the state its props read from — so `folder.id` read after
     * `onclose()` re-runs that getter against null and throws. Ordering is the
     * whole fix; there's nothing to snapshot as long as it's respected.
     */
    function run(action?: () => void) {
        action?.();
        onclose();
    }

    function togglePin() {
        void assetLibrary.setPinned("folder", folder.id, !pinned);
        onclose();
    }
</script>

<svelte:window
    onkeydown={(e) => {
        if (e.key === "Escape") onclose();
    }}
/>

<!-- Click-away catcher. `oncontextmenu` too, so a second right-click somewhere
     else closes this menu instead of stacking another one on top of it. -->
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
    <p class="nv-menu-label truncate">{folder.name}</p>

    <button type="button" role="menuitem" onclick={togglePin} class="nv-menu-item">
        {#if pinned}
            <PinOff class="h-3.5 w-3.5 text-neutral-500" /> Unpin from sidebar
        {:else}
            <Pin class="h-3.5 w-3.5 text-neutral-500" /> Pin to sidebar
        {/if}
    </button>

    {#if pinned}
        <!-- Colours only appear once pinned: an accent on an unpinned folder has
             nowhere to show, so offering it would be a setting with no effect.
             Picking one deliberately does NOT close the menu — trying two greens
             against each other is the normal way to choose. -->
        <div class="px-2 py-2">
            <PinSwatches kind="folder" id={folder.id} />
        </div>
    {/if}

    {#if onNewSubfolder || onRename || onDelete}
        <div class="nv-menu-sep"></div>
    {/if}

    {#if onNewSubfolder}
        <button type="button" role="menuitem" onclick={() => run(onNewSubfolder)} class="nv-menu-item">
            <FolderPlus class="h-3.5 w-3.5 text-neutral-500" /> New subfolder
        </button>
    {/if}
    {#if onRename}
        <button type="button" role="menuitem" onclick={() => run(onRename)} class="nv-menu-item">
            <Pencil class="h-3.5 w-3.5 text-neutral-500" /> Rename…
        </button>
    {/if}
    {#if onDelete}
        <!-- `!`: .nv-menu-item sets its own colour outside Tailwind's layers,
             so a plain utility would lose to it. -->
        <button
            type="button"
            role="menuitem"
            onclick={() => run(onDelete)}
            class="nv-menu-item text-red-400!"
        >
            <Trash2 class="h-3.5 w-3.5" /> Delete…
        </button>
    {/if}
</div>
