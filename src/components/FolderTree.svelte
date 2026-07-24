<script lang="ts">
    import { assetLibrary, type Folder, type ManifestScope } from "$lib/assets.svelte";
    import { RangeSelection, selection } from "$lib/selection.svelte";
    import { toast } from "svelte-sonner";

    const folders = $derived(assetLibrary.folders);
    const active = $derived(assetLibrary.scope);

    // parent_id -> children[], preserving the backend's position order.
    const childrenByParent = $derived.by(() => {
        const map = new Map<string | null, Folder[]>();
        for (const f of folders) {
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
        walk(null);
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

    /** The default views: navigate, and drop every selection — they own nothing. */
    function selectView(scope: ManifestScope) {
        treeSel.clear();
        selection.clear();
        return assetLibrary.setScope(scope);
    }

    const isScope = (id: string) => active.kind === "folder" && active.id === id;

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

<div class="flex flex-col gap-0.5 rounded-lg border border-neutral-800 bg-neutral-900/40 p-2 text-sm">
    <button
        type="button"
        onclick={() => selectView({ kind: "all" })}
        class="rounded px-2 py-1 text-left transition-colors
            {active.kind === 'all' ? 'bg-blue-600 text-white' : 'text-neutral-300 hover:bg-neutral-800'}"
    >
        All assets
    </button>
    <button
        type="button"
        onclick={() => selectView({ kind: "uncategorized" })}
        class="rounded px-2 py-1 text-left transition-colors
            {active.kind === 'uncategorized' ? 'bg-blue-600 text-white' : 'text-neutral-400 hover:bg-neutral-800'}"
    >
        Uncategorized
    </button>

    {#if folders.length > 0}
        <div class="my-1 h-px bg-neutral-800"></div>
    {/if}

    {#snippet tree(parentId: string | null, depth: number)}
        {#each childrenByParent.get(parentId) ?? [] as folder (folder.id)}
            {@const index = indexById.get(folder.id) ?? 0}
            <div class="group flex items-center" style="padding-left: {depth * 12}px">
                <button
                    type="button"
                    title={folder.name}
                    onpointerdown={(e) => pressFolder(e, index)}
                    onclick={(e) => clickFolder(e, folder)}
                    aria-pressed={treeSel.has(folder.id)}
                    aria-current={isScope(folder.id) ? "true" : undefined}
                    class="flex-1 truncate rounded px-2 py-1 text-left transition-colors
                        {treeSel.has(folder.id)
                        ? 'bg-blue-600 text-white'
                        : isScope(folder.id)
                          ? 'bg-neutral-800 text-neutral-100'
                          : 'text-neutral-300 hover:bg-neutral-800'}"
                >
                    {folder.name}
                </button>
                <div class="flex shrink-0 opacity-0 transition-opacity group-hover:opacity-100">
                    <button
                        type="button"
                        title="New subfolder"
                        onclick={() => newFolder(folder.id)}
                        class="px-1 text-neutral-500 hover:text-neutral-200">＋</button
                    >
                    <button
                        type="button"
                        title="Rename"
                        onclick={() => renameFolder(folder)}
                        class="px-1 text-neutral-500 hover:text-neutral-200">✎</button
                    >
                    <button
                        type="button"
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

    {@render tree(null, 0)}

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

    <button
        type="button"
        onclick={() => newFolder(null)}
        class="mt-1 rounded px-2 py-1 text-left text-xs text-neutral-500 transition-colors hover:bg-neutral-800 hover:text-neutral-300"
    >
        ＋ New folder
    </button>
</div>
