<script lang="ts">
    import { toast } from "svelte-sonner";
    import { assetLibrary, triStateOf, type Folder } from "$lib/assets.svelte";

    interface Props {
        assetIds: string[];
        legendClass: string;
    }
    let { assetIds, legendClass }: Props = $props();

    const total = $derived(assetIds.length);

    /** folder_id -> how many of the selected assets it holds. Absent means zero. */
    let counts = $state(new Map<string, number>());
    let loading = $state(true);
    let busy = $state<string | null>(null);

    $effect(() => {
        const ids = assetIds;
        let cancelled = false;
        loading = true;
        assetLibrary
            .fetchFolderMembership(ids)
            .then((rows) => {
                if (cancelled) return; // a newer selection superseded this fetch
                counts = new Map(rows.map((r) => [r.folder_id, r.count]));
                loading = false;
            })
            .catch(() => {
                if (cancelled) return;
                counts = new Map();
                loading = false;
            });
        return () => {
            cancelled = true;
        };
    });

    /** The tree flattened in display order, so nesting is legible in a flat list. */
    const ordered = $derived.by(() => {
        const byParent = new Map<string | null, Folder[]>();
        for (const f of assetLibrary.folders) {
            const arr = byParent.get(f.parent_id) ?? [];
            arr.push(f);
            byParent.set(f.parent_id, arr);
        }
        const out: { folder: Folder; depth: number }[] = [];
        const walk = (parent: string | null, depth: number) => {
            for (const f of byParent.get(parent) ?? []) {
                out.push({ folder: f, depth });
                walk(f.id, depth + 1);
            }
        };
        walk(null, 0);
        return out;
    });

    const stateOf = (id: string) => triStateOf(counts.get(id) ?? 0, total);

    /**
     * Toggle membership for the whole selection.
     *
     * Always an add/remove DELTA, never "write this folder's state onto every
     * asset" — which is precisely what lets a partial selection be safe to click.
     * A partial folder fills (matching Finder and Lightroom); clicking it again,
     * now full, empties it.
     */
    async function toggle(folderId: string) {
        if (busy) return;
        busy = folderId;
        try {
            if (stateOf(folderId) === "all") {
                await assetLibrary.removeAssetsFromFolder(folderId, assetIds);
            } else {
                await assetLibrary.addAssetsToFolder(folderId, assetIds);
            }
            // Re-read rather than adjusting locally: the write may have been a
            // partial no-op (INSERT OR IGNORE), so the DB is the only honest
            // source for the new state.
            const rows = await assetLibrary.fetchFolderMembership(assetIds);
            counts = new Map(rows.map((r) => [r.folder_id, r.count]));
        } catch (e) {
            toast.error(typeof e === "string" ? e : "Failed to update folders.");
        } finally {
            busy = null;
        }
    }
</script>

<div>
    <span class={legendClass}>Folders</span>

    {#if ordered.length === 0}
        <p class="text-xs text-neutral-600">No folders in this library yet.</p>
    {:else if loading}
        <p class="text-xs text-neutral-600">Reading membership…</p>
    {:else}
        <div class="max-h-40 overflow-y-auto rounded border border-neutral-800">
            {#each ordered as { folder, depth } (folder.id)}
                {@const state = stateOf(folder.id)}
                <button
                    type="button"
                    onclick={() => toggle(folder.id)}
                    disabled={busy !== null}
                    aria-pressed={state === "all"}
                    title={state === "some"
                        ? `${counts.get(folder.id)} of ${total} selected assets`
                        : folder.name}
                    class="flex w-full items-center gap-2 px-2 py-1 text-left text-xs
                           text-neutral-300 hover:bg-neutral-800 disabled:opacity-50"
                    style="padding-left: {8 + depth * 12}px"
                >
                    <!-- Three states, not two: a filled box would claim every
                         selected asset is in this folder, and an empty one would
                         claim none is. Both are lies for a partial selection. -->
                    <span
                        aria-hidden="true"
                        class="flex h-3.5 w-3.5 shrink-0 items-center justify-center rounded-sm border
                               text-[9px] leading-none text-white
                               {state === 'none'
                            ? 'border-neutral-600'
                            : 'border-blue-500 bg-blue-600'}"
                    >
                        {state === "all" ? "✓" : state === "some" ? "–" : ""}
                    </span>
                    <span class="truncate">{folder.name}</span>
                </button>
            {/each}
        </div>
    {/if}
</div>
