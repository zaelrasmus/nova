<script lang="ts">
    import { Type } from "@lucide/svelte";

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
        x: number;
        y: number;
        onclose: () => void;
        onRename: () => void;
    }

    const { count, x, y, onclose, onRename }: Props = $props();

    const MENU_W = 200;
    const MENU_H = 90;

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

    <button
        type="button"
        role="menuitem"
        onclick={() => {
            onRename();
            onclose();
        }}
        class="flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-sm text-neutral-300
               hover:bg-neutral-800"
    >
        <Type class="h-3.5 w-3.5" /> Rename…
    </button>
</div>
