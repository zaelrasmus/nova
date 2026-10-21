<!--
  SortControl — the grid toolbar's sort criterion + direction.

  A sort belongs to the SCOPE and is persisted there, so each place remembers how
  you left it. Options are filtered per scope: the two scope-relative criteria
  ("Date added to folder", "Manual") mean something precise in a folder and
  degrade to a fallback elsewhere, so they're only offered where they're honest.
-->
<script lang="ts">
    import { ArrowDownWideNarrow, ArrowUpNarrowWide, Check } from "@lucide/svelte";
    import { assetLibrary, ORDER_BY_LABELS, type OrderBy } from "$lib/assets.svelte";
    import Popover from "./controls/Popover.svelte";

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

    const currentLabel = $derived(
        ORDER_BY_LABELS.find((o) => o.value === sort.order_by)?.label ?? "Sort",
    );
</script>

<!-- Criterion and direction are two controls on purpose: flipping the order is
     the common move, and burying it inside the menu would make it two clicks. -->
<div class="flex shrink-0 items-center">
    <Popover label="Sort by" align="start" width="w-56">
        {#snippet trigger({ open, toggle })}
            <button
                type="button"
                onclick={toggle}
                aria-expanded={open}
                aria-label="Sort by {currentLabel}"
                title="Sort by"
                class="flex h-7 items-center rounded-l-md pl-2 pr-1.5 text-xs font-medium transition-colors
                       {open
                    ? 'bg-wash-strong text-neutral-100'
                    : 'text-neutral-400 hover:bg-wash hover:text-neutral-100'}"
            >
                {currentLabel}
            </button>
        {/snippet}
        {#snippet children({ close })}
            <p class="nv-menu-label">Sort by</p>
            {#each options as opt (opt.value)}
                <button
                    type="button"
                    role="menuitemradio"
                    aria-checked={sort.order_by === opt.value}
                    onclick={() => {
                        setCriterion(opt.value);
                        close();
                    }}
                    class="nv-menu-item"
                >
                    <span class="grid w-3.5 place-items-center text-brand-text">
                        {#if sort.order_by === opt.value}<Check class="h-3.5 w-3.5" />{/if}
                    </span>
                    {opt.label}
                </button>
            {/each}
        {/snippet}
    </Popover>
    <button
        type="button"
        onclick={toggleDirection}
        title={sort.is_ascending ? "Ascending — click for descending" : "Descending — click for ascending"}
        aria-label={sort.is_ascending ? "Sort ascending" : "Sort descending"}
        class="grid h-7 w-6 place-items-center rounded-r-md text-neutral-500 transition-colors
               hover:bg-wash hover:text-neutral-200"
    >
        {#if sort.is_ascending}
            <ArrowUpNarrowWide class="h-3.5 w-3.5" />
        {:else}
            <ArrowDownWideNarrow class="h-3.5 w-3.5" />
        {/if}
    </button>
</div>

