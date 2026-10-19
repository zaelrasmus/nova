<!--
  FolderTiles — the current folder's direct subfolders, as tiles above its
  assets. Lets a folder of folders (a series of chapters, a shoot of days) be
  browsed from the grid instead of only from the sidebar tree.

  Rendered outside the virtualized grid on purpose: tiles are folders, not
  assets, and mixing them into the manifest would break counts, selection,
  reorder and drag & drop, which all assume every item is an asset. A folder
  view lists tens to hundreds of children, so a plain grid is enough.

  Single click selects the folder (the inspector shows it, as in the tree);
  double click or Enter opens it, and the tree's highlight follows the scope.
-->
<script lang="ts">
    import { convertFileSrc, invoke } from "@tauri-apps/api/core";
    import {
        assetLibrary,
        thumbHashUrl,
        type AssetMetadata,
        type FolderChild,
    } from "$lib/assets.svelte";
    import { selection } from "$lib/selection.svelte";

    interface Props {
        /** How many tiles are showing, so the grid can label its sections. */
        count?: number;
        /** Take the whole pane: the folder has no assets of its own. */
        fill?: boolean;
    }

    let { count = $bindable(0), fill = false }: Props = $props();

    const parentId = $derived(
        assetLibrary.scope.kind === "folder" ? assetLibrary.scope.id : null,
    );

    let children = $state<FolderChild[]>([]);
    let covers = $state(new Map<string, AssetMetadata>());

    // Refetch when the place changes, when the tree changes (create, rename,
    // move, delete) and when the manifest reloads (assets moved or trashed from
    // here change the children's counts and covers). A token drops responses
    // that arrive after the user has already moved on.
    let token = 0;
    $effect(() => {
        const id = parentId;
        void assetLibrary.folders;
        void assetLibrary.manifest;
        const mine = ++token;
        if (id === null) {
            children = [];
            covers = new Map();
            return;
        }
        (async () => {
            try {
                const next = await assetLibrary.fetchFolderChildren(id);
                const ids = next.flatMap((c) => (c.cover_id ? [c.cover_id] : []));
                const rows =
                    ids.length > 0
                        ? await invoke<AssetMetadata[]>("fetch_assets_by_ids", { ids })
                        : [];
                if (mine !== token) return;
                children = next;
                covers = new Map(rows.map((r) => [r.id, r]));
            } catch (e) {
                if (mine !== token) return;
                console.error("Failed to load subfolders", e);
                children = [];
                covers = new Map();
            }
        })();
    });

    $effect(() => {
        count = children.length;
    });

    const selectedFolder = $derived(
        selection.current.kind === "folder" ? selection.current.id : null,
    );

    const nameOf = $derived(new Map(assetLibrary.folders.map((f) => [f.id, f.name])));

    function coverSrc(row: AssetMetadata | undefined): string | null {
        if (!row) return null;
        if (row.thumb_path) {
            return `${convertFileSrc(row.thumb_path)}?v=${assetLibrary.thumbVersion}`;
        }
        if (row.asset_type === "image" && row.dest_path) return convertFileSrc(row.dest_path);
        return thumbHashUrl(row.thumb_hash);
    }

    function describe(c: FolderChild): string {
        const items = `${c.item_count} ${c.item_count === 1 ? "item" : "items"}`;
        if (c.subfolder_count === 0) return items;
        const folders = `${c.subfolder_count} ${c.subfolder_count === 1 ? "folder" : "folders"}`;
        return c.item_count === 0 ? folders : `${items} · ${folders}`;
    }

    function open(id: string) {
        selection.selectFolder(id);
        assetLibrary.setScope({ kind: "folder", id });
    }
</script>

{#if children.length > 0}
    <section
        class="shrink-0 overflow-y-auto border-b border-neutral-800 px-3 pb-3 pt-2
               {fill ? 'min-h-0 flex-1 border-b-0' : 'max-h-[45%]'}"
        aria-label="Folders"
    >
        <h2 class="mb-2 text-xs font-medium text-neutral-500">
            Folders · {children.length}
        </h2>
        <div class="grid grid-cols-[repeat(auto-fill,minmax(120px,1fr))] gap-3">
            {#each children as child (child.folder_id)}
                {@const src = coverSrc(child.cover_id ? covers.get(child.cover_id) : undefined)}
                {@const selected = selectedFolder === child.folder_id}
                <button
                    type="button"
                    class="group flex min-w-0 flex-col gap-1.5 rounded-md p-1 text-left
                           focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-neutral-400
                           {selected ? 'bg-neutral-800' : 'hover:bg-neutral-900'}"
                    onclick={() => selection.selectFolder(child.folder_id)}
                    ondblclick={() => open(child.folder_id)}
                    onkeydown={(e) => {
                        if (e.key === "Enter") {
                            e.preventDefault();
                            open(child.folder_id);
                        }
                    }}
                >
                    <div
                        class="relative aspect-[3/4] w-full overflow-hidden rounded bg-neutral-900
                               ring-1 {selected ? 'ring-blue-500' : 'ring-white/10'}"
                    >
                        {#if src}
                            <img {src} alt="" class="absolute inset-0 h-full w-full object-cover" loading="lazy" />
                        {:else}
                            <div class="grid h-full place-items-center text-2xl text-neutral-700">📁</div>
                        {/if}
                    </div>
                    <span class="truncate text-xs font-medium text-neutral-200">
                        {nameOf.get(child.folder_id) ?? "Folder"}
                    </span>
                    <span class="-mt-1 truncate text-[11px] text-neutral-500">{describe(child)}</span>
                </button>
            {/each}
        </div>
    </section>
{/if}
