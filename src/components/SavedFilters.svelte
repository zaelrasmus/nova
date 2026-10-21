<!--
  SavedFilters — named, reusable filter combinations in the sidebar.

  A saved filter is a LENS: applying one narrows whatever scope you're currently
  in, and clearing it leaves you where you were. Contrast SmartFolders.svelte,
  whose entries are PLACES you navigate to. Both are rows in `rule_sets` and
  share one editor and one compiler — only `kind` differs.

  The live search text is deliberately NOT saved: it isn't part of the rule tree,
  so it structurally cannot reach storage.
-->
<script lang="ts">
    import { Filter, Pencil, Plus, RefreshCw, Trash2 } from "@lucide/svelte";
    import { assetLibrary, type SavedFilter } from "$lib/assets.svelte";
    import { describeRules } from "$lib/rules";
    import { toast } from "svelte-sonner";

    const saved = $derived(assetLibrary.savedFilters);
    const canSave = $derived(assetLibrary.hasFilters);

    const fail = (e: unknown, fallback: string) =>
        toast.error(typeof e === "string" ? e : fallback);

    async function saveCurrent() {
        const name = window.prompt("Name this filter:");
        if (!name?.trim()) return;
        try {
            await assetLibrary.saveCurrentFilters(name.trim());
        } catch (e) {
            fail(e, "Failed to save filter.");
        }
    }

    async function rename(filter: SavedFilter) {
        const name = window.prompt("Rename filter:", filter.name);
        if (!name?.trim() || name.trim() === filter.name) return;
        try {
            await assetLibrary.renameSavedFilter(filter.id, name.trim());
        } catch (e) {
            fail(e, "Failed to rename filter.");
        }
    }

    async function update(filter: SavedFilter) {
        const ok = window.confirm(`Replace "${filter.name}" with the filters active now?`);
        if (!ok) return;
        try {
            await assetLibrary.updateSavedFilter(filter.id);
        } catch (e) {
            fail(e, "Failed to update filter.");
        }
    }

    async function remove(filter: SavedFilter) {
        const ok = window.confirm(`Delete "${filter.name}"? Your assets aren't affected.`);
        if (!ok) return;
        try {
            await assetLibrary.deleteSavedFilter(filter.id);
        } catch (e) {
            fail(e, "Failed to delete filter.");
        }
    }

    /**
     * One-line summary of what a saved filter narrows, for the row's tooltip.
     * Shared with the rule editor and (from Phase 5) a smart folder's flyout, so
     * the same rule reads the same way wherever it's shown.
     */
    const describe = (f: SavedFilter) => describeRules(f.rules, assetLibrary.folderNames);
</script>

<!-- No heading of its own: the sidebar section and the rail flyout both title
     it already. -->
<div class="flex flex-col gap-px">
    {#each saved as filter (filter.id)}
        <div class="group relative flex items-center rounded-md">
            <!-- Applying is a lens over the CURRENT scope: clicking this does not
                 move you out of the folder you're in. -->
            <button
                type="button"
                title={describe(filter)}
                onclick={() => assetLibrary.applySavedFilter(filter.id)}
                class="flex h-[var(--nv-row-h)] min-w-0 flex-1 items-center gap-2.5 rounded-md px-2
                       text-left text-[13px] text-neutral-400 transition-colors
                       group-hover:bg-wash group-hover:text-neutral-100"
            >
                <Filter class="h-4 w-4 shrink-0 text-neutral-500" strokeWidth={1.5} />
                <span class="min-w-0 flex-1 truncate">{filter.name}</span>
            </button>
            <!-- Overlaid rather than reserving width, like the folder tree's
                 row actions. -->
            <div
                class="pointer-events-none invisible absolute inset-y-0 right-0 [&_button]:pointer-events-auto [&_select]:pointer-events-auto flex items-center gap-px rounded-r-md pl-4
                       pr-1 group-focus-within:visible group-hover:visible
                       [background:linear-gradient(var(--nv-wash),var(--nv-wash)),var(--nv-pane)]
                       [mask-image:linear-gradient(to_right,transparent,black_14px)]"
            >
                {#if canSave}
                    <button
                        type="button"
                        title="Replace with the filters active now"
                        aria-label="Replace with the filters active now"
                        onclick={() => update(filter)}
                        class="grid h-[22px] w-[22px] place-items-center rounded-[5px] text-neutral-500
                               transition-colors hover:bg-wash-strong hover:text-neutral-100"
                    >
                        <RefreshCw class="h-3.5 w-3.5" />
                    </button>
                {/if}
                <button
                    type="button"
                    title="Rename"
                    aria-label="Rename"
                    onclick={() => rename(filter)}
                    class="grid h-[22px] w-[22px] place-items-center rounded-[5px] text-neutral-500
                           transition-colors hover:bg-wash-strong hover:text-neutral-100"
                >
                    <Pencil class="h-3.5 w-3.5" />
                </button>
                <button
                    type="button"
                    title="Delete"
                    aria-label="Delete"
                    onclick={() => remove(filter)}
                    class="grid h-[22px] w-[22px] place-items-center rounded-[5px] text-neutral-500
                           transition-colors hover:bg-wash-strong hover:text-red-400"
                >
                    <Trash2 class="h-3.5 w-3.5" />
                </button>
            </div>
        </div>
    {:else}
        <span class="px-2 py-1 text-xs leading-relaxed text-neutral-600">
            Set some filters, then save them here.
        </span>
    {/each}

    <button
        type="button"
        onclick={saveCurrent}
        disabled={!canSave}
        title={canSave ? "Save the active filters" : "No filters are active"}
        class="flex h-[var(--nv-row-h)] items-center gap-2.5 rounded-md px-2 text-left text-[13px]
               transition-colors enabled:text-neutral-500 enabled:hover:bg-wash
               enabled:hover:text-neutral-200 disabled:cursor-not-allowed disabled:text-neutral-700"
    >
        <Plus class="h-4 w-4 shrink-0" strokeWidth={1.5} />
        Save current filters
    </button>
</div>
