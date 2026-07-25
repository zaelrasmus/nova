<script lang="ts">
    import { untrack } from "svelte";
    import {
        assetLibrary,
        allScopes,
        type SearchScopes,
        type TextSearch,
    } from "$lib/assets.svelte";

    // ── State ─────────────────────────────────────────────────────────────────
    let query = $state("");
    let scopes = $state<SearchScopes>(allScopes());
    let input = $state<HTMLInputElement | null>(null);
    let focused = $state(false);
    let scopesOpen = $state(false);
    let root = $state<HTMLDivElement | null>(null);

    // Recent searches — last 5, newest first, deduped, persisted so they survive
    // a reload (FIFO history from the spec).
    const RECENT_KEY = "nova.search.recent";
    const RECENT_MAX = 5;
    let recent = $state<string[]>(load());

    function load(): string[] {
        try {
            const raw = localStorage.getItem(RECENT_KEY);
            return raw ? (JSON.parse(raw) as string[]).slice(0, RECENT_MAX) : [];
        } catch {
            return [];
        }
    }
    function remember(term: string) {
        const t = term.trim();
        if (!t) return;
        recent = [t, ...recent.filter((r) => r.toLowerCase() !== t.toLowerCase())].slice(
            0,
            RECENT_MAX,
        );
        try {
            localStorage.setItem(RECENT_KEY, JSON.stringify(recent));
        } catch {
            /* private mode — history just won't persist */
        }
    }

    // ── Scope helpers ─────────────────────────────────────────────────────────
    const SCOPE_LABELS: { key: keyof SearchScopes; label: string }[] = [
        { key: "name", label: "Name" },
        { key: "extension", label: "Extension" },
        { key: "note", label: "Note" },
        { key: "url", label: "URL" },
        { key: "folderName", label: "Folder name" },
        { key: "folderNote", label: "Folder description" },
        { key: "tags", label: "Tags" },
    ];

    /** Trigram floor — the FTS backend can't match a term shorter than this. */
    const MIN_FTS = 3;

    const activeScopes = $derived(SCOPE_LABELS.filter((s) => scopes[s.key]));
    const allActive = $derived(activeScopes.length === SCOPE_LABELS.length);
    // Name-only is the instant path: filtered in the frontend, no backend query.
    const nameOnly = $derived(scopes.name && activeScopes.length === 1);

    function toggleScope(key: keyof SearchScopes) {
        // Never let the user turn off every scope — that searches nothing.
        const next = { ...scopes, [key]: !scopes[key] };
        if (!SCOPE_LABELS.some((s) => next[s.key])) return;
        scopes = next;
        run(); // a scope change re-runs immediately, no debounce
    }

    // ── Running the search ────────────────────────────────────────────────────
    // Route to the instant frontend name-filter, or the backend FTS. Always
    // clear the OTHER path so the two never both apply.
    //
    // Frontend when: only Name is active, OR the query is shorter than the
    // trigram floor. The FTS backend can't match under 3 characters — dropping
    // such a query would silently show everything, which is worse than a
    // substring name filter that actually narrows. So a short query filters by
    // name instantly; once it reaches 3 chars it graduates to the full FTS.
    function run() {
        const q = query.trim();
        if (!q) {
            assetLibrary.setNameFilter(null);
            assetLibrary.setSearch(null);
            return;
        }
        if (nameOnly || q.length < MIN_FTS) {
            assetLibrary.setSearch(null);
            assetLibrary.setNameFilter(q);
        } else {
            assetLibrary.setNameFilter(null);
            const text: TextSearch = { query: q, scopes: $state.snapshot(scopes) };
            assetLibrary.setSearch(text);
        }
    }

    // Debounce keystrokes; a trigram query is cheap but re-streaming per letter
    // still wastes work, and it keeps the instant path from thrashing too.
    let timer: ReturnType<typeof setTimeout>;
    function onInput() {
        clearTimeout(timer);
        timer = setTimeout(run, 180);
    }

    function clearAll() {
        clearTimeout(timer);
        query = "";
        assetLibrary.setNameFilter(null);
        assetLibrary.setSearch(null);
        input?.focus();
    }

    function apply(term: string) {
        query = term;
        run();
        remember(term);
        scopesOpen = false;
    }

    function onKey(e: KeyboardEvent) {
        if (e.key === "Enter") {
            clearTimeout(timer);
            run();
            if (query.trim()) remember(query);
        } else if (e.key === "Escape") {
            if (query) clearAll();
            else input?.blur();
        }
    }

    // ── Typeahead: matching tags and folders ──────────────────────────────────
    // Live suggestions of ENTITIES, distinct from executing the free-text search:
    // clicking one drops its name into the query.
    const SUGGEST_MAX = 6;
    const suggestions = $derived.by(() => {
        const q = query.trim().toLowerCase();
        if (q.length < 1) return [] as { kind: "tag" | "folder"; label: string }[];
        const tags = assetLibrary.tags
            .filter((t) => t.name.toLowerCase().includes(q))
            .slice(0, SUGGEST_MAX)
            .map((t) => ({ kind: "tag" as const, label: t.name }));
        const folders = assetLibrary.folders
            .filter((f) => f.name.toLowerCase().includes(q))
            .slice(0, SUGGEST_MAX)
            .map((f) => ({ kind: "folder" as const, label: f.name }));
        return [...tags, ...folders].slice(0, SUGGEST_MAX);
    });

    const searching = $derived(query.trim().length > 0);
    const showPanel = $derived(
        focused && (searching ? suggestions.length > 0 : recent.length > 0),
    );

    // Close the scope popover on an outside click.
    $effect(() => {
        if (!scopesOpen) return;
        const onDown = (e: MouseEvent) => {
            if (root && !root.contains(e.target as Node)) scopesOpen = false;
        };
        window.addEventListener("mousedown", onDown, true);
        return () => window.removeEventListener("mousedown", onDown, true);
    });

    // The store's search is session state; if it's cleared elsewhere (library
    // switch, or the grid's "Clear" button), empty the box to match. Guarded on
    // `!focused` so it never wipes what the user is actively typing before the
    // debounce has applied it.
    $effect(() => {
        const active = assetLibrary.filters.text !== null || assetLibrary.nameFiltering;
        if (!active && !focused) untrack(() => (query = ""));
    });
