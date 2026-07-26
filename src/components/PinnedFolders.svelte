<script lang="ts">
    import { Folder as FolderIcon } from "@lucide/svelte";
    import { assetLibrary, type Folder } from "$lib/assets.svelte";
    import { DROP_FOLDER_ATTR, DROP_FOLDER_NAME_ATTR } from "$lib/droptarget";
    import { drag, DRAG_SCROLL_ATTR } from "$lib/dragdrop.svelte";
    import FolderTree from "./FolderTree.svelte";
    import { dropzone } from "$lib/dropzone.svelte";
    import { pinColorVar } from "$lib/pins";
    import FolderContextMenu from "./FolderContextMenu.svelte";

    /**
     * The pinned folders — the sidebar's curated shortlist.
     *
     * Renders in both sidebar modes from one component, because a pin has to
     * behave identically in each: same order, same accent, same drop behaviour.
     * Only the amount of room differs, so only the row template does.
     *
     * DROP TARGET: each pin carries the same attributes as a tree row, so
     * dragging assets onto one files them there and an OS file drop imports into
     * it — the coordinate hit-test in droptarget.ts finds these without any
     * knowledge of the sidebar. This is the payoff of pinning: your handful of
     * real destinations, always one drop away, without opening the tree.
     */
    interface Props {
        variant: "rail" | "expanded";
        /**
         * A pin's flyout just opened. The rail uses this to close its own
         * section flyout — only one panel may occupy the space beside the rail,
         * or the two overlap and the newer one is unreadable.
         */
        onFlyoutOpen?: () => void;
    }

    const { variant, onFlyoutOpen }: Props = $props();

    const pins = $derived(assetLibrary.pinned);
    const scope = $derived(assetLibrary.scope);

    const parentOf = $derived(new Map(assetLibrary.folders.map((f) => [f.id, f.parent_id])));

    /** The folder currently being browsed, if the scope is a folder at all. */
    const activeFolder = $derived(scope.kind === "folder" ? scope.id : null);

    /**
     * Is the current scope *inside* this pin, without being it?
     *
     * Needs its own state, distinct from "active": drill two levels into a
     * pinned folder and every pin would otherwise look equally unselected, and
     * you'd lose track of where you are.
     */
    function containsActive(pinId: string): boolean {
        if (!activeFolder || activeFolder === pinId) return false;
        let cursor = parentOf.get(activeFolder) ?? null;
        while (cursor) {
            if (cursor === pinId) return true;
            cursor = parentOf.get(cursor) ?? null;
        }
        return false;
    }

    // ── Context menu ────────────────────────────────────────────────────────
    let menu = $state<{ folder: Folder; x: number; y: number } | null>(null);

    // ── Hover flyout (rail only) ────────────────────────────────────────────
    //
    // A pin in the rail is a 36px icon: enough to recognise, not enough to
    // navigate. Hovering opens its subtree beside it — the same "hover looks,
    // click stays" rule the section icons follow, so the rail stays browsable
    // without giving the sidebar its width back.
    let flyout = $state<{ folder: Folder; top: number } | null>(null);

    const FLYOUT_H = 420;
    const FLYOUT_W = 260;

    let closeTimer: ReturnType<typeof setTimeout> | null = null;

    function cancelClose() {
        if (closeTimer !== null) {
            clearTimeout(closeTimer);
            closeTimer = null;
        }
    }

    // A grace period, so the diagonal move from icon to panel doesn't dismiss it.
    function scheduleClose() {
        cancelClose();
        closeTimer = setTimeout(() => (flyout = null), 150);
    }

    function openFlyout(folder: Folder, anchor: HTMLElement) {
        // Not while rearranging: a flyout appearing under a dragged pin would
        // cover the very list the drop is aiming at.
        if (variant !== "rail" || dragId) return;
        cancelClose();
        const rect = anchor.getBoundingClientRect();
        flyout = {
            folder,
            top: Math.max(8, Math.min(rect.top - 4, window.innerHeight - FLYOUT_H - 8)),
        };
        onFlyoutOpen?.();
    }

    /** Close from outside, when the rail's own flyout is taking over the space. */
    export function closeFlyout() {
        cancelClose();
        flyout = null;
    }

    $effect(() => cancelClose);

    // ── Drag to reorder ─────────────────────────────────────────────────────
    //
    // Deliberately NOT the app-wide drag system. That one answers "what does
    // dropping these assets on that folder mean"; this is a list rearranging
    // itself, with no payload and no cross-surface target. Keeping them separate
    // means dragging a pin can't be mistaken for filing a folder into another.
    let listEl = $state<HTMLElement | null>(null);
    let pressId: string | null = null;
    let pressY = 0;
    let dragId = $state<string | null>(null);
    let dragY = $state(0);
    /** Midpoints of the pins that are staying put, measured once per drag. */
    let midpoints = $state<{ id: string; mid: number }[]>([]);
    /** A drag just ended — swallow the click that pointer-up is about to fire. */
    let justDragged = false;

    /** Where the dragged pin would land: the number of stationary pins above it. */
    const insertAt = $derived.by(() => {
        if (!dragId) return -1;
        return midpoints.filter((m) => m.mid < dragY).length;
    });

    /**
     * Does the insertion line go above the pin rendered at index `i`?
     *
     * The dragged pin stays rendered (removing it would shift every row and
     * invalidate the midpoints measured at drag start), so the rendered indices
     * and `insertAt` — which counts only the pins staying put — don't line up.
     * This is that translation, and the reason it can't just be `insertAt === i`.
     */
    function lineBefore(i: number): boolean {
        if (!dragId) return false;
        const dragIdx = pins.findIndex((p) => p.id === dragId);
        return insertAt === (i > dragIdx ? i - 1 : i);
    }

    function startReorder(e: PointerEvent, id: string) {
        if (e.button !== 0) return;
        // Cleared here rather than in the click handler: a drag that ends
        // outside the list fires no click, and a stale flag would then eat the
        // user's next real one.
        justDragged = false;
        pressId = id;
        pressY = e.clientY;
        (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    }

    function moveReorder(e: PointerEvent) {
        if (!pressId) return;
        // A 4px threshold, so a click that wobbles is still a click. Without it
        // every navigation would be a no-op reorder.
        if (!dragId && Math.abs(e.clientY - pressY) < 4) return;
        if (!dragId) {
            dragId = pressId;
            midpoints = [...(listEl?.querySelectorAll<HTMLElement>("[data-pin-id]") ?? [])]
                .filter((el) => el.dataset.pinId !== dragId)
                .map((el) => {
                    const r = el.getBoundingClientRect();
                    return { id: el.dataset.pinId!, mid: r.top + r.height / 2 };
                });
        }
        dragY = e.clientY;
    }

    async function endReorder() {
        const moved = dragId;
        const at = insertAt;
        pressId = null;
        dragId = null;
        if (!moved || at < 0) return;
        justDragged = true;
        // `after` is the pin it lands behind; null means it becomes the first.
        await assetLibrary.reorderPin(moved, at > 0 ? midpoints[at - 1].id : null);
    }

    function open(folder: Folder) {
        // A reorder's pointer-up fires a click too, which would otherwise
        // navigate to whichever pin happened to end up under the cursor.
        if (justDragged) {
            justDragged = false;
            return;
        }
        void assetLibrary.setScope({ kind: "folder", id: folder.id });
    }
</script>

{#if pins.length > 0}
    <!-- In the RAIL, pins are the one variable-length list, so they take the
         leftover height and scroll inside it — no item cap, because a cap tuned
         for one screen height is wrong on the next monitor.

         EXPANDED is the opposite: the sidebar's own <nav> scrolls, and the tree
         below is the list that grows. Claiming flex-1 here would make one pin
         occupy the height of five and shove the folder tree off the screen. -->
    <!-- The leave handler sits on the whole list, not each pin: moving between
         two pins must not flicker the flyout closed and open again. -->
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
        bind:this={listEl}
        onpointerleave={scheduleClose}
        class="flex flex-col gap-0.5
               {variant === 'rail'
            ? 'min-h-0 flex-1 items-center overflow-y-auto py-1 [scrollbar-width:thin]'
            : 'shrink-0 px-2 pb-2'}"
    >
        {#each pins as pin, i (pin.id)}
            {@const active = activeFolder === pin.id}
            {@const inside = containsActive(pin.id)}
            {@const dropping = dropzone.isOverFolder(pin.id) || drag.isOverFolder(pin.id)}
            {@const accent = pinColorVar(pin.color)}

            <!-- Insertion line for the reorder in progress. -->
            {#if lineBefore(i)}
                <div class="h-0.5 shrink-0 rounded bg-blue-500"></div>
            {/if}

            <div
                data-pin-id={pin.id}
                {...{ [DROP_FOLDER_ATTR]: pin.id, [DROP_FOLDER_NAME_ATTR]: pin.name }}
                class="shrink-0 {dragId === pin.id ? 'opacity-40' : ''}"
            >
                <button
                    type="button"
                    title={pin.name}
                    aria-current={active ? "true" : undefined}
                    onpointerdown={(e) => startReorder(e, pin.id)}
                    onpointermove={moveReorder}
                    onpointerup={endReorder}
                    onpointercancel={endReorder}
                    onpointerenter={(e) => openFlyout(pin, e.currentTarget)}
                    onfocus={(e) => openFlyout(pin, e.currentTarget)}
                    onclick={() => open(pin)}
                    oncontextmenu={(e) => {
                        e.preventDefault();
                        menu = { folder: pin, x: e.clientX, y: e.clientY };
                    }}
                    class="flex items-center transition-colors
                           {variant === 'rail'
                        ? 'h-9 w-9 justify-center rounded-md'
                        : 'w-full gap-2 rounded px-2 py-1 text-left text-sm'}
                           {dropping
                        ? 'bg-emerald-600/25 ring-1 ring-emerald-500'
                        : active
                          ? 'bg-neutral-800'
                          : 'hover:bg-neutral-800/60'}"
                >
                    <!-- The accent lives on the icon, not the row: a full-width
                         tinted row would fight the selection highlight, and the
                         glyph is what you actually scan for in the rail. -->
                    <span class="relative flex shrink-0 items-center" style="color: {accent}">
                        <FolderIcon class="h-4 w-4" strokeWidth={1.5} />
                        {#if inside}
                            <!-- "You're somewhere below this pin." Quieter than
                                 the active state on purpose — it's a trail, not
                                 a destination. -->
                            <span
                                class="absolute -bottom-0.5 left-1/2 h-1 w-1 -translate-x-1/2
                                       rounded-full bg-current"
                            ></span>
                        {/if}
                    </span>

                    {#if variant === "expanded"}
                        <span
                            class="truncate {active ? 'text-neutral-100' : 'text-neutral-300'}"
                        >
                            {pin.name}
                        </span>
                    {/if}
                </button>
            </div>
        {/each}

        <!-- Dropping past the last pin: `insertAt` has run off the end of the
             stationary list, so no row can claim the line. -->
        {#if dragId && insertAt === midpoints.length}
            <div class="h-0.5 shrink-0 rounded bg-blue-500"></div>
        {/if}
    </div>
{:else if variant === "expanded"}
    <p class="px-3 pb-2 text-xs text-neutral-600">
        Right-click a folder to pin it here.
    </p>
{/if}

{#if flyout}
    <!-- Fixed, not absolute: `.pane` is `overflow: hidden`, which would clip an
         absolutely-positioned child inside the 52px rail. -->
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
        data-rail-flyout
        class="fixed z-[85] flex flex-col overflow-hidden rounded-r-lg rounded-bl-lg border
               border-neutral-800 bg-neutral-900 shadow-2xl"
        style="left: var(--rail-w); top: {flyout.top}px; width: {FLYOUT_W}px;
               max-height: {FLYOUT_H}px"
        onpointerenter={cancelClose}
        onpointerleave={scheduleClose}
    >
        <div
            class="flex shrink-0 items-center justify-between gap-2 border-b border-neutral-800
                   px-3 py-2"
        >
            <span class="flex min-w-0 items-center gap-1.5">
                <span
                    class="h-1.5 w-1.5 shrink-0 rounded-full"
                    style="background-color: {pinColorVar(flyout.folder.color)}"
                ></span>
                <span class="truncate text-[11px] font-semibold uppercase tracking-wider
                             text-neutral-400">
                    {flyout.folder.name}
                </span>
            </span>
            <button
                type="button"
                onclick={() => {
                    const target = flyout!.folder;
                    flyout = null;
                    void assetLibrary.setScope({ kind: "folder", id: target.id });
                }}
                class="shrink-0 rounded px-1.5 py-0.5 text-[11px] text-neutral-500
                       transition-colors hover:bg-neutral-800 hover:text-neutral-200"
            >
                Open
            </button>
        </div>

        <!-- The pin's own subtree, fully interactive: same tree component, just
             rooted here instead of at the library. Drag, drop, rename and the
             context menu all work exactly as they do in the full sidebar. -->
        <div
            class="flex-1 overflow-y-auto p-2 [scrollbar-width:thin]"
            {...{ [DRAG_SCROLL_ATTR]: "" }}
        >
            <FolderTree rootId={flyout.folder.id} />
        </div>
    </div>
{/if}

{#if menu}
    <FolderContextMenu
        folder={menu.folder}
        x={menu.x}
        y={menu.y}
        onclose={() => (menu = null)}
    />
{/if}
