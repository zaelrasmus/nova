<script lang="ts">
    import { assetLibrary, type SavedFilter } from "$lib/assets.svelte";
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

    /** One-line summary of what a saved filter narrows, for the row's tooltip. */
    function describe(f: SavedFilter): string {
        const parts: string[] = [];
        if (f.filters.asset_types.length) parts.push(f.filters.asset_types.join(", "));
        if (f.filters.shape) parts.push(`shape: ${f.filters.shape.kind}`);
        if (f.filters.date) parts.push(`by ${f.filters.date.field.replace("_", " ")}`);
        if (f.filters.size) parts.push("size range");
        return parts.length ? parts.join(" · ") : "no conditions";
    }
</script>

<div
    class="flex flex-col gap-0.5 rounded-lg border border-neutral-800 bg-neutral-900/40 p-2 text-sm"
>
    <span class="px-2 pb-1 text-[10px] font-medium uppercase tracking-wide text-neutral-500">
        Saved filters
    </span>

    {#each saved as filter (filter.id)}
        <div class="group flex items-center">
            <!-- Applying is a lens over the CURRENT scope: clicking this does not
                 move you out of the folder you're in. -->
            <button
                type="button"
                title={describe(filter)}
                onclick={() => assetLibrary.applySavedFilter(filter.id)}
                class="flex-1 truncate rounded px-2 py-1 text-left text-neutral-300
                       transition-colors hover:bg-neutral-800"
            >
                {filter.name}
            </button>
            <div class="flex shrink-0 opacity-0 transition-opacity group-hover:opacity-100">
                {#if canSave}
                    <button
                        type="button"
                        title="Replace with the filters active now"
                        onclick={() => update(filter)}
                        class="px-1 text-neutral-500 hover:text-neutral-200">⭯</button
                    >
                {/if}
                <button
                    type="button"
                    title="Rename"
                    onclick={() => rename(filter)}
                    class="px-1 text-neutral-500 hover:text-neutral-200">✎</button
                >
                <button
                    type="button"
                    title="Delete"
                    onclick={() => remove(filter)}
                    class="px-1 text-neutral-500 hover:text-red-400">🗑</button
                >
            </div>
        </div>
    {:else}
        <span class="px-2 py-1 text-xs text-neutral-600">
            Set some filters, then save them here.
        </span>
    {/each}

    <button
        type="button"
        onclick={saveCurrent}
        disabled={!canSave}
        title={canSave ? "Save the active filters" : "No filters are active"}
        class="mt-1 rounded px-2 py-1 text-left text-xs transition-colors
               enabled:text-neutral-500 enabled:hover:bg-neutral-800 enabled:hover:text-neutral-300
               disabled:cursor-not-allowed disabled:text-neutral-700"
    >
        ＋ Save current filters
    </button>
</div>
