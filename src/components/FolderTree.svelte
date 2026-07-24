<script lang="ts">
    import { assetLibrary, type Folder, type ManifestScope } from "$lib/assets.svelte";
    import { selection } from "$lib/selection.svelte";
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
     * Navigating and inspecting are two different things that one click happens
     * to do together: the scope decides which assets the grid queries, the
     * selection decides what the inspector shows.
     *
     * They're kept as separate state rather than derived from each other because
     * "All" and "Uncategorized" are scopes with NO folder row behind them — no
     * name, no notes, no timestamp — so they select nothing at all.
     */
    function select(scope: ManifestScope) {
        if (scope.kind === "folder") selection.selectFolder(scope.id);
        else selection.clear();
        return assetLibrary.setScope(scope);
    }

    const isFolder = (id: string) => active.kind === "folder" && active.id === id;

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

    async function deleteFolder(folder: Folder) {
        const ok = window.confirm(
            `Delete "${folder.name}" and its subfolders? Your assets stay in the library.`,
        );
        if (!ok) return;
        try {
            await assetLibrary.deleteFolder(folder.id);
        } catch (e) {
            toast.error(typeof e === "string" ? e : "Failed to delete folder.");
        }
    }
</script>

<div class="flex flex-col gap-0.5 rounded-lg border border-neutral-800 bg-neutral-900/40 p-2 text-sm">
    <button
        type="button"
        onclick={() => select({ kind: "all" })}
        class="rounded px-2 py-1 text-left transition-colors
            {active.kind === 'all' ? 'bg-blue-600 text-white' : 'text-neutral-300 hover:bg-neutral-800'}"
    >
        All assets
    </button>
    <button
        type="button"
        onclick={() => select({ kind: "uncategorized" })}
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
            <div class="group flex items-center" style="padding-left: {depth * 12}px">
                <button
                    type="button"
                    title={folder.name}
                    onclick={() => select({ kind: "folder", id: folder.id })}
                    class="flex-1 truncate rounded px-2 py-1 text-left transition-colors
                        {isFolder(folder.id) ? 'bg-blue-600 text-white' : 'text-neutral-300 hover:bg-neutral-800'}"
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
                        title="Delete"
                        onclick={() => deleteFolder(folder)}
                        class="px-1 text-neutral-500 hover:text-red-400">🗑</button
                    >
                </div>
            </div>
            {@render tree(folder.id, depth + 1)}
        {/each}
    {/snippet}

    {@render tree(null, 0)}

    <button
        type="button"
        onclick={() => newFolder(null)}
        class="mt-1 rounded px-2 py-1 text-left text-xs text-neutral-500 transition-colors hover:bg-neutral-800 hover:text-neutral-300"
    >
        ＋ New folder
    </button>
</div>
