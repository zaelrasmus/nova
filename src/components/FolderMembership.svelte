<script lang="ts">
    import { untrack } from "svelte";
    import { toast } from "svelte-sonner";
    import { Check, Folder as FolderIcon, Minus, Plus, X } from "@lucide/svelte";
    import { assetLibrary, triStateOf, type Folder } from "$lib/assets.svelte";
    import Popover from "./controls/Popover.svelte";

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

    // Keyed on the ids' CONTENT, not the array: the inspector builds a fresh
    // `[asset.id]` whenever the asset row refreshes (as it does after a toggle),
    // and refetching then flipped `loading`, which unmounted the open Add list
    // mid-use. `toggle` already re-reads membership itself.
    const idsKey = $derived(assetIds.join("\u0000"));

    $effect(() => {
        void idsKey;
        const ids = untrack(() => assetIds);
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

    /** Folders holding at least one selected asset, in tree order. These are the chips. */
    const held = $derived(ordered.filter(({ folder }) => (counts.get(folder.id) ?? 0) > 0));

    let adding = $state(false);

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
        <div class="flex flex-wrap gap-1.5">
            <!-- A chip toggles exactly like the row in the Add list: a full one
                 empties, a partial one fills. Partial chips are dashed and carry
                 their count, so the two never read as the same claim. -->
            {#each held as { folder } (folder.id)}
                {@const state = stateOf(folder.id)}
                <button
                    type="button"
                    onclick={() => toggle(folder.id)}
                    disabled={busy !== null}
                    title={state === "some"
                        ? `${counts.get(folder.id)} of ${total} selected assets — click to add the rest`
                        : `Remove from ${folder.name}`}
                    class="group flex h-[22px] max-w-full items-center gap-1.5 rounded-md border pr-1 pl-1.5
                           text-xs transition-colors disabled:opacity-50
                           {state === 'all'
                        ? 'border-transparent bg-raised-2 text-neutral-100'
                        : 'border-dashed border-line-strong text-neutral-400 hover:text-neutral-200'}"
                >
                    <FolderIcon class="h-3 w-3 shrink-0 text-neutral-500" />
                    <span class="truncate">{folder.name}</span>
                    {#if state === "some"}
                        <span class="text-[10.5px] tabular-nums text-neutral-500"
                            >{counts.get(folder.id)}/{total}</span
                        >
                    {/if}
                    <span
                        aria-hidden="true"
                        class="grid h-4 w-4 shrink-0 place-items-center rounded text-neutral-500
                               group-hover:bg-wash-strong group-hover:text-neutral-100"
                    >
                        {#if state === "all"}
                            <X class="h-3 w-3" />
                        {:else}
                            <Plus class="h-3 w-3" />
                        {/if}
                    </span>
                </button>
            {/each}

            <Popover bind:open={adding} label="Add to folder" align="start" width="w-64">
                {#snippet trigger({ open, toggle: toggleList })}
                    <button
                        type="button"
                        onclick={toggleList}
                        aria-expanded={open}
                        class="flex h-[22px] items-center gap-1 rounded-md border border-dashed pr-2 pl-1.5
                               text-xs transition-colors hover:border-neutral-600 hover:text-neutral-200
                               {open
                            ? 'border-neutral-600 bg-wash text-neutral-200'
                            : 'border-line-strong text-neutral-500'}"
                    >
                        <Plus class="h-3 w-3" />
                        {held.length > 0 ? "Add" : total > 1 ? "Add all to folder" : "Add to folder"}
                    </button>
                {/snippet}
                {#snippet children()}
                    <div class="nv-menu-label">Folders</div>
                    <div class="max-h-72 overflow-y-auto">
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
                                class="nv-menu-item text-[12.5px]"
                                style="padding-left: {8 + depth * 14}px"
                            >
                                <!-- Three states, not two: a filled box would claim every
                                     selected asset is in this folder, and an empty one would
                                     claim none is. Both are lies for a partial selection. -->
                                <span
                                    aria-hidden="true"
                                    class="grid h-3.5 w-3.5 shrink-0 place-items-center rounded-[4px] text-white
                                           {state === 'none'
                                        ? 'shadow-[inset_0_0_0_1px_var(--nv-line-strong)]'
                                        : 'bg-brand'}"
                                >
                                    {#if state === "all"}
                                        <Check class="h-2.5 w-2.5" strokeWidth={3} />
                                    {:else if state === "some"}
                                        <Minus class="h-2.5 w-2.5" strokeWidth={3} />
                                    {/if}
                                </span>
                                <span class="truncate">{folder.name}</span>
                                {#if state === "some"}
                                    <span class="ml-auto text-[11px] tabular-nums text-neutral-500"
                                        >{counts.get(folder.id)}/{total}</span
                                    >
                                {/if}
                            </button>
                        {/each}
                    </div>
                {/snippet}
            </Popover>
        </div>
    {/if}
</div>
