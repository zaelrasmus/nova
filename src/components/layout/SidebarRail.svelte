<script lang="ts">
    import { Layers, Filter, Tag, FolderTree as FolderTreeIcon } from "@lucide/svelte";
    import { layout } from "$lib/layout.svelte";
    import { DRAG_SCROLL_ATTR } from "$lib/dragdrop.svelte";
    import FolderTree from "../FolderTree.svelte";
    import SavedFilters from "../SavedFilters.svelte";
    import SystemViews from "../SystemViews.svelte";
    import PinnedFolders from "../PinnedFolders.svelte";

    /**
     * The collapsed sidebar.
     *
     *   ▤  Library        smart views (All assets, Uncategorized)
     *   ⚗  Saved filters
     *   🏷 Tags
     *  ─────────────────  the one separator: chrome above, folders below
     *   📁 Folder tree     hover to peek · click to keep the peek open
     *   ▪  pinned folders  flex-1, scrolls; each one peeks at its own subtree
     *
     * WHY THIS SHAPE — a rail earns its keep through spatial memory: few icons,
     * stable positions, each distinguishable. So the exhaustive folder tree stays
     * behind ONE icon (hovering it shows every name at once, which beats hunting
     * through identical glyphs), and only the user's own shortlist gets icons of
     * its own, told apart by an accent colour.
     *
     * Note the top group holds a LIBRARY glyph, not a folder one. Two folder
     * icons meaning different things is exactly the discrimination problem the
     * pins are designed to avoid.
     */
    interface Props {
        onManageTags: () => void;
    }

    const { onManageTags }: Props = $props();

    type Flyout = "library" | "filters" | "tags" | "tree";

    const SECTIONS: { id: Flyout; icon: typeof Tag; label: string }[] = [
        { id: "library", icon: Layers, label: "Library" },
        { id: "filters", icon: Filter, label: "Saved filters" },
        { id: "tags", icon: Tag, label: "Tags" },
    ];

    /** Which flyout is open, if any. */
    let hovered = $state<Flyout | null>(null);
    /** Viewport y of the flyout, aligned to the icon that opened it. */
    let flyoutTop = $state(0);
    /**
     * The flyout was opened by a CLICK and stays until dismissed.
     *
     * Clicking a rail icon used to expand the sidebar, which was backwards:
     * being in the rail is a choice the user made, and a click shouldn't undo
     * it. Now hover peeks, click keeps that peek open, and expanding is its own
     * explicit button in the flyout header.
     */
    let sticky = $state(false);

    /** The pinned list, so its flyout can be dismissed when a section takes over. */
    let pinsRef = $state<{ closeFlyout: () => void } | null>(null);

    const FLYOUT_H = 460;
    const FLYOUT_W = 260;

    // A grace period on close, so the diagonal move from icon to panel doesn't
    // dismiss it. Without this the flyout is unusable with a mouse.
    let closeTimer: ReturnType<typeof setTimeout> | null = null;

    function cancelClose() {
        if (closeTimer !== null) {
            clearTimeout(closeTimer);
            closeTimer = null;
        }
    }

    function scheduleClose() {
        if (sticky) return; // a clicked-open panel outlives the pointer
        cancelClose();
        closeTimer = setTimeout(() => (hovered = null), 150);
    }

    function dismiss() {
        cancelClose();
        sticky = false;
        hovered = null;
    }

    function openFlyout(section: Flyout, anchor: HTMLElement) {
        cancelClose();
        // Hovering a DIFFERENT icon demotes a stuck panel back to a peek —
        // otherwise one click would leave the rail permanently pinned open.
        if (hovered !== section) sticky = false;
        const rect = anchor.getBoundingClientRect();
        // Align to the icon, but never let the panel run off the bottom.
        flyoutTop = Math.max(8, Math.min(rect.top - 4, window.innerHeight - FLYOUT_H - 8));
        hovered = section;
        pinsRef?.closeFlyout();
    }

    /** Click: keep this peek open, or close it if it already is. */
    function toggleSticky(section: Flyout, anchor: HTMLElement) {
        if (hovered === section && sticky) {
            dismiss();
            return;
        }
        openFlyout(section, anchor);
        sticky = true;
    }

    $effect(() => cancelClose);

    const label = $derived(
        hovered === "tree" ? "Folders" : (SECTIONS.find((s) => s.id === hovered)?.label ?? ""),
    );
</script>

