<script lang="ts">
    import { ChevronDown, Plus, FolderOpen, Library } from "@lucide/svelte";
    import { libraryManager } from "../../routes/settings.svelte";

    /**
     * Library switcher — the sidebar header's content.
     *
     * LAYOUT NOTE: top-left is where users look for "where am I", so the active
     * library's name lives there rather than in a window title we don't have.
     */
    interface Props {
        oncreate: () => void;
        onopen: () => void;
    }

    const { oncreate, onopen }: Props = $props();

    let open = $state(false);

    // The menu is wider than the sidebar column, and `.pane` is `overflow: hidden`
    // (it has to be, or a collapsing pane would paint its content over the grid).
    // So the menu is `fixed` and anchored to the trigger's rect — the same escape
    // the rail flyout uses. RULE: any popover in a pane header that can exceed
    // its column must be fixed, not absolute.
    let anchor = $state({ x: 0, y: 0 });

    function toggle(e: MouseEvent) {
        const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
        anchor = { x: rect.left, y: rect.bottom + 4 };
        open = !open;
    }

    const activeName = $derived(
        libraryManager.state.activeLibrary?.split(/[\\/]/).pop() ?? "No library",
    );
</script>

<div class="min-w-0">
    <button
        type="button"
        onclick={toggle}
        class="flex min-w-0 items-center gap-1.5 rounded-md px-2 py-1 text-sm text-neutral-200
               transition-colors hover:bg-neutral-800"
    >
        <Library class="h-3.5 w-3.5 shrink-0 text-neutral-500" />
        <span class="truncate font-medium">{activeName}</span>
        <ChevronDown class="h-3.5 w-3.5 shrink-0 text-neutral-500" />
    </button>

    {#if open}
        <!-- Click-away backdrop. A real popover (bits-ui) can replace this later;
             for the layout prototype the behaviour is what matters. -->
        <button
            type="button"
            tabindex="-1"
            aria-label="Close menu"
            class="fixed inset-0 z-40 cursor-default"
            onclick={() => (open = false)}
        ></button>

        <div
            class="fixed z-50 w-64 rounded-lg border border-neutral-800 bg-neutral-900 p-1 shadow-2xl"
            style="left: {anchor.x}px; top: {anchor.y}px"
        >
            {#if libraryManager.state.history.length > 0}
                <p class="px-2 py-1.5 text-[10px] font-semibold uppercase tracking-wider text-neutral-500">
                    Recent
                </p>
                {#each libraryManager.state.history as path (path)}
                    <button
                        type="button"
                        onclick={() => {
                            open = false;
                            void libraryManager.switchLibrary(path);
                        }}
                        class="flex w-full items-center rounded px-2 py-1.5 text-left text-sm
                               text-neutral-300 hover:bg-neutral-800
                               {path === libraryManager.state.activeLibrary ? 'text-blue-400' : ''}"
                    >
                        <span class="truncate">{path.split(/[\\/]/).pop() || path}</span>
                    </button>
                {/each}
                <div class="my-1 h-px bg-neutral-800"></div>
            {/if}

            <button
                type="button"
                onclick={() => {
                    open = false;
                    oncreate();
                }}
                class="flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-sm
                       text-neutral-300 hover:bg-neutral-800"
            >
                <Plus class="h-3.5 w-3.5" /> New library
            </button>
            <button
                type="button"
                onclick={() => {
                    open = false;
                    onopen();
                }}
                class="flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-sm
                       text-neutral-300 hover:bg-neutral-800"
            >
                <FolderOpen class="h-3.5 w-3.5" /> Open existing…
            </button>
        </div>
    {/if}
</div>
