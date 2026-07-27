<script lang="ts">
    import { Type, Trash2, Undo2, Flame } from "@lucide/svelte";

    /**
     * Right-click menu for a selection of assets.
     *
     * Starts with one entry, and that's fine — it exists so the operations with
     * no other home have one. Rename is the first because it's the only bulk
     * edit multi-select genuinely cannot do.
     *
     * Same conventions as `FolderContextMenu`: flip toward the window, close on
     * a second right-click, and do the work FIRST and close LAST (props are lazy
     * getters, and the parent closes this by nulling the state they read from).
     */
    interface Props {
        /** How many assets the menu acts on — the selection at open time. */
        count: number;
        /** Inside the Trash the verbs are different: restore, or delete for good. */
        inTrash: boolean;
        x: number;
        y: number;
        onclose: () => void;
        onRename: () => void;
        onTrash: () => void;
        onRestore: () => void;
        onPurge: () => void;
    }

    const { count, inTrash, x, y, onclose, onRename, onTrash, onRestore, onPurge }: Props =
        $props();

    const MENU_W = 200;
    const MENU_H = 160;

    /** Do the work FIRST and close LAST — props are lazy getters. */
    function run(action: () => void) {
        action();
        onclose();
    }

    const left = $derived(Math.min(x, window.innerWidth - MENU_W - 8));
    const top = $derived(Math.min(y, window.innerHeight - MENU_H - 8));
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
    <p class="truncate px-2 py-1.5 text-[11px] font-semibold text-neutral-500">
        {count.toLocaleString()}
        {count === 1 ? "asset" : "assets"}
    </p>

    {#if inTrash}
        <button
            type="button"
            role="menuitem"
            onclick={() => run(onRestore)}
            class="flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-sm
                   text-neutral-300 hover:bg-neutral-800"
        >
            <Undo2 class="h-3.5 w-3.5" /> Restore
        </button>

        <div class="my-1 h-px bg-neutral-800"></div>

        <!-- The only irreversible action in the app, so it's the only one styled
             like one and the only one that always confirms. -->
        <button
            type="button"
            role="menuitem"
            onclick={() => run(onPurge)}
            class="flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-sm
                   text-red-400 hover:bg-neutral-800"
        >
            <Flame class="h-3.5 w-3.5" /> Delete permanently…
        </button>
    {:else}
        <button
            type="button"
            role="menuitem"
            onclick={() => run(onRename)}
            class="flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-sm
                   text-neutral-300 hover:bg-neutral-800"
        >
            <Type class="h-3.5 w-3.5" /> Rename…
        </button>

        <div class="my-1 h-px bg-neutral-800"></div>

        <button
            type="button"
            role="menuitem"
            onclick={() => run(onTrash)}
            class="flex w-full items-center justify-between gap-2 rounded px-2 py-1.5 text-left
                   text-sm text-neutral-300 hover:bg-neutral-800"
        >
            <span class="flex items-center gap-2">
                <Trash2 class="h-3.5 w-3.5" /> Move to Trash
            </span>
            <span class="text-[10px] text-neutral-600">Del</span>
        </button>
    {/if}
</div>
