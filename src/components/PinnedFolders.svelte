<script lang="ts">
    import { Folder as FolderIcon, Sparkles } from "@lucide/svelte";
    import {
        assetLibrary,
        thumbHashUrl,
        type AssetLightRow,
        type PinnedItem,
    } from "$lib/assets.svelte";
    import {
        DROP_FOLDER_ATTR,
        DROP_FOLDER_NAME_ATTR,
        DROP_SMART_ATTR,
        DROP_SMART_NAME_ATTR,
    } from "$lib/droptarget";
    import { drag, DRAG_SCROLL_ATTR } from "$lib/dragdrop.svelte";
    import FolderTree from "./FolderTree.svelte";
    import { dropzone } from "$lib/dropzone.svelte";
    import { pinColorVar } from "$lib/pins";
    import { describeConditions } from "$lib/rules";
    import PinContextMenu from "./PinContextMenu.svelte";

    /**
     * The sidebar's curated shortlist — folders AND smart folders, in one order.
     *
     * Renders in both sidebar modes from one component, because a pin has to
     * behave identically in each: same order, same accent, same drop behaviour.
     * Only the amount of room differs, so only the row template does.
     *
     * DROP TARGETS, and the reason the two kinds differ:
     *   • a folder pin carries the same attributes as a tree row, so dragging
     *     assets onto one files them there and an OS drop imports into it. This
     *     is the payoff of pinning — your real destinations, one drop away.
     *   • a smart pin carries DROP_SMART_ATTR instead, which every validator
     *     REFUSES. You can't put something into a query. Marking it (rather than
     *     leaving it unmarked so drops quietly do nothing) is what lets the drag
     *     preview explain why.
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

    const pins = $derived(assetLibrary.pins);
    const scope = $derived(assetLibrary.scope);

    /** Pins span two tables, so identity is the PAIR, never the id alone. */
    const refKey = (p: { kind: string; id: string }) => `${p.kind}:${p.id}`;

    const parentOf = $derived(new Map(assetLibrary.folders.map((f) => [f.id, f.parent_id])));
    const activeFolder = $derived(scope.kind === "folder" ? scope.id : null);

    function isActive(pin: PinnedItem): boolean {
        if (pin.kind === "folder") return activeFolder === pin.id;
        return scope.kind === "smart" && scope.id === pin.id;
    }

    /**
     * Is the current scope *inside* this pin, without being it?
     *
     * Folders only — a smart folder has no descendants, so for those the
     * question collapses into plain equality.
     */
    function containsActive(pin: PinnedItem): boolean {
        if (pin.kind !== "folder" || !activeFolder || activeFolder === pin.id) return false;
        let cursor = parentOf.get(activeFolder) ?? null;
        while (cursor) {
            if (cursor === pin.id) return true;
            cursor = parentOf.get(cursor) ?? null;
        }
        return false;
    }

    // ── Context menu ────────────────────────────────────────────────────────
    let menu = $state<{ pin: PinnedItem; x: number; y: number } | null>(null);

    // ── Hover flyout (rail only) ────────────────────────────────────────────
    //
    // A pin in the rail is a 36px icon: enough to recognise, not enough to
    // navigate. Hovering opens the pin's contents beside it — the same "hover
    // looks, click stays" rule the section icons follow.
    let flyout = $state<{ pin: PinnedItem; top: number } | null>(null);
    /** A smart pin's current matches. Cached per pin so re-hovering is free. */
    let preview = $state<AssetLightRow[]>([]);
    const previewCache = new Map<string, AssetLightRow[]>();

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

    /** The rules behind a smart pin, for its chips and its preview query. */
    const smartRules = (id: string) => assetLibrary.smartFolders.find((f) => f.id === id)?.rules;

    async function loadPreview(pin: PinnedItem) {
        const key = refKey(pin);
        const cached = previewCache.get(key);
        if (cached) {
            preview = cached;
            return;
        }
        const rules = smartRules(pin.id);
        if (!rules) return;
        try {
            const rows = await assetLibrary.previewMatches($state.snapshot(rules), 9);
            previewCache.set(key, rows);
            // Only paint if this pin is still the one hovered — a slow query
            // must not drop its rows into a panel that has since moved on.
            if (flyout && refKey(flyout.pin) === key) preview = rows;
        } catch {
            /* the panel still shows the rules, which is the durable half */
        }
    }

    function openFlyout(pin: PinnedItem, anchor: HTMLElement) {
        // Not while rearranging: a flyout appearing under a dragged pin would
        // cover the very list the drop is aiming at.
        if (variant !== "rail" || dragId) return;
        cancelClose();
        const rect = anchor.getBoundingClientRect();
        flyout = {
            pin,
            top: Math.max(8, Math.min(rect.top - 4, window.innerHeight - FLYOUT_H - 8)),
        };
        preview = previewCache.get(refKey(pin)) ?? [];
        if (pin.kind === "smart") void loadPreview(pin);
        onFlyoutOpen?.();
    }

    /** Close from outside, when the rail's own flyout is taking over the space. */
    export function closeFlyout() {
        cancelClose();
        flyout = null;
    }

    $effect(() => cancelClose);

    // Anything that changes membership invalidates the previews. Cheap to drop
    // wholesale — they're nine rows each and refetch on the next hover.
    $effect(() => {
        void assetLibrary.smartFolders;
        void assetLibrary.thumbVersion;
        previewCache.clear();
    });

    // ── Drag to reorder ─────────────────────────────────────────────────────
    //
    // Deliberately NOT the app-wide drag system. That one answers "what does
    // dropping these assets on that folder mean"; this is a list rearranging
    // itself, with no payload and no cross-surface target. Keeping them separate
    // means dragging a pin can't be mistaken for filing a folder into another.
    let listEl = $state<HTMLElement | null>(null);
    let pressKey: string | null = null;
    let pressY = 0;
    let dragId = $state<string | null>(null);
    let dragY = $state(0);
    /** Midpoints of the pins that are staying put, measured once per drag. */
    let midpoints = $state<{ key: string; mid: number }[]>([]);
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
        const dragIdx = pins.findIndex((p) => refKey(p) === dragId);
        return insertAt === (i > dragIdx ? i - 1 : i);
    }

    function startReorder(e: PointerEvent, key: string) {
        if (e.button !== 0) return;
        // Cleared here rather than in the click handler: a drag that ends
        // outside the list fires no click, and a stale flag would then eat the
        // user's next real one.
        justDragged = false;
        pressKey = key;
        pressY = e.clientY;
        (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    }

    function moveReorder(e: PointerEvent) {
        if (!pressKey) return;
        // A 4px threshold, so a click that wobbles is still a click. Without it
        // every navigation would be a no-op reorder.
        if (!dragId && Math.abs(e.clientY - pressY) < 4) return;
        if (!dragId) {
            dragId = pressKey;
            midpoints = [...(listEl?.querySelectorAll<HTMLElement>("[data-pin-key]") ?? [])]
                .filter((el) => el.dataset.pinKey !== dragId)
                .map((el) => {
                    const r = el.getBoundingClientRect();
                    return { key: el.dataset.pinKey!, mid: r.top + r.height / 2 };
                });
        }
        dragY = e.clientY;
    }

    async function endReorder() {
        const movedKey = dragId;
        const at = insertAt;
        pressKey = null;
        dragId = null;
        if (!movedKey || at < 0) return;
        justDragged = true;

        const moved = pins.find((p) => refKey(p) === movedKey);
        if (!moved) return;
        // `after` is the pin it lands behind; null means it becomes the first.
        const afterKey = at > 0 ? midpoints[at - 1].key : null;
        const after = afterKey ? (pins.find((p) => refKey(p) === afterKey) ?? null) : null;
        await assetLibrary.reorderPin(
            moved.kind,
            moved.id,
            after ? { kind: after.kind, id: after.id } : null,
        );
    }

    function open(pin: PinnedItem) {
        // A reorder's pointer-up fires a click too, which would otherwise
        // navigate to whichever pin happened to end up under the cursor.
        if (justDragged) {
            justDragged = false;
            return;
        }
        void assetLibrary.setScope(
            pin.kind === "folder" ? { kind: "folder", id: pin.id } : { kind: "smart", id: pin.id },
        );
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
        {#each pins as pin, i (refKey(pin))}
            {@const key = refKey(pin)}
            {@const active = isActive(pin)}
            {@const inside = containsActive(pin)}
            {@const isSmart = pin.kind === "smart"}
            {@const dropping =
                !isSmart && (dropzone.isOverFolder(pin.id) || drag.isOverFolder(pin.id))}
            {@const refusing =
                isSmart && drag.target?.kind === "smart" && drag.target.id === pin.id}
            {@const accent = pinColorVar(pin.color)}

            <!-- Insertion line for the reorder in progress. -->
            {#if lineBefore(i)}
                <div class="h-0.5 shrink-0 rounded bg-blue-500"></div>
            {/if}

            <div
                data-pin-key={key}
                {...isSmart
                    ? { [DROP_SMART_ATTR]: pin.id, [DROP_SMART_NAME_ATTR]: pin.name }
                    : { [DROP_FOLDER_ATTR]: pin.id, [DROP_FOLDER_NAME_ATTR]: pin.name }}
                class="shrink-0 {dragId === key ? 'opacity-40' : ''}"
            >
                <button
                    type="button"
                    title={pin.name}
                    aria-current={active ? "true" : undefined}
                    onpointerdown={(e) => startReorder(e, key)}
                    onpointermove={moveReorder}
                    onpointerup={endReorder}
                    onpointercancel={endReorder}
                    onpointerenter={(e) => openFlyout(pin, e.currentTarget)}
                    onfocus={(e) => openFlyout(pin, e.currentTarget)}
                    onclick={() => open(pin)}
                    oncontextmenu={(e) => {
                        e.preventDefault();
                        menu = { pin, x: e.clientX, y: e.clientY };
                    }}
                    class="flex items-center transition-colors
                           {variant === 'rail'
                        ? 'h-9 w-9 justify-center rounded-md'
                        : 'w-full gap-2 rounded px-2 py-1 text-left text-sm'}
                           {refusing
                        ? 'cursor-not-allowed bg-red-900/30 ring-1 ring-red-700/60'
                        : dropping
                          ? 'bg-emerald-600/25 ring-1 ring-emerald-500'
                          : active
                            ? 'bg-neutral-800'
                            : 'hover:bg-neutral-800/60'}"
                >
                    <!-- The accent lives on the icon, not the row: a full-width
                         tinted row would fight the selection highlight, and the
                         glyph is what you actually scan for in the rail.

                         The glyph itself distinguishes the two kinds — a smart
                         pin can't be dropped into, so it must not look like
                         something that can. -->
                    <span class="relative flex shrink-0 items-center" style="color: {accent}">
                        {#if isSmart}
                            <Sparkles class="h-4 w-4" strokeWidth={1.5} />
                        {:else}
                            <FolderIcon class="h-4 w-4" strokeWidth={1.5} />
                        {/if}
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
                        <span class="truncate {active ? 'text-neutral-100' : 'text-neutral-300'}">
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
        Right-click a folder or smart folder to pin it here.
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
                    style="background-color: {pinColorVar(flyout.pin.color)}"
                ></span>
                <span
                    class="truncate text-[11px] font-semibold uppercase tracking-wider
                           text-neutral-400"
                >
                    {flyout.pin.name}
                </span>
            </span>
            <button
                type="button"
                onclick={() => {
                    const target = flyout!.pin;
                    flyout = null;
                    open(target);
                }}
                class="shrink-0 rounded px-1.5 py-0.5 text-[11px] text-neutral-500
                       transition-colors hover:bg-neutral-800 hover:text-neutral-200"
            >
                Open
            </button>
        </div>

        <div
            class="flex-1 overflow-y-auto p-2 [scrollbar-width:thin]"
            {...{ [DRAG_SCROLL_ATTR]: "" }}
        >
            {#if flyout.pin.kind === "folder"}
                <!-- The pin's own subtree, fully interactive: same tree
                     component, just rooted here instead of at the library. -->
                <FolderTree rootId={flyout.pin.id} />
            {:else}
                <!-- A smart folder has no subtree. Its rules say WHY things are
                     in here; the thumbnails say WHAT is, right now. Neither
                     answers the other's question, so it shows both. -->
                {@const rules = smartRules(flyout.pin.id) ?? null}
                <div class="flex flex-wrap gap-1">
                    {#each describeConditions(rules) as chip (chip)}
                        <span
                            class="rounded bg-neutral-800 px-1.5 py-0.5 text-[10px] text-neutral-400"
                        >
                            {chip}
                        </span>
                    {:else}
                        <span class="text-[10px] text-neutral-600">Matches everything.</span>
                    {/each}
                </div>

                {#if preview.length > 0}
                    <div class="mt-2 grid grid-cols-3 gap-1">
                        {#each preview as row (row.id)}
                            {@const src = thumbHashUrl(row.thumb_hash)}
                            <div
                                class="aspect-square overflow-hidden rounded bg-neutral-800"
                                title={row.filename}
                            >
                                {#if src}
                                    <!-- ThumbHash, not the real thumbnail: it's
                                         already in the light row, so the preview
                                         costs no file I/O on hover. -->
                                    <img
                                        src={src}
                                        alt=""
                                        class="h-full w-full object-cover"
                                        draggable="false"
                                    />
                                {/if}
                            </div>
                        {/each}
                    </div>
                {:else}
                    <p class="mt-2 text-[10px] text-neutral-600">Nothing matches right now.</p>
                {/if}
            {/if}
        </div>
    </div>
{/if}

{#if menu}
    <PinContextMenu pin={menu.pin} x={menu.x} y={menu.y} onclose={() => (menu = null)} />
{/if}
