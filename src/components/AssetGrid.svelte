<script lang="ts">
    import { untrack } from "svelte";
    import { createVirtualizer } from "@tanstack/svelte-virtual";
    import AssetCard from "./AssetCard.svelte";
    import FilterBar from "./FilterBar.svelte";
    import { layout } from "$lib/layout.svelte";
    import {get} from "svelte/store";
    import { libraryManager, settings } from "../routes/settings.svelte";
    import { assetLibrary, type AssetLightRow } from "$lib/assets.svelte";
    import { selection } from "$lib/selection.svelte";
    import { dropzone } from "$lib/dropzone.svelte";
    import { DROP_LIBRARY_ATTR, type DropTarget } from "$lib/droptarget";
    import { drag, draggable, DRAG_SCROLL_ATTR, type DropContext, type DragPayload } from "$lib/dragdrop.svelte";
    import { viewer } from "$lib/viewer.svelte";
    import ViewerOverlay from "./ViewerOverlay.svelte";
    import { computeJustified, visibleRows } from "$lib/justified";
    import { invoke } from "@tauri-apps/api/core";
    import { startDrag } from "@crabnebula/tauri-plugin-drag";
    import { toast } from "svelte-sonner";
    import AssetContextMenu from "./AssetContextMenu.svelte";
    import RenameDialog from "./actions/RenameDialog.svelte";
    import { undoRun } from "./actions/run";


    // Manifest = layout source of truth (id, width, height, asset_type).
    // `displayed`, not `manifest`: the instant name-filter (search hybrid)
    // narrows in the frontend. Everything here — count, selection, layout —
    // reads this one filtered view.
    const assets = $derived(assetLibrary.displayed);

    // User-controlled column count, persisted in preferences. Local mirror so the
    // slider drags smoothly; we persist on release (onchange). A $effect re-syncs
    // it if settings hydrate from disk after this component mounts.
    let numColumns = $state(settings.preferences.gridColumns);
    $effect(() => {
        numColumns = settings.preferences.gridColumns;
    });

    // Scroll container - passed to the virtualizer's getScrollElement
    let scrollEl = $state<HTMLDivElement | null>(null);

    // Measured container width, updated by ResizeObserver
    // Starts at 0 to supress layout until the first measurement.
    let containerWidth = $state(0);

    const GAP = 10; // px, gap between items in the grid


    $effect(() => {
      if (!libraryManager.state.activeLibrary) return;
      // The active library is the ONLY dependency here. `untrack` is load-bearing:
      // these calls both read and write assetLibrary state (setScope reads the
      // current sort, then load() writes it), so tracking them would invalidate
      // this effect from inside its own body and re-run it forever — which showed
      // up as the grid flickering between "Loading assets..." and "No assets yet"
      // while every manifest load cancelled the one before it.
      untrack(() => {
        // New library: drop every cached row (asset ids are library-scoped, so
        // carrying them over would mix two libraries), reset to the full view, and
        // refresh the folder tree — a stale folder scope would show nothing.
        // Thumbnails are generated on-view (below) as items scroll into the
        // window — no eager pass over the whole library.
        assetLibrary.clearCaches();
        assetLibrary.setScope({ kind: "all" });
        assetLibrary.loadFolders();
        assetLibrary.loadSavedFilters();
        assetLibrary.loadSmartFolders();
        assetLibrary.loadSmartFolderGroups();
        assetLibrary.loadPins();
        assetLibrary.loadQuickActions();
        assetLibrary.loadActionRuns();
        assetLibrary.loadTrashCount();
        assetLibrary.loadColorCoverage();
        assetLibrary.loadTags();
        assetLibrary.loadTagGroups();
      });
    })

    // Height too, for justified row virtualization (waterfall gets it from
    // TanStack; the justified path measures the window itself).
    /**
     * Right-click menu state, carrying the selection AS IT WAS when the menu
     * opened. Right-clicking an unselected card selects it first (that's
     * `pointerDownAsset`, which fires for any button), and right-clicking inside
     * a multi-selection leaves it intact — so by the time this is set, the
     * selection is already what the user means.
     */
    let assetMenu = $state<{ x: number; y: number; ids: string[] } | null>(null);
    let renaming = $state<string[] | null>(null);

    const inTrash = $derived(assetLibrary.scope.kind === "trash");

    /**
     * Move to the Trash, or back out.
     *
     * Reversible either way, so no confirmation — the toast carries the Undo,
     * and Ctrl+Z carries it after that. Confirming a reversible bulk action is
     * how users learn to dismiss dialogs unread.
     */
    async function setTrashed(assetIds: string[], trashed: boolean) {
        try {
            const summary = await assetLibrary.setAssetsTrashed(assetIds, trashed);
            const runId = summary.run_id;
            const what = `${trashed ? "Moved" : "Restored"} ${summary.asset_count.toLocaleString()} ${
                summary.asset_count === 1 ? "asset" : "assets"
            }`;
            if (runId && summary.is_undoable) {
                toast.success(what, {
                    action: { label: "Undo", onClick: () => void undoRun(runId) },
                });
            } else {
                toast.success(what);
            }
        } catch (e) {
            toast.error(typeof e === "string" ? e : "Couldn't move those assets.");
        }
    }

    /** The one irreversible action in the app, so the one that always confirms. */
    async function purge(assetIds: string[]) {
        const ok = window.confirm(
            `Permanently delete ${assetIds.length.toLocaleString()} ${
                assetIds.length === 1 ? "asset" : "assets"
            }? The files are removed from disk. This can't be undone.`,
        );
        if (!ok) return;
        try {
            const purged = await assetLibrary.purgeAssets(assetIds);
            toast.success(`Deleted ${purged.toLocaleString()} permanently`);
        } catch (e) {
            toast.error(typeof e === "string" ? e : "Couldn't delete those assets.");
        }
    }

    let containerHeight = $state(0);
    let scrollTop = $state(0);

    $effect(() => {
        if (!scrollEl) return;
        const observer = new ResizeObserver(([entry]) => {
            containerWidth = entry.contentRect.width;
            containerHeight = entry.contentRect.height;
        });
        observer.observe(scrollEl);
        return () => observer.disconnect();
    });

    // Track scroll position for the justified layout's visible-row window. rAF-
    // coalesced so a fast scroll updates once per frame, not per event. The
    // waterfall path ignores this — TanStack reads scroll on its own.
    $effect(() => {
        const el = scrollEl;
        if (!el) return;
        let ticking = false;
        const onScroll = () => {
            if (ticking) return;
            ticking = true;
            requestAnimationFrame(() => {
                scrollTop = el.scrollTop;
                ticking = false;
            });
        };
        el.addEventListener("scroll", onScroll, { passive: true });
        scrollTop = el.scrollTop;
        return () => el.removeEventListener("scroll", onScroll);
    });

    const columnWidth = $derived(
        containerWidth > 0 ? (containerWidth - GAP * (numColumns - 1)) / numColumns : 200, // safe fallback before first measuremnt
    );

    const manualSort = $derived(assetLibrary.sort.order_by === "manual");

    // The layout actually rendered. Waterfall is the staggered masonry; justified
    // fills full-width rows in reading order. Manual sort FORCES justified,
    // regardless of the saved preference, because only reading order supports
    // exact reorder — leaving manual restores the user's choice.
    const effectiveView = $derived(
        manualSort ? "justified" : settings.preferences.gridView,
    );
    const isJustified = $derived(effectiveView === "justified");

    // ── Waterfall layout (TanStack lanes) ─────────────────────────────────────
    // Seed values only — kept live by the setOptions $effect below. `untrack`
    // makes "I want the current value, not a reactive dependency" explicit (and
    // silences state_referenced_locally, which is right: these must NOT re-create
    // the virtualizer).
    const virtualizer = createVirtualizer<HTMLDivElement, HTMLDivElement>({
      count: untrack(() => assets.length),
      getScrollElement: () => scrollEl,
      lanes: untrack(() => numColumns),
      estimateSize: (index) => {
        const a = assets[index];
        const ratio = a?.width && a?.height ? a.width / a.height : 1;
        return columnWidth / ratio + GAP;
      },
      overscan: 6,
      getItemKey: (index) => assets[index]?.id ?? index,
    })

    $effect(() => {
      const count = assets.length;
      const lanes = numColumns;
      void columnWidth;
      void scrollEl;
      const instance = get(virtualizer);
      instance.setOptions({ count, lanes});
      instance.measure();
    })

    // ── Justified layout ──────────────────────────────────────────────────────
    // The columns slider still controls thumbnail SIZE here: a "column width"
    // becomes the target row height, so more columns → smaller thumbnails →
    // more per row, matching the waterfall's feel.
    const justified = $derived.by(() => {
        if (!isJustified) return { rows: [], totalHeight: 0 };
        const ratios = assets.map((a) => (a.width && a.height ? a.width / a.height : 1));
        return computeJustified(ratios, containerWidth, columnWidth, GAP);
    });

    // Only the rows in (or near) the viewport get rendered — row-level
    // virtualization, the justified counterpart of TanStack's item windowing.
    const justifiedVisible = $derived(
        isJustified ? visibleRows(justified, scrollTop, containerHeight, columnWidth * 2) : [],
    );

    // Hydrate heavy rows + generate thumbnails for the visible window (+overscan).
    //
    // CRITICAL: read the virtualizer via get(virtualizer), NOT $virtualizer.
    // Reading $virtualizer reactively inside an effect self-invalidates (calling
    // getVirtualItems() re-notifies the store, re-running the effect faster than
    // the debounce, so the timer is cleared forever and this never runs). Instead
    // we trigger off the scroll DOM event and off manifest changes.
    // The indices on screen, from whichever layout is active. Both paths window
    // to (visible + overscan), so hydration and thumbnail generation cover the
    // same set regardless of view.
    function visibleIndices(): number[] {
        if (isJustified) {
            return justifiedVisible.flatMap((row) => row.items.map((it) => it.index));
        }
        return get(virtualizer)
            .getVirtualItems()
            .map((item) => item.index);
    }

    function runHydrate() {
        const rows = visibleIndices()
            .map((index) => assets[index])
            .filter((r): r is AssetLightRow => !!r);
        if (!rows.length) return;
        assetLibrary.ensure(rows.map((r) => r.id)); // hydrate heavy rows for the window
        // Only images still missing a thumbnail need generation.
        const needIds = rows
            .filter((r) => r.asset_type === "image" && r.thumb_hash === null)
            .map((r) => r.id);
        if (needIds.length) {
            assetLibrary.ensureThumbnails(
                needIds,
                settings.preferences.thumbnailQuality,
                settings.preferences.thumbnailLossyQuality,
            );
        }
    }

    let hydrateTimer: ReturnType<typeof setTimeout>;
    function scheduleHydrate(delay = 100) {
        clearTimeout(hydrateTimer);
        hydrateTimer = setTimeout(runHydrate, delay);
    }

    // Scroll-driven: a plain DOM listener, no reactive virtualizer read. We fire on
    // `scrollend` (fires the instant momentum settles) so a window fills with zero
    // wait when you stop — a debounce could only *guess* at the stop and always add
    // that lag. Nothing generates mid-scroll (placeholders only, no wasted decode).
    // Fall back to a short debounce on webviews without `scrollend`.
    $effect(() => {
        const el = scrollEl;
        if (!el) return;
        const hasScrollEnd = "onscrollend" in window;
        const onScrollEnd = () => runHydrate();
        const onScroll = () => scheduleHydrate(100);
        if (hasScrollEnd) {
            el.addEventListener("scrollend", onScrollEnd, { passive: true });
        } else {
            el.addEventListener("scroll", onScroll, { passive: true });
        }
        return () => {
            el.removeEventListener("scrollend", onScrollEnd);
            el.removeEventListener("scroll", onScroll);
        };
    });

    // Manifest/layout-driven: hydrate the initial window after load/import and
    // when the column count or view changes (either shifts what's visible).
    $effect(() => {
        void assets.length;
        void numColumns;
        void effectiveView;
        scheduleHydrate();
    });

    // Justified path has no `scrollend`-style hook of its own — its visible set
    // is a derived, so scrolling changes it reactively. Hydrate whenever that
    // set shifts. (Waterfall ignores this; it hydrates off the scroll listener.)
    $effect(() => {
        if (!isJustified) return;
        void justifiedVisible;
        scheduleHydrate(80);
    });


    // ── Selection ────────────────────────────────────────────────────────────
    // The store owns the RULES; this component owns the ORDER. Ids are read
    // lazily per interaction rather than kept in a $derived, because the manifest
    // streams in chunks — a derived id list would be rebuilt on every chunk, and
    // that's O(n²) over a large library. Clicks are rare; one map() is nothing.
    const idsNow = () => assets.map((a) => a.id);
    const mods = (e: MouseEvent | PointerEvent) => ({
        ctrl: e.ctrlKey || e.metaKey, // metaKey = Cmd on macOS
        shift: e.shiftKey,
    });

    // Ctrl+A / Escape / Space / F. Window-level so they work wherever focus sits.
    $effect(() => {
        const onKey = (e: KeyboardEvent) => {
            // Never steal keys from a text field — the search/filter bars sit
            // right above this grid and Space there must type a space.
            const t = e.target as HTMLElement | null;
            if (t && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName))) return;

            // The viewer owns the keyboard while it's open (nav, zoom, close);
            // ViewerOverlay handles those. Don't double-handle here.
            if (viewer.isOpen) return;

            if (e.key === " ") {
                // Space opens QuickLook on the current selection.
                e.preventDefault();
                viewer.toggleQuickLook();
            } else if (e.key.toLowerCase() === "f" && !e.ctrlKey && !e.metaKey && !e.altKey) {
                e.preventDefault();
                viewer.toggleFullscreen();
            } else if (e.key === "Escape") {
                selection.clear();
            } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "a") {
                e.preventDefault();
                // `assets` is read here, when the handler RUNS, not when this
                // effect does — so it never re-registers the listener, and
                // "select all" always means all currently *visible* rows.
                selection.selectAllAssets(idsNow());
            } else if (e.key.toLowerCase() === "t" && !e.ctrlKey && !e.metaKey && !e.altKey) {
                // Open the tag editor for the current selection. No-op with
                // nothing selected (requestTagFocus guards that).
                e.preventDefault();
                selection.requestTagFocus();
            }
        };
        window.addEventListener("keydown", onKey);
        return () => window.removeEventListener("keydown", onKey);
    });

    // Keep the grid beneath in step with the viewer: scrolling the current asset
    // into view means closing the viewer lands on it, and the virtualizer mounts
    // the row that arrow-nav is pointing at. `scrollToIndex`, NOT scrollIntoView —
    // the target row is usually unmounted, so there's no DOM node to scroll to.
    $effect(() => {
        if (!viewer.isOpen) return;
        const index = viewer.index;
        untrack(() => get(virtualizer).scrollToIndex(index, { align: "auto" }));
    });

    // ── Dragging assets out to the sidebar ────────────────────────────────────
    //
    // One `draggable` on the scroll container, not one per card: rows mount and
    // unmount on every scroll tick under virtualization, so per-card listeners
    // would churn and a card can vanish mid-drag. `payload` decides from the
    // event whether the press landed on a card.

    /** The selection is the payload — a press on a card drags whatever is selected. */
    function dragPayload(e: PointerEvent) {
        if (!(e.target as HTMLElement).closest('[role="option"]')) return null;
        return selection.assetCount > 0
            ? ({ kind: "assets", ids: selection.assetIds } as const)
            : null;
    }

    // ── Manual reorder within the grid ────────────────────────────────────────
    //
    // Reordering writes a rank. Under any sort but manual that rank is invisible
    // — the card would snap back to where the sort puts it — so the gesture is
    // available ONLY in manual mode (see `manualSort`, defined with the layout).
    // Dragging OUT to the sidebar stays live in every mode: it writes
    // membership, not a rank.

    /**
     * Where a drop at (x, y) inserts: the manifest `index` it lands before
     * (`assets.length` = the very end), plus viewport coordinates for the bar.
     *
     * ONE function feeds both the live bar and the actual drop, so the two can
     * never disagree — the item lands exactly where the bar showed.
     *
     * The split is the card under the cursor at its horizontal midpoint. Honest
     * caveat: the grid is a shortest-lane MASONRY, so a card's manifest index
     * and its visual position don't march in lock-step — near a tall image the
     * insert can land a slot off from where it looks. The optimistic reorder
     * shows the true result instantly, which is the best available answer short
     * of a reading-order layout for manual mode.
     */
    function insertionAt(x: number, y: number) {
        const el = document.elementFromPoint(x, y);
        const card = el?.closest<HTMLElement>("[data-asset-id]");
        if (!card) return { index: assets.length, bar: null }; // empty space past the end
        const idx = Number(card.dataset.assetIndex);
        const r = card.getBoundingClientRect();
        const after = x >= r.left + r.width / 2;
        return {
            index: after ? idx + 1 : idx,
            bar: { x: after ? r.right + GAP / 2 : r.left - GAP / 2, top: r.top, height: r.height },
        };
    }

    /** Is (x, y) inside the scroll viewport? Lets a near-miss still reorder. */
    function withinGrid(x: number, y: number): boolean {
        if (!scrollEl) return false;
        const r = scrollEl.getBoundingClientRect();
        return x >= r.left && x <= r.right && y >= r.top && y <= r.bottom;
    }

    /** Live insertion bar while dragging assets over the grid in manual mode. */
    const reorderBar = $derived.by(() => {
        if (drag.payload?.kind !== "assets" || !manualSort) return null;
        // Over a folder row we're filing, not reordering — no bar.
        if (drag.target?.kind === "folder") return null;
        if (!withinGrid(drag.x, drag.y)) return null;
        return insertionAt(drag.x, drag.y).bar; // reactive via drag.x / drag.y
    });

    /** Blocked reorder: dragging assets over the grid while NOT in manual sort. */
    const reorderBlocked = $derived(
        drag.payload?.kind === "assets" && drag.target?.kind === "library" && !manualSort,
    );

    async function reorderTo(index: number, movedIds: string[]) {
        // The block lands after the nearest asset at/above the insert point that
        // ISN'T being moved — Rust drops moved ids from the neighbour set, so an
        // afterId must be a stayer or it falls through to "append".
        const moved = new Set(movedIds);
        let afterId: string | null = null;
        for (let i = index - 1; i >= 0; i--) {
            if (!moved.has(assets[i].id)) {
                afterId = assets[i].id;
                break;
            }
        }
        try {
            await assetLibrary.reorderAssets(movedIds, afterId);
        } catch (e) {
            toast.error(typeof e === "string" ? e : "Failed to reorder.");
        }
    }

    // ── Dragging assets OUT to other apps ─────────────────────────────────────
    //
    // The engine hands off here when the cursor leaves the window mid-drag. We
    // stage the files in Rust (ids in, paths out — never the reverse), then let
    // the native drag plugin take over. `mode: "copy"` plus the hard-link staging
    // means the library is safe even if the receiver "moves" the file.

    /** A small blue badge with the item count, as a PNG data URL for the drag image. */
    function dragIcon(count: number): string {
        const s = 48;
        const c = document.createElement("canvas");
        c.width = s;
        c.height = s;
        const ctx = c.getContext("2d");
        if (!ctx) return "";
        ctx.fillStyle = "#2563eb";
        const r = 10;
        ctx.beginPath();
        ctx.roundRect(0, 0, s, s, r);
        ctx.fill();
        ctx.fillStyle = "#fff";
        ctx.font = "bold 22px system-ui, sans-serif";
        ctx.textAlign = "center";
        ctx.textBaseline = "middle";
        ctx.fillText(String(count), s / 2, s / 2 + 1);
        return c.toDataURL("image/png");
    }

    async function onDragOut(payload: DragPayload) {
        if (payload.kind !== "assets" || payload.ids.length === 0) return;
        try {
            const paths = await invoke<string[]>("start_asset_drag", { assetIds: payload.ids });
            if (!paths.length) return;
            await startDrag(
                { item: paths, icon: dragIcon(payload.ids.length), mode: "copy" },
                // Fires on Dropped OR Cancelled — either way the staged links have
                // done their job and can go.
                () => void invoke("clear_drag_staging").catch(() => {}),
            );
        } catch (e) {
            toast.error(typeof e === "string" ? e : "Couldn't start the drag.");
        }
    }

    async function onAssetDrop(target: DropTarget | null, ctx: DropContext) {
        if (ctx.payload.kind !== "assets") return; // a folder drag isn't ours
        const ids = ctx.payload.ids;
        if (ids.length === 0) return;

        // A drop on the grid — or a near-miss released within its bounds — is a
        // reorder in manual mode, otherwise a silent abandon (the guardrail
        // already said why during the drag). Accepting the null/near-miss case
        // is what fixes "dragged to the bottom but nothing happened": releasing
        // in the empty space past the last card no longer falls through.
        const onGrid = target?.kind === "library" || (target === null && withinGrid(ctx.x, ctx.y));
        if (onGrid) {
            if (manualSort) await reorderTo(insertionAt(ctx.x, ctx.y).index, ids);
            return;
        }
        if (target?.kind !== "folder") return;

        // Move only means something when the drag STARTED somewhere: in "All" or
        // "Uncategorized" there is no source membership to remove, so Shift
        // quietly degrades to an add.
        const source = assetLibrary.scope.kind === "folder" ? assetLibrary.scope.id : null;
        if (source === target.id) return; // dropped on the folder we're already in

        try {
            if (ctx.move) {
                await assetLibrary.moveAssetsToFolder(source, target.id, ids);
                toast.success(
                    source
                        ? `Moved ${ids.length} to "${target.name}".`
                        : `Added ${ids.length} to "${target.name}".`,
                );
            } else {
                await assetLibrary.addAssetsToFolder(target.id, ids);
                toast.success(`Added ${ids.length} to "${target.name}".`);
            }
        } catch (e) {
            toast.error(typeof e === "string" ? e : "Failed to file assets.");
        }
    }