</script>

<div bind:this={root} class="relative flex items-center gap-2 px-4 py-2">
    <!-- Input -->
    <div class="relative flex-1">
        <span class="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-neutral-400"
            >🔍</span
        >
        <input
            bind:this={input}
            bind:value={query}
            oninput={onInput}
            onkeydown={onKey}
            onfocus={() => (focused = true)}
            onblur={() => setTimeout(() => (focused = false), 120)}
            spellcheck="false"
            placeholder="Search — name, tags, notes…  ( - excludes, &quot;quotes&quot; for exact )"
            class="w-full rounded-md border border-neutral-300 bg-white py-1.5 pl-8 pr-8 text-sm
                   text-neutral-800 placeholder:text-neutral-400 focus:border-blue-400 focus:outline-none"
        />
        {#if query}
            <button
                type="button"
                onclick={clearAll}
                title="Clear search"
                class="absolute right-2 top-1/2 -translate-y-1/2 text-neutral-400 hover:text-neutral-700"
                >✕</button
            >
        {/if}

        <!-- Recent / typeahead panel -->
        {#if showPanel}
            <div
                class="absolute z-30 mt-1 w-full overflow-hidden rounded-md border border-neutral-200
                       bg-white shadow-lg"
            >
                {#if searching}
                    {#each suggestions as s (s.kind + s.label)}
                        <button
                            type="button"
                            onclick={() => apply(s.label)}
                            class="flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm
                                   text-neutral-700 hover:bg-neutral-100"
                        >
                            <span class="text-xs">{s.kind === "tag" ? "🏷" : "📁"}</span>
                            <span class="truncate">{s.label}</span>
                        </button>
                    {/each}
                {:else}
                    <div class="px-3 pb-0.5 pt-1.5 text-[10px] font-medium uppercase text-neutral-400">
                        Recent
                    </div>
                    {#each recent as term (term)}
                        <button
                            type="button"
                            onclick={() => apply(term)}
                            class="flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm
                                   text-neutral-700 hover:bg-neutral-100"
                        >
                            <span class="text-xs text-neutral-400">🕑</span>
                            <span class="truncate">{term}</span>
                        </button>
                    {/each}
                {/if}
            </div>
        {/if}
    </div>

    <!-- Scope selector -->
    <div class="relative">
        <button
            type="button"
            onclick={() => (scopesOpen = !scopesOpen)}
            title="Choose where to search"
            class="flex items-center gap-1 rounded-md border border-neutral-300 bg-white px-2 py-1.5
                   text-xs text-neutral-600 hover:bg-neutral-50"
        >
            Scope
            <span class="rounded bg-neutral-200 px-1 text-[10px] text-neutral-600">
                {allActive ? "All" : activeScopes.length}
            </span>
            <span class="text-[9px]">▼</span>
        </button>

        {#if scopesOpen}
            <div
                class="absolute right-0 z-30 mt-1 w-44 overflow-hidden rounded-md border border-neutral-200
                       bg-white py-1 shadow-lg"
            >
                {#each SCOPE_LABELS as s (s.key)}
                    <label
                        class="flex cursor-pointer items-center gap-2 px-3 py-1 text-sm text-neutral-700
                               hover:bg-neutral-100"
                    >
                        <input
                            type="checkbox"
                            checked={scopes[s.key]}
                            onchange={() => toggleScope(s.key)}
                            class="accent-blue-600"
                        />
                        {s.label}
                    </label>
                {/each}
            </div>
        {/if}
    </div>

    <!-- Active-scope chips (only when narrowed, so the default stays quiet) -->
    {#if !allActive}
        <div class="flex flex-wrap items-center gap-1">
            {#each activeScopes as s (s.key)}
                <button
                    type="button"
                    onclick={() => toggleScope(s.key)}
                    title="Remove {s.label} from the search"
                    class="flex items-center gap-1 rounded-full bg-blue-100 px-2 py-0.5 text-[11px]
                           text-blue-700 hover:bg-blue-200"
                >
                    {s.label}
                    <span aria-hidden="true">✕</span>
                </button>
            {/each}
        </div>
    {/if}
</div>
