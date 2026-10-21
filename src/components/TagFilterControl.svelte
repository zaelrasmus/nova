<!--
  TagFilterControl — the tag dimension of the FilterBar: include, exclude, and
  the "untagged" pseudo-selection, combined by any / all / equals.

  Filters carry tag IDS, not names, so renaming a tag can't silently change what
  a saved filter matches. "Untagged" is never a real tag row.
-->
<script lang="ts">
    import { ChevronDown, X } from "@lucide/svelte";
    import {
        assetLibrary,
        emptyTagFilter,
        type TagFilter,
        type TagMatchMode,
        type Tag,
    } from "$lib/assets.svelte";

    interface Props {
        fieldClass: string;
        labelClass: string;
    }
    let { fieldClass, labelClass }: Props = $props();

    const tf = $derived(assetLibrary.filters.tags);
    const activeCount = $derived(
        tf ? tf.include.length + tf.exclude.length + (tf.untagged ? 1 : 0) : 0,
    );

    let open = $state(false);
    let search = $state("");

    // Close on outside click / Escape while open. Registered only when open so it
    // isn't a standing global listener. Scoped to the whole control (root), not
    // just the panel, so a click on the trigger doesn't close-then-reopen — the
    // capture-phase handler would otherwise fire before the button's own toggle.
    let root = $state<HTMLDivElement | null>(null);
    $effect(() => {
        if (!open) return;
        const onDown = (e: MouseEvent) => {
            if (root && !root.contains(e.target as Node)) open = false;
        };
        const onKey = (e: KeyboardEvent) => {
            if (e.key === "Escape") open = false;
        };
        window.addEventListener("mousedown", onDown, true);
        window.addEventListener("keydown", onKey);
        return () => {
            window.removeEventListener("mousedown", onDown, true);
            window.removeEventListener("keydown", onKey);
        };
    });

    /** A mutable copy of the applied filter (or a fresh default) to edit from. */
    const draft = (): TagFilter =>
        tf
            ? { ...tf, include: [...tf.include], exclude: [...tf.exclude] }
            : emptyTagFilter();

    const stateOf = (id: string): "include" | "exclude" | "none" => {
        if (!tf) return "none";
        if (tf.include.includes(id)) return "include";
        if (tf.exclude.includes(id)) return "exclude";
        return "none";
    };

    const MODES: { value: TagMatchMode; label: string; title: string }[] = [
        { value: "any", label: "Any", title: "Assets with at least one selected tag" },
        { value: "all", label: "All", title: "Assets with every selected tag" },
        { value: "equals", label: "Exact", title: "Assets with exactly these tags, nothing else" },
    ];

    function setMode(mode: TagMatchMode) {
        const d = draft();
        d.mode = mode;
        // Exclusion is meaningless when the set is fully specified, so drop it
        // rather than keep an invisible-but-ignored constraint.
        if (mode === "equals") d.exclude = [];
        assetLibrary.setTagFilter(d);
    }

    /** Left-click: cycle a tag into the include set (and out of exclude). */
    function toggleInclude(id: string) {
        const d = draft();
        d.exclude = d.exclude.filter((x) => x !== id);
        d.include = d.include.includes(id)
            ? d.include.filter((x) => x !== id)
            : [...d.include, id];
        assetLibrary.setTagFilter(d);
    }

    /** Right-click: cycle a tag into the exclude set (and out of include). */
    function toggleExclude(id: string) {
        if (tf?.mode === "equals") return; // no exclusion in exact mode
        const d = draft();
        d.include = d.include.filter((x) => x !== id);
        d.exclude = d.exclude.includes(id)
            ? d.exclude.filter((x) => x !== id)
            : [...d.exclude, id];
        assetLibrary.setTagFilter(d);
    }

    function toggleUntagged() {
        const d = draft();
        d.untagged = !d.untagged;
        assetLibrary.setTagFilter(d);
    }

    function clear() {
        assetLibrary.setTagFilter(null);
        search = "";
    }

    const matched = $derived.by(() => {
        const q = search.trim().toLowerCase();
        return assetLibrary.tags.filter((t) => (q ? t.name.toLowerCase().includes(q) : true));
    });

    const swatch = (t: Tag) => t.color ?? "#9ca3af";
</script>

