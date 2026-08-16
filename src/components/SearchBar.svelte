<!--
  SearchBar — the grid's text search, with per-column scope toggles and a recent
  history.

  Drives the HYBRID, which is the thing to understand here. A short, name-only
  query is answered in the frontend by filtering the already-loaded manifest
  (`setNameFilter`) — instant, no round trip. Anything else goes to the backend
  as `FilterSet.text` and is compiled into an FTS5 MATCH.

  That split isn't just an optimisation: the trigram tokenizer needs >= 3
  characters, so short terms have no index to use anyway.

  Search is SUBSTRING matching, not typo tolerance — worth remembering before
  writing UI copy that promises otherwise.
-->
<script lang="ts">
    import { untrack } from "svelte";
    import { toast } from "svelte-sonner";
    import {
        Search,
        X,
        Tag,
        Folder,
        Clock,
        SlidersHorizontal,
        ChevronDown,
        TriangleAlert,
    } from "@lucide/svelte";
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

    // Recovery for a drifted index. Rebuilding is O(library) and the notice is
    // the only thing that will clear it, so the button reports its own progress
    // rather than leaving the user wondering whether the click registered.
    let rebuilding = $state(false);
    async function rebuildIndex() {
        rebuilding = true;
        try {
            await assetLibrary.rebuildSearchIndex();
            toast.success("Search index rebuilt.");
        } catch (e) {
            toast.error(typeof e === "string" ? e : "Couldn't rebuild the search index.");
        } finally {
            rebuilding = false;
        }
    }

    // The store's search is session state; if it's cleared elsewhere (library
    // switch, or the grid's "Clear" button), empty the box to match. Guarded on
    // `!focused` so it never wipes what the user is actively typing before the
    // debounce has applied it.
    $effect(() => {
        const active = assetLibrary.filters.text !== null || assetLibrary.nameFiltering;
        if (!active && !focused) untrack(() => (query = ""));
    });
</script>

<!--
  Search is answering from a stale index.

  Shown HERE, attached to the control it invalidates, rather than as a toast: the
  condition holds until someone rebuilds, so a message that fades after a few
  seconds would be the wrong shape — and it would almost certainly appear while
  the user was looking somewhere else, since the reindex that failed was a side
  effect of tagging or importing, not of searching.

  It is also the only non-fatal failure in the app whose consequence is
  invisible. A missing thumbnail announces itself; a search result that is
  quietly wrong is indistinguishable from an asset you misremembered.
-->
{#if assetLibrary.searchDegraded}
    <div
        role="status"
        class="mx-4 mb-1 mt-2 flex items-center gap-2 rounded-md border border-amber-500/40
               bg-amber-500/10 px-3 py-1.5 text-xs text-amber-200"
    >
        <TriangleAlert class="h-3.5 w-3.5 shrink-0" />
        <span class="flex-1">
            Search results may be incomplete — the index didn't finish updating.
        </span>
        <button
            type="button"
            onclick={rebuildIndex}
            disabled={rebuilding}
            class="rounded border border-amber-500/50 px-2 py-0.5 font-medium
                   hover:bg-amber-500/20 disabled:opacity-50"
        >
            {rebuilding ? "Rebuilding…" : "Rebuild"}
        </button>
    </div>
{/if}

<!-- Lives in the grid pane header, a 44px strip shared with the scope label and
     the view controls, so it is sized to sit IN that row rather than to fill it:
     h-7 to match the icon buttons either side, and free to shrink when the panes
     are wide (see the header's min-w-0 / shrink rules in +page.svelte). -->
<div bind:this={root} class="relative flex min-w-0 items-center gap-1.5">
    <!-- Input -->
    <div class="relative min-w-0 flex-1">
        <Search
            class="pointer-events-none absolute left-2 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-neutral-500"
        />
        <input
            bind:this={input}
            bind:value={query}
            oninput={onInput}
            onkeydown={onKey}
            onfocus={() => (focused = true)}
            onblur={() => setTimeout(() => (focused = false), 120)}
            spellcheck="false"
            placeholder="Search…"
            title={'Search name, tags and notes.  " " for exact, - to exclude'}
            class="h-7 w-full rounded-md border border-neutral-800 bg-neutral-900 pl-7 pr-7 text-xs
                   text-neutral-200 placeholder:text-neutral-500 transition-colors
                   hover:border-neutral-700 focus:border-neutral-600 focus:outline-none"
        />
        {#if query}
            <button
                type="button"
                onclick={clearAll}
                title="Clear search"
                aria-label="Clear search"
                class="absolute right-1.5 top-1/2 grid h-4 w-4 -translate-y-1/2 place-items-center
                       rounded text-neutral-500 transition-colors hover:bg-neutral-800
                       hover:text-neutral-200"
            >
                <X class="h-3 w-3" />
            </button>
        {/if}

        <!-- Recent / typeahead panel -->
        {#if showPanel}
            <div
                class="absolute z-30 mt-1 w-full overflow-hidden rounded-md border border-neutral-800
                       bg-neutral-900 shadow-lg"
            >
                {#if searching}
                    {#each suggestions as s (s.kind + s.label)}
                        <button
                            type="button"
                            onclick={() => apply(s.label)}
                            class="flex w-full items-center gap-2 px-3 py-1.5 text-left text-xs
                                   text-neutral-200 hover:bg-neutral-800"
                        >
                            {#if s.kind === "tag"}
                                <Tag class="h-3 w-3 shrink-0 text-neutral-500" />
                            {:else}
                                <Folder class="h-3 w-3 shrink-0 text-neutral-500" />
                            {/if}
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
                            class="flex w-full items-center gap-2 px-3 py-1.5 text-left text-xs
                                   text-neutral-200 hover:bg-neutral-800"
                        >
                            <Clock class="h-3 w-3 shrink-0 text-neutral-500" />
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
            aria-expanded={scopesOpen}
            class="flex h-7 shrink-0 items-center gap-1 rounded-md border px-2 text-xs
                   transition-colors
                   {allActive
                ? 'border-neutral-800 bg-neutral-900 text-neutral-400 hover:border-neutral-700 hover:text-neutral-200'
                : 'border-blue-500/40 bg-blue-500/10 text-blue-300 hover:bg-blue-500/20'}"
        >
            <!-- The count is the point: a narrowed scope is the other way a
                 search can quietly return less than expected, so it reads as
                 active-blue rather than sitting silently at "3". -->
            <SlidersHorizontal class="h-3 w-3" />
            <span class="tabular-nums">{allActive ? "All" : activeScopes.length}</span>
            <ChevronDown class="h-3 w-3 opacity-60" />
        </button>

        {#if scopesOpen}
            <div
                class="absolute right-0 z-30 mt-1 w-44 overflow-hidden rounded-md border border-neutral-800
                       bg-neutral-900 py-1 shadow-lg"
            >
                {#each SCOPE_LABELS as s (s.key)}
                    <label
                        class="flex cursor-pointer items-center gap-2 px-3 py-1 text-sm text-neutral-200
                               hover:bg-neutral-800"
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
