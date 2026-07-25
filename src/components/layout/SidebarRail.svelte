<script lang="ts">
    import { FolderTree as FolderTreeIcon, Filter, Tag } from "@lucide/svelte";
    import { layout, type RailSection } from "$lib/layout.svelte";
    import { DRAG_SCROLL_ATTR } from "$lib/dragdrop.svelte";
    import FolderTree from "../FolderTree.svelte";
    import SavedFilters from "../SavedFilters.svelte";

    /**
     * The collapsed sidebar: section icons, plus a hover flyout.
     *
     * LAYOUT NOTE — a rail is not "the sidebar, narrower". A folder tree has no
     * icon-only form (you can't read nesting at 52px), so the rail shows the
     * sidebar's SECTIONS, and hovering one opens the real section in a floating
     * panel beside it. That's what makes the rail usable rather than merely
     * small: you can browse folders without giving the sidebar its width back.
     *
     * Click still expands the sidebar for good — hover is for a look, click is
     * for a stay.
     *
     * The panel is `position: fixed`, not absolute: `.pane` is `overflow: hidden`
     * (it has to be, or a wide tree would push the grid), which would clip any
     * absolutely-positioned child. Fixed elements aren't clipped by an ancestor's
     * overflow, so the flyout escapes the 52px column.
     */
    interface Props {
        onManageTags: () => void;
    }

    const { onManageTags }: Props = $props();

    const SECTIONS: { id: RailSection; icon: typeof Tag; label: string }[] = [
        { id: "folders", icon: FolderTreeIcon, label: "Folders" },
        { id: "filters", icon: Filter, label: "Saved filters" },
        { id: "tags", icon: Tag, label: "Tags" },
    ];

    /** Which section's flyout is open, if any. */
    let hovered = $state<RailSection | null>(null);
    /** Viewport y of the flyout, aligned to the icon that opened it. */
    let flyoutTop = $state(0);

    const FLYOUT_H = 420;
    const FLYOUT_W = 260;

    // A grace period on close, so the diagonal move from the icon to the panel
    // doesn't dismiss it. Without this the flyout is unusable with a mouse.
    let closeTimer: ReturnType<typeof setTimeout> | null = null;

    function cancelClose() {
        if (closeTimer !== null) {
            clearTimeout(closeTimer);
            closeTimer = null;
        }
    }

    function scheduleClose() {
        cancelClose();
        closeTimer = setTimeout(() => (hovered = null), 150);
    }

    function openFlyout(section: RailSection, anchor: HTMLElement) {
        cancelClose();
        const rect = anchor.getBoundingClientRect();
        // Align to the icon, but never let the panel run off the bottom.
        flyoutTop = Math.max(8, Math.min(rect.top - 4, window.innerHeight - FLYOUT_H - 8));
        hovered = section;
    }

    $effect(() => cancelClose);

    const label = $derived(SECTIONS.find((s) => s.id === hovered)?.label ?? "");
</script>

<svelte:window
    onkeydown={(e) => {
        if (e.key === "Escape") hovered = null;
    }}
/>

<nav class="flex flex-1 flex-col items-center gap-1 py-2" onpointerleave={scheduleClose}>
    {#each SECTIONS as section (section.id)}
        {@const Icon = section.icon}
        <button
            type="button"
            title={section.label}
            aria-label={section.label}
            aria-expanded={hovered === section.id}
            onpointerenter={(e) => openFlyout(section.id, e.currentTarget)}
            onfocus={(e) => openFlyout(section.id, e.currentTarget)}
            onclick={() => {
                hovered = null;
                layout.showSection(section.id);
            }}
            class="grid h-9 w-9 place-items-center rounded-md transition-colors
                   {layout.railSection === section.id || hovered === section.id
                ? 'bg-neutral-800 text-neutral-100'
                : 'text-neutral-500 hover:bg-neutral-800/60 hover:text-neutral-300'}"
        >
            <Icon class="h-4 w-4" />
        </button>
    {/each}
</nav>

{#if hovered}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
        class="fixed z-40 flex flex-col overflow-hidden rounded-r-lg rounded-bl-lg border
               border-neutral-800 bg-neutral-900 shadow-2xl"
        style="left: var(--rail-w); top: {flyoutTop}px; width: {FLYOUT_W}px; max-height: {FLYOUT_H}px"
        onpointerenter={cancelClose}
        onpointerleave={scheduleClose}
    >
        <div
            class="flex shrink-0 items-center justify-between border-b border-neutral-800 px-3 py-2"
        >
            <span class="text-[11px] font-semibold uppercase tracking-wider text-neutral-500">
                {label}
            </span>
            <button
                type="button"
                onclick={() => {
                    const section = hovered;
                    hovered = null;
                    if (section) layout.showSection(section);
                }}
                class="rounded px-1.5 py-0.5 text-[11px] text-neutral-500 transition-colors
                       hover:bg-neutral-800 hover:text-neutral-200"
            >
                Pin
            </button>
        </div>

        <!-- Same DRAG_SCROLL_ATTR as the expanded sidebar: a drag hovering the
             panel's edges still needs to be able to scroll it. -->
        <div class="flex-1 overflow-y-auto p-2 [scrollbar-width:thin]" {...{ [DRAG_SCROLL_ATTR]: "" }}>
            {#if hovered === "folders"}
                <FolderTree />
            {:else if hovered === "filters"}
                <SavedFilters />
            {:else}
                <!-- ANTICIPATED: the real sidebar tag list goes here (prerequisite
                     for drag-to-tag). Until then this is the door to the manager. -->
                <button
                    type="button"
                    onclick={() => {
                        hovered = null;
                        onManageTags();
                    }}
                    class="w-full rounded-lg border border-neutral-800 bg-neutral-900/40 px-3 py-2
                           text-left text-sm text-neutral-300 transition-colors hover:bg-neutral-800"
                >
                    Manage tags…
                </button>
            {/if}
        </div>
    </div>
{/if}
