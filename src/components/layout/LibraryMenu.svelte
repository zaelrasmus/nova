<script lang="ts">
    import { ChevronsUpDown, Plus, FolderOpen, Check } from "@lucide/svelte";
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

    /** The badge's letter: something to recognise a library by at a glance. */
    const initial = $derived(activeName.trim().charAt(0).toUpperCase() || "?");
</script>

<div class="min-w-0">
    <button
        type="button"
        onclick={toggle}
        title="Switch library"
        class="flex h-8 min-w-0 items-center gap-2 rounded-lg pl-1 pr-2 text-[13px] text-neutral-100
               transition-colors hover:bg-wash {open ? 'bg-wash-strong' : ''}"
    >
        <span
            aria-hidden="true"
            class="grid h-6 w-6 shrink-0 place-items-center rounded-[7px] text-xs font-bold
                   text-white shadow-[inset_0_0_0_1px_rgb(255_255_255/0.18),0_1px_4px_rgb(0_0_0/0.4)]"
            style="background: linear-gradient(140deg, var(--nv-accent), color-mix(in oklch, var(--nv-accent) 45%, #ff7a59))"
        >
            {initial}
        </span>
        <span class="truncate font-semibold">{activeName}</span>
        <ChevronsUpDown class="h-3.5 w-3.5 shrink-0 text-neutral-500" />
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

        <div class="nv-menu fixed z-50 w-64" style="left: {anchor.x}px; top: {anchor.y}px">
            {#if libraryManager.state.history.length > 0}
                <p class="nv-menu-label">Recent</p>
                {#each libraryManager.state.history as path (path)}
                    {@const current = path === libraryManager.state.activeLibrary}
                    <button
                        type="button"
                        title={path}
                        aria-current={current ? "true" : undefined}
                        onclick={() => {
                            open = false;
                            void libraryManager.switchLibrary(path);
                        }}
                        class="nv-menu-item"
                    >
                        <span class="min-w-0 flex-1 truncate">{path.split(/[\\/]/).pop() || path}</span>
                        {#if current}
                            <Check class="h-3.5 w-3.5 shrink-0 text-brand-text" />
                        {/if}
                    </button>
                {/each}
                <div class="nv-menu-sep"></div>
            {/if}

            <button
                type="button"
                onclick={() => {
                    open = false;
                    oncreate();
                }}
                class="nv-menu-item"
            >
                <Plus class="h-3.5 w-3.5 text-neutral-500" /> New library
            </button>
            <button
                type="button"
                onclick={() => {
                    open = false;
                    onopen();
                }}
                class="nv-menu-item"
            >
                <FolderOpen class="h-3.5 w-3.5 text-neutral-500" /> Open existing…
            </button>
        </div>
    {/if}
</div>
