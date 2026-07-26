<script lang="ts">
    import { assetLibrary, ORDER_BY_LABELS, type OrderBy } from "$lib/assets.svelte";

    const sort = $derived(assetLibrary.sort);

    // "Date added to folder" only means something in a folder — outside one it
    // silently falls back to the import date, which is a different question.
    // A group of smart folders has no manual order: its contents are a union of
    // queries the user never arranged as one list. The backend coerces a stored
    // `manual` away too — this just keeps it off the menu.
    const inFolder = $derived(assetLibrary.scope.kind === "folder");
    const inGroup = $derived(assetLibrary.scope.kind === "smart_group");
    const options = $derived(
        ORDER_BY_LABELS.filter(
            (o) => (!o.folderOnly || inFolder) && !(inGroup && o.value === "manual"),
        ),
    );

    // Switching criterion picks the direction people actually expect: newest and
    // largest first for dates/size, A→Z and first-added-first for name/manual.
    function setCriterion(order_by: OrderBy) {
        if (order_by === sort.order_by) return;
        const is_ascending = order_by === "filename" || order_by === "manual";
        assetLibrary.setSort({ order_by, is_ascending });
    }

    const toggleDirection = () =>
        assetLibrary.setSort({ ...sort, is_ascending: !sort.is_ascending });
</script>

<div class="flex items-center gap-1.5">
    <span class="text-xs text-neutral-500">Sort</span>
    <select
        value={sort.order_by}
        onchange={(e) => setCriterion(e.currentTarget.value as OrderBy)}
        aria-label="Sort by"
        class="rounded border border-neutral-300 bg-white px-1.5 py-0.5 text-xs text-neutral-700
               focus:outline-none focus-visible:ring-1 focus-visible:ring-neutral-400"
    >
        {#each options as opt (opt.value)}
            <option value={opt.value}>{opt.label}</option>
        {/each}
    </select>
    <button
        type="button"
        onclick={toggleDirection}
        title={sort.is_ascending ? "Ascending" : "Descending"}
        aria-label={sort.is_ascending ? "Sort ascending" : "Sort descending"}
        class="rounded px-1.5 py-0.5 text-xs text-neutral-500
               hover:bg-neutral-200 hover:text-neutral-800"
    >
        {sort.is_ascending ? "↑" : "↓"}
    </button>
</div>
