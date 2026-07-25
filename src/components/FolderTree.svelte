<script lang="ts">
    import { assetLibrary, type Folder } from "$lib/assets.svelte";
    import { RangeSelection, selection } from "$lib/selection.svelte";
    import { dropzone } from "$lib/dropzone.svelte";
    import {
        drag,
        draggable,
        type DragPayload,
        type DropContext,
    } from "$lib/dragdrop.svelte";
    import { DROP_FOLDER_ATTR, DROP_FOLDER_NAME_ATTR, type DropTarget } from "$lib/droptarget";
    import { pinColorVar } from "$lib/pins";
    import FolderContextMenu from "./FolderContextMenu.svelte";
    import { toast } from "svelte-sonner";

    /**
     * Where the tree starts.
     *
     * `null` is the library root — the sidebar's full tree. Passing a folder id
     * renders that folder's subtree instead, which is what a pinned folder's
     * hover flyout shows. Same component either way: the drag, drop, rename and
     * context-menu machinery is identical, only the starting node differs.
     */
    interface Props {
        rootId?: string | null;
    }

    const { rootId = null }: Props = $props();

    /** Right-click target, with the viewport point the menu opens at. */
    let menu = $state<{ folder: Folder; x: number; y: number } | null>(null);

    const folders = $derived(assetLibrary.folders);
    const active = $derived(assetLibrary.scope);

    /**
     * Name filter for the tree.
     *
     * A plain substring match over folder names, NOT the FTS5 path: folder names
     * are already in memory and the list is small, while `search` is an asset
     * index whose text lives in `FilterSet.text`. Routing this through it would
     * fight the asset search for the same state.
     */
    let filterText = $state("");
    const filtering = $derived(filterText.trim().length > 0);

    /**
     * Ids to render while filtering: every match, plus each match's ancestors —
     * without the ancestors a deep hit would have nothing to hang from and the
     * tree would collapse into a flat list that lies about the structure.
     */
    const visibleIds = $derived.by(() => {
        if (!filtering) return null;
        const needle = filterText.trim().toLowerCase();
        const byId = new Map(folders.map((f) => [f.id, f]));
        const keep = new Set<string>();
        for (const f of folders) {
            if (!f.name.toLowerCase().includes(needle)) continue;
            keep.add(f.id);
            let cursor = f.parent_id;
            while (cursor && !keep.has(cursor)) {
                keep.add(cursor);
                cursor = byId.get(cursor)?.parent_id ?? null;
            }
        }
        return keep;
    });

    // parent_id -> children[], preserving the backend's position order.
    const childrenByParent = $derived.by(() => {
        const map = new Map<string | null, Folder[]>();
        for (const f of folders) {
            if (visibleIds && !visibleIds.has(f.id)) continue;
            const arr = map.get(f.parent_id) ?? [];
            arr.push(f);
            map.set(f.parent_id, arr);
        }
        return map;
    });

    /**
     * The tree flattened in display order. Shift-ranges are computed over THIS,
     * not the DOM — it's what the user sees top to bottom, so a range across a
     * collapsed boundary still means what it looks like it means.
     */
    const flat = $derived.by(() => {
        const out: Folder[] = [];
        const walk = (parent: string | null) => {
            for (const f of childrenByParent.get(parent) ?? []) {
                out.push(f);
                walk(f.id);
            }
        };
        // Scoped to the rendered subtree, so a shift-range in a pin's flyout
        // can't select rows that aren't on screen.
        walk(rootId);
        return out;
    });

    /**
     * Tree-LOCAL multi-selection, for group delete only. Deliberately its own
     * `RangeSelection` rather than part of the app-wide `selection`: selecting
     * folders here is picking an operand, not choosing something to inspect. The
     * inspector still holds at most ONE folder — see Selection in
     * selection.svelte.ts for why multi-folder inspection has no coherent answer.
     */
    const treeSel = new RangeSelection();

    /** id -> position in `flat`. Built once per tree change; the alternative was
        a findIndex inside the render loop, which is O(n²) over the whole tree. */
    const indexById = $derived(new Map(flat.map((f, i) => [f.id, i])));

    const orderedIds = () => flat.map((f) => f.id);
    const mods = (e: MouseEvent | PointerEvent) => ({
        ctrl: e.ctrlKey || e.metaKey, // metaKey = Cmd on macOS
        shift: e.shiftKey,
    });

    /**
     * Keep the inspector in step with the tree: it shows a folder exactly when
     * one is selected here, and nothing when zero or many are. That's the same
     * rule "All"/"Uncategorized" follow — if there isn't a single thing to edit,
     * the panel says so rather than guessing.
     */
    function syncInspector() {
        const ids = treeSel.ids;
        if (ids.length === 1) selection.selectFolder(ids[0]);
        else selection.clear();
    }

    function pressFolder(e: PointerEvent, index: number) {
        // Left button only. A right-click is aiming at the context menu, and
        // must not collapse a multi-selection the user built to delete.
        if (e.pointerType === "mouse" && e.button !== 0) return;
        treeSel.pointerDown(orderedIds(), index, mods(e));
        syncInspector();
    }

    /**
     * Plain click navigates; Ctrl/Shift only ever adjusts the selection. Holding
     * a modifier means "I'm building a set to act on", and jumping the grid to a
     * new folder mid-way through that would be actively hostile.
     */
    function clickFolder(e: MouseEvent, folder: Folder) {
        treeSel.click(folder.id);
        syncInspector();
        if (!e.ctrlKey && !e.metaKey && !e.shiftKey) {
            assetLibrary.setScope({ kind: "folder", id: folder.id });
        }
    }

    const isScope = (id: string) => active.kind === "folder" && active.id === id;

    // ── Reorganising the tree by dragging ────────────────────────────────────
    //
    // Drop on the MIDDLE of a row to reparent; on its top or bottom EDGE to
    // insert between siblings. Two different operations, so they get two
    // different indicators — a row highlight and an insertion line. Ambiguity
    // here is the single most common complaint about tree drag & drop.

    /** id -> its parent chain, so "is this a descendant of that" is a lookup. */
    const parentOf = $derived(new Map(folders.map((f) => [f.id, f.parent_id])));

    /** Is `candidate` inside `rootId`'s subtree (or the folder itself)? */
    function isWithin(candidate: string, rootId: string): boolean {
        let cursor: string | null | undefined = candidate;
        while (cursor) {
            if (cursor === rootId) return true;
            cursor = parentOf.get(cursor);
        }
        return false;
    }

    /**
     * Where a drop on `target` would put the dragged folder.
     *
     * The edge zones resolve to "sibling of the row", which is why `before` and
     * `after` both read the row's OWN parent rather than the row itself.
     */
    function placement(target: DropTarget): { parent: string | null; after: string | null } | null {
        if (target.kind !== "folder") return null;
        const row = folders.find((f) => f.id === target.id);
        if (!row) return null;

        if (target.zone === "into") {
            // Appending is what "drop onto a folder" means; the precise slot is
            // the user's next drag, not this one.
            return { parent: target.id, after: null };
        }

        const siblings = childrenByParent.get(row.parent_id) ?? [];
        const at = siblings.findIndex((f) => f.id === row.id);
        return {
            parent: row.parent_id,
            // "before this row" means after the row above it — or first, if
            // there is nothing above.
            after: target.zone === "after" ? row.id : (siblings[at - 1]?.id ?? null),
        };
    }

    function folderPayload(e: PointerEvent): DragPayload | null {
        const row = (e.target as HTMLElement).closest(`[${DROP_FOLDER_ATTR}]`);
        // Row actions (＋ ✎ 🗑) sit inside the row: dragging from a button would
        // be a gesture nobody intends.
        if (!row || (e.target as HTMLElement).closest("button[data-row-action]")) return null;
        const id = row.getAttribute(DROP_FOLDER_ATTR);
        const folder = folders.find((f) => f.id === id);
        return folder ? { kind: "folder", id: folder.id, name: folder.name } : null;
    }

    /**
     * A folder cannot land inside its own subtree — that detaches it from the
     * root. Rust refuses this too; checking here is what lets the cursor say so
     * before the user commits rather than after.
     */
    function validateFolderDrop(target: DropTarget, payload: DragPayload): boolean {
        if (payload.kind !== "folder") return true;
        if (target.kind !== "folder") return false;
        const place = placement(target);
        if (!place) return false;
        if (place.parent !== null && isWithin(place.parent, payload.id)) return false;
        // Dropping onto itself in "into" mode changes nothing.
        return !(target.zone === "into" && target.id === payload.id);
    }

    async function onFolderDrop(target: DropTarget | null, ctx: DropContext) {
        if (ctx.payload.kind !== "folder" || !target) return;
        const place = placement(target);
        if (!place) return;
        try {
            await assetLibrary.reorderFolder(ctx.payload.id, place.parent, place.after);
        } catch (e) {
            toast.error(typeof e === "string" ? e : "Failed to move folder.");
        }
    }

    /** Which edge of this row should show an insertion line, if any. */
    function insertLine(id: string): "before" | "after" | null {
        const t = drag.target;
        if (drag.payload?.kind !== "folder" || drag.forbidden) return null;
        if (t?.kind !== "folder" || t.id !== id || t.zone === "into") return null;
        return t.zone;
    }

    async function newFolder(parentId: string | null) {
        const name = window.prompt("Folder name:");
        if (!name?.trim()) return;
        try {
            await assetLibrary.createFolder(name.trim(), parentId);
        } catch (e) {
            toast.error(typeof e === "string" ? e : "Failed to create folder.");
        }
    }

    async function renameFolder(folder: Folder) {
        const name = window.prompt("Rename folder:", folder.name);
        if (!name?.trim() || name.trim() === folder.name) return;
        try {
            await assetLibrary.renameFolder(folder.id, name.trim());
        } catch (e) {
            toast.error(typeof e === "string" ? e : "Failed to rename folder.");
        }
    }

    /**
     * Delete the whole selection when the clicked folder is part of it, otherwise
     * just the one clicked. Same rule as every file manager: the row action
     * applies to the selection you can see, never silently to more or less.
     */
    async function deleteFolders(folder: Folder) {
        const ids = treeSel.has(folder.id) ? treeSel.ids : [folder.id];
        const what =
            ids.length === 1
                ? `"${folder.name}"`
                : `${ids.length} folders`;
        if (!window.confirm(`Delete ${what} and their subfolders? Your assets stay in the library.`))
            return;
        try {
            await assetLibrary.deleteFolders(ids);
            treeSel.clear();
            selection.clear();
        } catch (e) {
            toast.error(typeof e === "string" ? e : "Failed to delete folders.");
        }
    }
