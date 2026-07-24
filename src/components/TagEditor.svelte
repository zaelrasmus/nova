<script lang="ts">
    import { untrack } from "svelte";
    import { toast } from "svelte-sonner";
    import { assetLibrary, triStateOf, type Tag } from "$lib/assets.svelte";
    import { selection } from "$lib/selection.svelte";

    interface Props {
        assetIds: string[];
        legendClass: string;
    }
    let { assetIds, legendClass }: Props = $props();

    let input = $state<HTMLInputElement | null>(null);

    // The `T` shortcut bumps a nonce; focus the field when it changes. Skip the
    // first run so mounting doesn't steal focus. `untrack` on the focus call keeps
    // this keyed strictly to the nonce.
    let lastNonce = selection.tagFocusNonce;
    $effect(() => {
        const nonce = selection.tagFocusNonce;
        if (nonce !== lastNonce) {
            lastNonce = nonce;
            untrack(() => input?.focus());
        }
    });

    const total = $derived(assetIds.length);

    /** tag_id -> how many of the selected assets carry it. Absent means zero. */
    let counts = $state(new Map<string, number>());
    let loading = $state(true);
    let busy = $state(false);

    let query = $state("");
    let open = $state(false);

    async function refreshCounts(ids: string[]) {
        const rows = await assetLibrary.fetchTagUsage(ids);
        counts = new Map(rows.map((r) => [r.tag_id, r.count]));
    }

    $effect(() => {
        const ids = assetIds;
        let cancelled = false;
        loading = true;
        refreshCounts(ids)
            .catch(() => {
                if (!cancelled) counts = new Map();
            })
            .finally(() => {
                if (!cancelled) loading = false;
            });
        return () => {
            cancelled = true; // a newer selection superseded this fetch
        };
    });

    const stateOf = (tagId: string) => triStateOf(counts.get(tagId) ?? 0, total);

    // Tags currently on at least one selected asset, in the store's alphabetical
    // order. These render as tri-state badges.
    const applied = $derived(assetLibrary.tags.filter((t) => (counts.get(t.id) ?? 0) > 0));

    // The dropdown: existing tags matching the query that aren't already on EVERY
    // asset (adding those is a no-op). Case-insensitive, capped so a big library
    // doesn't render a thousand rows.
    const suggestions = $derived.by(() => {
        const q = query.trim().toLowerCase();
        return assetLibrary.tags
            .filter((t) => stateOf(t.id) !== "all")
            .filter((t) => (q ? t.name.toLowerCase().includes(q) : true))
            .slice(0, 8);
    });

    // Offer "create" only when the typed name matches nothing exactly — so typing
    // an existing tag's full name assigns it rather than trying to duplicate.
    const exactExists = $derived(
        assetLibrary.tags.some((t) => t.name.toLowerCase() === query.trim().toLowerCase()),
    );
    const canCreate = $derived(query.trim().length > 0 && !exactExists);

    /**
     * Toggle a tag across the whole selection. Always an add/remove DELTA: a
     * partial tag fills (applies to all), a full one clears. Same semantics as
     * folder membership, so single-asset editing collapses to a plain toggle.
     */
    async function toggle(tagId: string) {
        if (busy) return;
        busy = true;
        try {
            if (stateOf(tagId) === "all") await assetLibrary.unassignTag(tagId, assetIds);
            else await assetLibrary.assignTag(tagId, assetIds);
            await refreshCounts(assetIds);
        } catch (e) {
            toast.error(typeof e === "string" ? e : "Failed to update tags.");
        } finally {
            busy = false;
        }
    }

    async function addExisting(tag: Tag) {
        query = "";
        if (busy) return;
        busy = true;
        try {
            await assetLibrary.assignTag(tag.id, assetIds);
            await refreshCounts(assetIds);
        } catch (e) {
            toast.error(typeof e === "string" ? e : "Failed to add tag.");
        } finally {
            busy = false;
        }
    }

    /** Create-on-the-fly: find-or-create by name, then assign to all selected. */
    async function createAndAdd() {
        const name = query.trim();
        if (!name || busy) return;
        query = "";
        busy = true;
        try {
            const id = await assetLibrary.ensureTag(name);
            await assetLibrary.assignTag(id, assetIds);
            await refreshCounts(assetIds);
        } catch (e) {
            toast.error(typeof e === "string" ? e : "Failed to create tag.");
        } finally {
            busy = false;
        }
    }

    /** Enter takes the first suggestion, else creates. Escape closes the list. */
    function onKey(e: KeyboardEvent) {
        if (e.key === "Enter") {
            e.preventDefault();
            if (suggestions.length > 0) addExisting(suggestions[0]);
            else if (canCreate) createAndAdd();
        } else if (e.key === "Escape") {
            open = false;
            (e.currentTarget as HTMLInputElement).blur();
        }
    }

    const swatch = (t: Tag) => t.color ?? "#9ca3af";