<svelte:window
    onkeydown={(e) => {
        if (e.key === "Escape") dismiss();
    }}
    onclick={(e) => {
        // A clicked-open panel ignores pointerleave, so it needs a click-away.
        if (!sticky) return;
        const el = e.target as HTMLElement | null;
        if (el?.closest("[data-rail],[data-rail-flyout]")) return;
        dismiss();
    }}
/>

<!-- The leave handler is on the whole rail, not each button: moving between two
     icons must not flicker the flyout closed and open again. -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="flex min-h-0 flex-1 flex-col" data-rail onpointerleave={scheduleClose}>
    <!-- Chrome: fixed destinations that never move. -->
    <nav class="flex shrink-0 flex-col items-center gap-1 py-1">
        {#each SECTIONS as section (section.id)}
            {@const Icon = section.icon}
            <button
                type="button"
                title={section.label}
                aria-label={section.label}
                aria-expanded={hovered === section.id}
                onpointerenter={(e) => openFlyout(section.id, e.currentTarget)}
                onfocus={(e) => openFlyout(section.id, e.currentTarget)}
                onclick={(e) => toggleSticky(section.id, e.currentTarget)}
                class="grid h-9 w-9 place-items-center rounded-md transition-colors
                       {hovered === section.id
                    ? 'bg-neutral-800 text-neutral-100'
                    : 'text-neutral-500 hover:bg-neutral-800/60 hover:text-neutral-300'}"
            >
                <Icon class="h-4 w-4" strokeWidth={1.5} />
            </button>
        {/each}
    </nav>

    <div class="mx-3 my-1 h-px shrink-0 bg-neutral-800"></div>

    <!-- Folder-land: the tree behind one icon, then the user's shortlist. -->
    <div class="flex shrink-0 flex-col items-center pb-1">
        <button
            type="button"
            title="Folders"
            aria-label="Folders"
            aria-expanded={hovered === "tree"}
            onpointerenter={(e) => openFlyout("tree", e.currentTarget)}
            onfocus={(e) => openFlyout("tree", e.currentTarget)}
            onclick={(e) => toggleSticky("tree", e.currentTarget)}
            class="grid h-9 w-9 place-items-center rounded-md transition-colors
                   {hovered === 'tree'
                ? 'bg-neutral-800 text-neutral-100'
                : 'text-neutral-500 hover:bg-neutral-800/60 hover:text-neutral-300'}"
        >
            <FolderTreeIcon class="h-4 w-4" strokeWidth={1.5} />
        </button>
    </div>

    <!-- The two panels are mutually exclusive: they occupy the same strip beside
         the rail, so whichever opens closes the other. -->
    <PinnedFolders
        bind:this={pinsRef}
        variant="rail"
        onFlyoutOpen={() => {
            cancelClose();
            sticky = false;
            hovered = null;
        }}
    />
</div>

{#if hovered}
    <!-- Fixed, not absolute: `.pane` is `overflow: hidden` (it has to be, or a
         collapsing pane would paint over the grid), which would clip an
         absolutely-positioned child. Fixed escapes the 52px column. -->
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
        data-rail-flyout
        class="fixed z-[85] flex flex-col overflow-hidden rounded-r-lg rounded-bl-lg border
               border-neutral-800 bg-neutral-900 shadow-2xl"
        style="left: var(--rail-w); top: {flyoutTop}px; width: {FLYOUT_W}px; max-height: {FLYOUT_H}px"
        onpointerenter={cancelClose}
        onpointerleave={scheduleClose}
    >
        <div class="flex shrink-0 items-center justify-between border-b border-neutral-800 px-3 py-2">
            <span class="text-[11px] font-semibold uppercase tracking-wider text-neutral-500">
                {label}
            </span>
            <!-- The ONLY thing that gives the sidebar its width back. Expanding
                 is a deliberate act now, not a side effect of clicking an icon. -->
            <button
                type="button"
                title="Expand the sidebar"
                onclick={() => {
                    dismiss();
                    layout.expand();
                }}
                class="rounded px-1.5 py-0.5 text-[11px] text-neutral-500 transition-colors
                       hover:bg-neutral-800 hover:text-neutral-200"
            >
                Expand
            </button>
        </div>

        <!-- Same DRAG_SCROLL_ATTR as the expanded sidebar: a drag hovering the
             panel's edges still needs to be able to scroll it. -->
        <div class="flex-1 overflow-y-auto p-2 [scrollbar-width:thin]" {...{ [DRAG_SCROLL_ATTR]: "" }}>
            {#if hovered === "library"}
                <SystemViews variant="expanded" />
            {:else if hovered === "tree"}
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