</script>

<!-- One drag source for the whole tree, matching the grid: rows are plain DOM
     here, but keeping both surfaces on the same pattern means one place to fix
     when a gesture misbehaves. `folderPayload` decides from the event which row
     (if any) the press landed on. -->
<div
    use:draggable={{
        payload: folderPayload,
        validate: validateFolderDrop,
        onDrop: onFolderDrop,
    }}
    class="flex flex-col gap-0.5 rounded-lg border border-neutral-800 bg-neutral-900/40 p-2 text-sm"
>
    <!-- "All assets" and "Uncategorized" used to sit here. They're smart views,
         not folders, so they live in SystemViews.svelte now — the sidebar draws
         the line between what the library computes and what the user built. -->
    {#if !rootId && folders.length > 4}
        <!-- The filter earns its row only once the tree is big enough to need
             it; on a five-folder library, or in a single folder's subtree, it
             would be pure chrome. -->
        <input
            type="text"
            bind:value={filterText}
            placeholder="Filter folders…"
            class="mb-1 w-full rounded border border-neutral-800 bg-neutral-950 px-2 py-1 text-xs
                   text-neutral-200 placeholder:text-neutral-600 focus:border-neutral-700
                   focus:outline-none"
        />
    {/if}

    {#snippet tree(parentId: string | null, depth: number)}
        {#each childrenByParent.get(parentId) ?? [] as folder (folder.id)}
            {@const index = indexById.get(folder.id) ?? 0}
            <!-- One highlight for both drag systems: OS files landing here and
                 assets dragged from the grid mean the same thing to this row. -->
            {@const dropping = dropzone.isOverFolder(folder.id) || drag.isOverFolder(folder.id)}
            {@const line = insertLine(folder.id)}
            {@const blocked =
                drag.forbidden && drag.target?.kind === "folder" && drag.target.id === folder.id}
            <!-- The whole row is the drop target, indent included: a thin strip
                 is hard to hit with a file already in hand. The native drag-drop
                 event carries no DOM target, so `dropzone` hit-tests these
                 attributes by coordinate — see droptarget.ts.

                 `relative` anchors the insertion lines, which are drawn as
                 absolutely-positioned children so they never shift the row and
                 make the tree jitter as the cursor crosses it. -->
            <div
                {...{ [DROP_FOLDER_ATTR]: folder.id, [DROP_FOLDER_NAME_ATTR]: folder.name }}
                class="group relative flex items-center rounded transition-colors
                    {dropping ? 'bg-emerald-600/25 ring-1 ring-emerald-500' : ''}
                    {blocked ? 'bg-red-600/15 ring-1 ring-red-600/50' : ''}"
                style="padding-left: {depth * 12}px"
            >
                <!-- Insertion line: "between these two", as distinct from the
                     row highlight's "inside this one". pointer-events-none so it
                     never intercepts the hit-test it exists to describe. -->
                {#if line}
                    <div
                        class="pointer-events-none absolute inset-x-0 z-10 h-0.5 bg-emerald-400
                            {line === 'before' ? '-top-px' : '-bottom-px'}"
                        style="margin-left: {depth * 12}px"
                    ></div>
                {/if}
                <button
                    type="button"
                    title={folder.name}
                    onpointerdown={(e) => pressFolder(e, index)}
                    onclick={(e) => clickFolder(e, folder)}
                    oncontextmenu={(e) => {
                        e.preventDefault();
                        menu = { folder, x: e.clientX, y: e.clientY };
                    }}
                    aria-pressed={treeSel.has(folder.id)}
                    aria-current={isScope(folder.id) ? "true" : undefined}
                    class="flex min-w-0 flex-1 items-center gap-1.5 rounded px-2 py-1 text-left
                        transition-colors
                        {treeSel.has(folder.id)
                        ? 'bg-blue-600 text-white'
                        : isScope(folder.id)
                          ? 'bg-neutral-800 text-neutral-100'
                          : 'text-neutral-300 hover:bg-neutral-800'}"
                >
                    <!-- Pin marker. Pinning must read as "marked", not "moved
                         somewhere else" — without a badge here the folder looks
                         like it left the tree for the pinned list. -->
                    {#if folder.pin_position !== null}
                        <span
                            class="h-1.5 w-1.5 shrink-0 rounded-full"
                            style="background-color: {pinColorVar(folder.color)}"
                            title="Pinned to the sidebar"
                        ></span>
                    {/if}
                    <span class="truncate">{folder.name}</span>
                </button>
                <div class="flex shrink-0 opacity-0 transition-opacity group-hover:opacity-100">
                    <button
                        type="button"
                        data-row-action
                        title="New subfolder"
                        onclick={() => newFolder(folder.id)}
                        class="px-1 text-neutral-500 hover:text-neutral-200">＋</button
                    >
                    <button
                        type="button"
                        data-row-action
                        title="Rename"
                        onclick={() => renameFolder(folder)}
                        class="px-1 text-neutral-500 hover:text-neutral-200">✎</button
                    >
                    <button
                        type="button"
                        data-row-action
                        title={treeSel.has(folder.id) && treeSel.size > 1
                            ? `Delete ${treeSel.size} selected folders`
                            : "Delete"}
                        onclick={() => deleteFolders(folder)}
                        class="px-1 text-neutral-500 hover:text-red-400">🗑</button
                    >
                </div>
            </div>
            {@render tree(folder.id, depth + 1)}
        {/each}
    {/snippet}

    {@render tree(rootId, 0)}

    {#if rootId && (childrenByParent.get(rootId) ?? []).length === 0}
        <p class="px-2 py-1 text-xs text-neutral-600">No subfolders.</p>
    {/if}

    {#if treeSel.size > 1}
        <!-- Multi-select is invisible unless something says so: the row actions
             only appear on hover, so the count is the only standing signal that
             a delete would take more than one folder. -->
        <div class="mt-1 flex items-center gap-2 px-2 text-xs text-blue-400">
            <span>{treeSel.size} selected</span>
            <button
                type="button"
                onclick={() => {
                    treeSel.clear();
                    selection.clear();
                }}
                class="text-neutral-500 hover:text-neutral-300">Clear</button
            >
        </div>
    {/if}

    <!-- Creates at the level being shown: the library root in the full tree, the
         pinned folder in its own flyout. -->
    <button
        type="button"
        onclick={() => newFolder(rootId)}
        class="mt-1 rounded px-2 py-1 text-left text-xs text-neutral-500 transition-colors hover:bg-neutral-800 hover:text-neutral-300"
    >
        ＋ {rootId ? "New subfolder" : "New folder"}
    </button>
</div>

{#if menu}
    <!-- `target` still reads through `menu` lazily, which is only safe because
         the menu runs an action BEFORE closing itself (see the invariant in
         FolderContextMenu). Close-then-act would evaluate this against null. -->
    {@const target = menu.folder}
    <!-- The structural actions are handed in rather than living in the menu:
         deleting from the tree can take the whole multi-selection with it, which
         only this component knows about. -->
    <FolderContextMenu
        folder={target}
        x={menu.x}
        y={menu.y}
        onclose={() => (menu = null)}
        onNewSubfolder={() => newFolder(target.id)}
        onRename={() => renameFolder(target)}
        onDelete={() => deleteFolders(target)}
    />
{/if}
