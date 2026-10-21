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

  Each tile is a folder drop target, exactly like its sidebar row: assets
  dragged from the grid file into it (Shift moves), and files dragged in from
  the OS import into it. Both drag systems find it by its `data-drop-folder`
  attribute (see droptarget.ts), so no drop logic lives here.
-->
<script lang="ts">
    import { convertFileSrc, invoke } from "@tauri-apps/api/core";
    import { Folder } from "@lucide/svelte";
    import {
        assetLibrary,
        thumbHashUrl,
        type AssetMetadata,
        type FolderChild,
    } from "$lib/assets.svelte";
    import { selection } from "$lib/selection.svelte";
    import { drag } from "$lib/dragdrop.svelte";
    import { dropzone } from "$lib/dropzone.svelte";
    import { DROP_FOLDER_ATTR, DROP_FOLDER_NAME_ATTR } from "$lib/droptarget";

    interface Props {
        /** How many tiles are showing, so the grid can label its sections. */
        count?: number;
        /** Take the whole pane: the folder has no assets of its own. */
        fill?: boolean;
        /** Tiles per row — the grid's Size/Columns slider, so folders and
         *  assets resize together. */
        columns: number;
        /** Gap between tiles, px — the grid's, so the two sections line up. */
        gap: number;
    }

    let { count = $bindable(0), fill = false, columns, gap }: Props = $props();

    const parentId = $derived(
        assetLibrary.scope.kind === "folder" ? assetLibrary.scope.id : null,
    );

    let children = $state<FolderChild[]>([]);
    let covers = $state(new Map<string, AssetMetadata>());

    // Refetch when the place changes, when the tree changes (create, rename,
    // move, delete), when the manifest reloads (assets moved or trashed from
    // here) and when assets are filed into a child (a drop on a tile reloads
    // nothing else). A token drops responses that arrive after the user has
    // already moved on.
    let token = 0;
    $effect(() => {
        const id = parentId;
        void assetLibrary.folders;
        void assetLibrary.manifest;
        void assetLibrary.membershipVersion;
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
        class="shrink-0 overflow-y-auto border-b border-line px-[var(--nv-grid-mx)] pb-4 pt-3
               {fill ? 'min-h-0 flex-1 border-b-0' : 'max-h-[45%]'}"
        aria-label="Folders"
    >
        <h2 class="mb-2.5 flex items-baseline gap-2 text-xs font-semibold text-neutral-400">
            Folders
            <span class="font-medium tabular-nums text-neutral-600">{children.length}</span>
        </h2>
        <div
            class="grid"
            style="grid-template-columns: repeat({columns}, minmax(0, 1fr)); gap: {gap}px;"
        >
            {#each children as child (child.folder_id)}
                {@const src = coverSrc(child.cover_id ? covers.get(child.cover_id) : undefined)}
                {@const selected = selectedFolder === child.folder_id}
                {@const name = nameOf.get(child.folder_id) ?? "Folder"}
                <!-- Same highlight as the tree row, for both drag systems. -->
                {@const dropping =
                    dropzone.isOverFolder(child.folder_id) || drag.isOverFolder(child.folder_id)}
                {@const blocked =
                    drag.forbidden &&
                    drag.target?.kind === "folder" &&
                    drag.target.id === child.folder_id}
                <button
                    type="button"
                    {...{ [DROP_FOLDER_ATTR]: child.folder_id, [DROP_FOLDER_NAME_ATTR]: name }}
                    class="group flex min-w-0 flex-col gap-1.5 rounded-[calc(var(--nv-tile-radius)+4px)] p-1.5
                           text-left transition-colors
                           focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-brand-line
                           {dropping
                        ? 'bg-emerald-600/25 ring-1 ring-emerald-500'
                        : blocked
                          ? 'bg-red-600/15 ring-1 ring-red-600/50'
                          : selected
                            ? 'bg-brand-soft'
                            : 'hover:bg-wash'}"
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
                        class="relative aspect-[3/4] w-full overflow-hidden rounded-[var(--nv-tile-radius)] bg-raised
                               {selected
                            ? 'ring-2 ring-brand'
                            : 'ring-1 ring-inset ring-white/[0.06]'}"
                    >
                        {#if src}
                            <img {src} alt="" class="absolute inset-0 h-full w-full object-cover" loading="lazy" />
                        {:else}
                            <div class="grid h-full place-items-center text-neutral-600">
                                <Folder class="h-8 w-8" strokeWidth={1.25} />
                            </div>
                        {/if}
                    </div>
                    <span class="truncate px-0.5 text-xs font-medium text-neutral-200">
                        {name}
                    </span>
                    <span class="-mt-1 truncate px-0.5 text-[11px] text-neutral-500">{describe(child)}</span>
                </button>
            {/each}
        </div>
    </section>
{/if}