<div bind:this={root} class="relative flex items-center gap-1.5">
    <span class={labelClass}>Tags</span>
    <button
        type="button"
        onclick={() => (open = !open)}
        aria-expanded={open}
        class="{fieldClass} flex items-center gap-1
               {activeCount > 0 ? 'border-brand-line' : ''}"
    >
        {#if activeCount > 0}
            <span class="rounded-full bg-brand px-1.5 text-[10px] font-medium text-white">
                {activeCount}
            </span>
        {/if}
        <span>{activeCount > 0 ? tf?.mode.toUpperCase() : "Any"}</span>
        <ChevronDown class="h-3 w-3 text-neutral-500" aria-hidden="true" />
    </button>

    {#if activeCount > 0}
        <button
            type="button"
            onclick={clear}
            title="Clear tag filter"
            aria-label="Clear tag filter"
            class="grid h-5 w-5 place-items-center rounded text-neutral-500 hover:bg-wash-strong
                   hover:text-neutral-200"><X class="h-3 w-3" /></button
        >
    {/if}

    {#if open}
        <div
            class="nv-menu absolute left-0 top-full z-20 mt-1.5 w-64 p-2"
        >
            <!-- Mode: how the included tags combine. -->
            <div class="mb-2 flex gap-px rounded-lg border border-line bg-raised p-0.5">
                {#each MODES as m (m.value)}
                    <button
                        type="button"
                        onclick={() => setMode(m.value)}
                        title={m.title}
                        aria-pressed={(tf?.mode ?? "all") === m.value}
                        class="h-6 flex-1 rounded-md px-1.5 text-xs font-medium transition-colors
                            {(tf?.mode ?? 'all') === m.value
                            ? 'bg-raised-2 text-neutral-100 shadow-[0_1px_2px_rgb(0_0_0/0.45),inset_0_0_0_1px_var(--nv-line)]'
                            : 'text-neutral-500 hover:text-neutral-200'}"
                    >
                        {m.label}
                    </button>
                {/each}
            </div>

            <input
                type="text"
                bind:value={search}
                placeholder="Search tags…"
                spellcheck="false"
                class="mb-1.5 h-7 w-full rounded-md border border-line bg-raised px-2 text-xs
                       text-neutral-200 placeholder:text-neutral-500 focus:border-brand-line focus:outline-none"
            />

            <div class="max-h-56 overflow-y-auto">
                <!-- Untagged is a pseudo-row, not a tag: assets with none at all. -->
                <button
                    type="button"
                    onclick={toggleUntagged}
                    class="flex w-full items-center gap-2 rounded-md px-1.5 py-1 text-left text-xs
                           text-neutral-400 hover:bg-wash-strong"
                >
                    <span
                        class="flex h-3.5 w-3.5 items-center justify-center rounded-sm border text-[9px]
                               text-white {tf?.untagged
                            ? 'border-brand bg-brand'
                            : 'border-neutral-600'}"
                    >
                        {tf?.untagged ? "✓" : ""}
                    </span>
                    <span class="italic">Untagged</span>
                </button>

                {#if matched.length > 0}
                    <div class="nv-menu-sep"></div>
                {/if}

                {#each matched as tag (tag.id)}
                    {@const st = stateOf(tag.id)}
                    <button
                        type="button"
                        onclick={() => toggleInclude(tag.id)}
                        oncontextmenu={(e) => {
                            e.preventDefault();
                            toggleExclude(tag.id);
                        }}
                        title="Left-click include · right-click exclude"
                        class="flex w-full items-center gap-2 rounded-md px-1.5 py-1 text-left text-xs
                               hover:bg-wash-strong
                               {st === 'include'
                            ? 'text-brand-text'
                            : st === 'exclude'
                              ? 'text-red-400'
                              : 'text-neutral-300'}"
                    >
                        <span
                            class="flex h-3.5 w-3.5 items-center justify-center rounded-sm border text-[9px]
                                   {st === 'include'
                                ? 'border-brand bg-brand text-white'
                                : st === 'exclude'
                                  ? 'border-red-500 bg-red-500 text-white'
                                  : 'border-neutral-600'}"
                        >
                            {st === "include" ? "✓" : st === "exclude" ? "−" : ""}
                        </span>
                        <span
                            class="h-2 w-2 shrink-0 rounded-full"
                            style="background-color: {swatch(tag)}"
                        ></span>
                        <span class="truncate">{tag.name}</span>
                        <span class="ml-auto text-[10px] tabular-nums text-neutral-500">{tag.usage}</span>
                    </button>
                {/each}

                {#if assetLibrary.tags.length === 0}
                    <p class="px-1.5 py-2 text-xs text-neutral-500">No tags in this library yet.</p>
                {/if}
            </div>

            <div class="mt-1.5 border-t border-line pt-1.5 text-[10px] text-neutral-500">
                Left-click <span class="text-brand-text">include</span> · right-click
                <span class="text-red-400">exclude</span>{tf?.mode === "equals"
                    ? " (off in Exact)"
                    : ""}
            </div>
        </div>
    {/if}
</div>