</script>

<div>
    <span class={legendClass}>Tags</span>

    {#if loading}
        <p class="text-xs text-neutral-600">Reading tags…</p>
    {:else}
        {#if applied.length > 0}
            <div class="mb-1.5 flex flex-wrap gap-1">
                {#each applied as tag (tag.id)}
                    {@const state = stateOf(tag.id)}
                    <button
                        type="button"
                        onclick={() => toggle(tag.id)}
                        disabled={busy}
                        title={state === "some"
                            ? `${counts.get(tag.id)} of ${total} — click to apply to all`
                            : "Click to remove"}
                        class="flex items-center gap-1 rounded-full border px-2 py-0.5 text-xs
                               transition-colors disabled:opacity-50
                               {state === 'all'
                            ? 'border-transparent text-neutral-900'
                            : 'border-dashed border-neutral-600 text-neutral-400'}"
                        style={state === "all" ? `background-color: ${swatch(tag)}` : ""}
                    >
                        <span
                            class="h-2 w-2 shrink-0 rounded-full"
                            style="background-color: {swatch(tag)}"
                        ></span>
                        {tag.name}
                        <!-- 'all' shows ×, 'some' shows the partial glyph, so the
                             two states never look interchangeable. -->
                        <span aria-hidden="true">{state === "all" ? "×" : "–"}</span>
                    </button>
                {/each}
            </div>
        {/if}

        <div class="relative">
            <input
                bind:this={input}
                type="text"
                bind:value={query}
                onfocus={() => (open = true)}
                onblur={() => setTimeout(() => (open = false), 120)}
                onkeydown={onKey}
                disabled={busy}
                spellcheck="false"
                placeholder="Add a tag…"
                class="w-full rounded border border-neutral-700 bg-neutral-900 px-2 py-1 text-xs
                       text-neutral-200 placeholder:text-neutral-600 focus:border-neutral-500
                       focus:outline-none disabled:opacity-50"
            />

            {#if open && (suggestions.length > 0 || canCreate)}
                <div
                    class="absolute z-10 mt-1 w-full overflow-hidden rounded border border-neutral-700
                           bg-neutral-900 shadow-xl"
                >
                    {#each suggestions as tag (tag.id)}
                        <button
                            type="button"
                            onclick={() => addExisting(tag)}
                            class="flex w-full items-center gap-2 px-2 py-1 text-left text-xs
                                   text-neutral-300 hover:bg-neutral-800"
                        >
                            <span
                                class="h-2 w-2 shrink-0 rounded-full"
                                style="background-color: {swatch(tag)}"
                            ></span>
                            <span class="truncate">{tag.name}</span>
                            <span class="ml-auto text-[10px] text-neutral-600">{tag.usage}</span>
                            {#if stateOf(tag.id) === "some"}
                                <span class="text-[10px] text-amber-600">partial</span>
                            {/if}
                        </button>
                    {/each}
                    {#if canCreate}
                        <button
                            type="button"
                            onclick={createAndAdd}
                            class="flex w-full items-center gap-1 border-t border-neutral-800 px-2 py-1
                                   text-left text-xs text-blue-400 hover:bg-neutral-800"
                        >
                            Create <span class="font-medium">"{query.trim()}"</span>
                        </button>
                    {/if}
                </div>
            {/if}
        </div>
    {/if}
</div>