</script>


<!-- The neutral import surface: a drop anywhere here that isn't a folder row
     means "import into the library at large". Marked on the outer element rather
     than the scroll container so the header and empty states accept drops too —
     an empty library is exactly when you most want to drop files in. -->
<div class="relative flex h-full flex-col" {...{ [DROP_LIBRARY_ATTR]: "" }}>
    <!-- LAYOUT: this component has no header any more. The asset count, the
         search field and the view controls moved up into the grid pane's header
         (+page.svelte + GridToolbar.svelte) so all three panes share one 44px
         chrome strip and this is nothing but assets.
         The FilterBar stays, toggled from the toolbar so it costs no space when
         idle. It is FORCE-SHOWN while any filter is active: a narrowed view must
         always show what's narrowing it, or "why is my library half empty" has
         no answer on screen. (Gating it on `hasFilters` alone was circular —
         these are the controls that set the first filter.) -->
    {#if layout.filterBarOpen || assetLibrary.hasFilters}
        <div class="shrink-0">
            <FilterBar />
        </div>
    {/if}

    {#if assetLibrary.isLoading && assets.length === 0}
            <div class="flex items-center justify-center h-32 text-sm text-neutral-500">Loading assets...</div>
        {:else if assetLibrary.error}
            <div class="flex items-center justify-center h-32 text-sm text-red-400">{assetLibrary.error}</div>
        {:else if assets.length === 0 && (assetLibrary.hasFilters || assetLibrary.nameFiltering)}
            <!-- Distinguish "narrowed to nothing" from "nothing imported" — the
                 fix for one is a click, for the other it's an import. The name
                 filter counts here too, so a no-match search doesn't read as an
                 empty library. -->
            <div class="flex flex-col items-center justify-center gap-2 h-32 text-sm text-neutral-500">
                <span>No assets match your search.</span>
                <button
                    type="button"
                    onclick={() => {
                        assetLibrary.clearFilters();
                        assetLibrary.setNameFilter(null);
                    }}
                    class="rounded px-2 py-0.5 text-xs font-medium text-blue-700 hover:bg-blue-100"
                >
                    ✕ Clear
                </button>
            </div>
        {:else if assets.length === 0}
            <div class="flex items-center justify-center h-32 text-sm text-neutral-500">No assets in this library yet.</div>
        {:else}
            <!-- Clicking the gaps between cards deselects, the way it does in
                 every file manager. Cards stop this by being the closest
                 [role=option]; the check runs on the bubbled event. -->
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <div
                bind:this={scrollEl}
                use:draggable={{
                    payload: dragPayload,
                    // The press that starts a drag must not collapse a
                    // multi-selection when it's released.
                    onStart: () => selection.assets.cancelPendingCollapse(),
                    // A smart folder collects by rule, so nothing can be put
                    // INTO one. Refusing here (rather than letting the drop land
                    // and do nothing) is what paints the forbidden state and
                    // lets the preview explain itself.
                    validate: (target) => target.kind !== "smart",
                    onDrop: onAssetDrop,
                    onDragOut: onDragOut,
                }}
                {...{ [DRAG_SCROLL_ATTR]: "" }}
                onpointerdown={(e) => {
                    if (!(e.target as HTMLElement).closest('[role="option"]')) selection.clear();
                }}
                oncontextmenu={(e) => {
                    // Right-clicking empty space is a deselect, not a menu — the
                    // menu acts on a selection and there isn't one.
                    const card = (e.target as HTMLElement).closest('[role="option"]');
                    if (!card) return;
                    e.preventDefault();
                    // The selection is snapshotted HERE, at open time. A menu
                    // action can take seconds to confirm, and by then the grid
                    // may have re-streamed under it.
                    assetMenu = { x: e.clientX, y: e.clientY, ids: selection.assetIds };
                }}
                class="relative flex-1 min-h-0 overflow-y-auto w-full
                       [scrollbar-width:thin] [scrollbar-color:theme(colors.neutral.700)_transparent]"
            >
                <!-- svelte-ignore a11y_no_noninteractive_element_to_interactive_role -->
                <div
                    role="listbox"
                    aria-multiselectable="true"
                    aria-label="Assets"
                    style="position: relative; width: 100%; height: {isJustified
                        ? justified.totalHeight
                        : $virtualizer.getTotalSize()}px;"
                >
                    {#if isJustified}
                        <!-- Justified: absolute row tops, per-item left/width from
                             the layout. Reading order == index order, so the
                             reorder hit-test lands exactly. -->
                        {#each justifiedVisible as row (row.top)}
                            <!-- `?.` in the key, not just the body: a key
                                 expression is evaluated before the block, so a
                                 bare deref here would throw where no guard can
                                 catch it. Same shrink hazard as the waterfall
                                 branch below. -->
                            {#each row.items as it (assets[it.index]?.id ?? it.index)}
                                {@const light = assets[it.index]}
                                {#if light}
                                {@const heavy = assetLibrary.heavy.get(light.id)}
                                <AssetCard
                                    assetType={light.asset_type}
                                    thumbHash={light.thumb_hash}
                                    isAnimated={light.is_animated}
                                    animate={settings.preferences.animateGifsInGrid}
                                    {heavy}
                                    style="width: {it.width}px; height: {row.height}px; left: {it.left}px; transform: translateY({row.top}px);"
                                    selected={selection.has(light.id)}
                                    dataId={light.id}
                                    dataIndex={it.index}
                                    onPointerDown={(e) =>
                                        selection.pointerDownAsset(idsNow(), it.index, mods(e))}
                                    onClick={() => selection.clickAsset(light.id)}
                                    onOpen={() => viewer.open(it.index, "quicklook")}
                                />
                                {/if}
                            {/each}
                        {/each}
                    {:else}
                        {#each $virtualizer.getVirtualItems() as item (item.key)}
                            {@const light = assets[item.index]}
                            <!-- The virtualizer's item list lags `assets` by a
                                 frame: it's still sized for the OLD count until
                                 the setOptions/measure effect runs, so a filter
                                 that SHRINKS the manifest briefly yields indices
                                 past the end. Without this guard the render
                                 throws on `light.thumb_hash`, and a thrown
                                 render leaves the previous grid on screen —
                                 which looks exactly like "the filter did
                                 nothing" rather than like a crash. -->
                            {#if light}
                            {@const heavy = assetLibrary.heavy.get(light.id)}
                            <AssetCard
                                assetType={light.asset_type}
                                thumbHash={light.thumb_hash}
                                isAnimated={light.is_animated}
                                animate={settings.preferences.animateGifsInGrid}
                                {heavy}
                                style="width: {columnWidth}px; height: {item.size - GAP}px; left: {item.lane * (columnWidth + GAP)}px; transform: translateY({item.start}px);"
                                selected={selection.has(light.id)}
                                dataId={light.id}
                                dataIndex={item.index}
                                onPointerDown={(e) =>
                                    selection.pointerDownAsset(idsNow(), item.index, mods(e))}
                                onClick={() => selection.clickAsset(light.id)}
                                onOpen={() => viewer.open(item.index, "quicklook")}
                            />
                            {/if}
                        {/each}
                    {/if}
                </div>
            </div>
        {/if}

    <!-- The viewer. Mounted INSIDE the grid column so QuickLook (absolute) stays
         scoped to the grid with the sidebars visible; Fullscreen (fixed) escapes
         to cover the window. -->
    <ViewerOverlay />

    <!-- Drop affordance. `pointer-events-none` is load-bearing, not cosmetic:
         `elementFromPoint` skips elements that ignore pointer events, so without
         it this overlay would sit between the cursor and the very target the
         dropzone is trying to resolve. -->
    {#if dropzone.active && dropzone.target?.kind === "library"}
        <div
            class="pointer-events-none absolute inset-0 z-20 flex items-center justify-center
                   border-2 border-dashed border-emerald-500 bg-emerald-500/10"
        >
            <span class="rounded bg-emerald-600 px-3 py-1.5 text-sm font-medium text-white">
                Import {dropzone.count}
                {dropzone.count === 1 ? "item" : "items"}
            </span>
        </div>
    {/if}

    <!-- Manual reorder insertion bar. Fixed-position, off the card under the
         cursor, so it needs no virtualizer math. pointer-events-none for the
         same reason as every drag overlay: it must not shadow its own target. -->
    {#if reorderBar}
        <div
            class="pointer-events-none fixed z-30 w-0.5 rounded bg-blue-500"
            style="left: {reorderBar.x}px; top: {reorderBar.top}px; height: {reorderBar.height}px"
        ></div>
    {/if}

    <!-- Guardrail: a reorder attempt under a non-manual sort can't write a
         visible order, so say why rather than silently doing nothing. The button
         switches sort mid-drag; releasing then completes the reorder. -->
    {#if reorderBlocked}
        <div
            class="pointer-events-none absolute inset-x-0 bottom-3 z-30 flex justify-center"
        >
            <div
                class="pointer-events-auto flex items-center gap-2 rounded-full bg-neutral-900/95 px-3
                       py-1.5 text-xs text-neutral-200 shadow-lg ring-1 ring-neutral-700"
            >
                <span>Sorted by {assetLibrary.sort.order_by.replace("_", " ")} — reordering needs Manual sort.</span>
                <button
                    type="button"
                    onclick={() => assetLibrary.setSort({ order_by: "manual", is_ascending: true })}
                    class="rounded-full bg-blue-600 px-2 py-0.5 font-medium text-white hover:bg-blue-500"
                >
                    Switch
                </button>
            </div>
        </div>
    {/if}
    </div>

{#if assetMenu}
    {@const ids = assetMenu.ids}
    <AssetContextMenu
        count={ids.length}
        {inTrash}
        x={assetMenu.x}
        y={assetMenu.y}
        onclose={() => (assetMenu = null)}
        onRename={() => (renaming = ids)}
        onTrash={() => void setTrashed(ids, true)}
        onRestore={() => void setTrashed(ids, false)}
        onPurge={() => void purge(ids)}
    />
{/if}

{#if renaming}
    <RenameDialog assetIds={renaming} onclose={() => (renaming = null)} />
{/if}
