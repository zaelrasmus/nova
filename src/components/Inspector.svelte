<script lang="ts">
    import { untrack } from "svelte";
    import { toast } from "svelte-sonner";
    import { openUrl } from "@tauri-apps/plugin-opener";
    import { assetLibrary, describeShape, filenameStem } from "$lib/assets.svelte";
    import type {
        AssetPatch,
        FolderPatch,
        FolderStats,
        SelectionSummary,
    } from "$lib/assets.svelte";
    import { selection } from "$lib/selection.svelte";
    import { formatAspectRatio, formatBytes, formatTimestamp } from "$lib/format";
    import { Pin, PinOff } from "@lucide/svelte";
    import PaletteSection from "./PaletteSection.svelte";
    import FolderMembership from "./FolderMembership.svelte";
    import FolderAutoTags from "./FolderAutoTags.svelte";
    import TagEditor from "./TagEditor.svelte";
    import PinSwatches from "./PinSwatches.svelte";

    // The inspector renders the selection; it never owns it. Every mode below is
    // a branch of one union, so "3 assets and a folder" can't be reached.
    const current = $derived(selection.current);

    const singleId = $derived(
        current.kind === "assets" && current.ids.length === 1 ? current.ids[0] : null,
    );
    const asset = $derived(singleId ? assetLibrary.heavy.get(singleId) : undefined);

    // Looked up rather than stored: if the folder is deleted while selected, this
    // goes undefined and the panel falls back to the empty state on its own.
    const folder = $derived(
        current.kind === "folder"
            ? assetLibrary.folders.find((f) => f.id === current.id)
            : undefined,
    );

    // The selected row is normally on screen and already hydrated, but a
    // selection outlives its window — scroll far enough and the LRU evicts it.
    $effect(() => {
        if (singleId && !assetLibrary.heavy.has(singleId)) assetLibrary.ensure([singleId]);
    });

    // ── Editing ──────────────────────────────────────────────────────────────
    //
    // Drafts are local and saved on a debounce, so typing never waits on IPC. The
    // whole design turns on one hazard: type a new name, click a different asset
    // before the timer fires, and a naive implementation writes your text to the
    // WRONG row. So every queued edit captures the key it belongs to, and
    // switching targets flushes the old one before reseeding.

    const AUTOSAVE_MS = 500;

    /**
     * Which row the drafts belong to, as "asset:<id>" or "folder:<id>".
     * Deliberately NOT `$state`: it's bookkeeping for the flush, and making it
     * reactive would re-trigger the seeding effect from inside its own body.
     */
    let editingKey: string | null = null;

    let draftName = $state("");
    let draftNotes = $state("");
    let draftUrl = $state("");

    let pending: Record<string, string> = {};
    let pendingKey: string | null = null;
    let timer: ReturnType<typeof setTimeout> | undefined;

    const targetKey = (): string | null =>
        asset ? `asset:${asset.id}` : folder ? `folder:${folder.id}` : null;

    // Reseed when the inspected row changes — and only then. A save patches the
    // cache, which re-runs this effect with the same key; the early return is what
    // stops that from wiping out whatever is currently being typed.
    $effect(() => {
        const key = targetKey();
        const a = asset;
        const f = folder;
        untrack(() => {
            if (key === editingKey) return;
            flush(editingKey); // commit whatever the row we're leaving had pending
            editingKey = key;
            draftName = a ? filenameStem(a.filename, a.extension) : (f?.name ?? "");
            draftNotes = a ? (a.notes ?? "") : (f?.notes ?? "");
            draftUrl = a?.source_url ?? "";
        });
    });

    // Last line of defence: the panel going away must not swallow a pending edit.
    $effect(() => () => void flush(pendingKey));

    function queue(field: keyof AssetPatch | keyof FolderPatch, value: string) {
        const key = editingKey;
        if (!key) return;
        // A different row than the last queued edit means that one was already
        // flushed by the effect above; don't carry its fields over.
        if (pendingKey !== key) {
            pending = {};
            pendingKey = key;
        }
        pending[field] = value;
        clearTimeout(timer);
        timer = setTimeout(() => flush(key), AUTOSAVE_MS); // `key` captured, not read later
    }

    async function flush(key: string | null): Promise<void> {
        clearTimeout(timer);
        if (!key || key !== pendingKey) return; // nothing pending, or not for this row
        const patch = pending;
        pending = {};
        pendingKey = null;
        if (Object.keys(patch).length === 0) return;

        const cut = key.indexOf(":");
        const [kind, id] = [key.slice(0, cut), key.slice(cut + 1)];
        try {
            if (kind === "asset") await assetLibrary.updateAsset(id, patch as AssetPatch);
            else await assetLibrary.updateFolder(id, patch as FolderPatch);
        } catch (e) {
            toast.error(typeof e === "string" ? e : "Failed to save changes.");
        }
    }

    /** Names can't be blank. Skip the write rather than round-tripping an error. */
    function editName(value: string) {
        draftName = value;
        if (value.trim()) queue(asset ? "stem" : "name", value);
    }

    /** Blur restores a name the user emptied but never committed. */
    function commitName() {
        if (!draftName.trim()) {
            draftName = asset ? filenameStem(asset.filename, asset.extension) : (folder?.name ?? "");
            return;
        }
        flush(editingKey);
    }

    // Stored permissively, opened strictly: this is user-editable text being
    // handed to the OS shell, and file:// or a custom protocol handler is a real
    // vector. Anything that isn't http(s) stays plain text.
    const openable = $derived(/^https?:\/\//i.test(draftUrl.trim()));
    async function openSource() {
        const url = draftUrl.trim();
        if (!/^https?:\/\//i.test(url)) return;
        try {
            await openUrl(url);
        } catch {
            toast.error("Couldn't open that link.");
        }
    }

    // ── Folder aggregates ────────────────────────────────────────────────────
    // Recomputed when the selected folder changes AND when the folder list is
    // reloaded — which happens after an import and after any folder CRUD, so the
    // numbers refresh at the moments they'd otherwise go stale. Adding or
    // removing individual assets still needs a reselect; that's the honest limit
    // of not caching this anywhere.
    let stats = $state<FolderStats | null>(null);

    $effect(() => {
        const id = folder?.id;
        void assetLibrary.folders;
        if (!id) {
            stats = null;
            return;
        }
        let cancelled = false;
        assetLibrary
            .fetchFolderStats(id)
            .then((s) => {
                if (!cancelled) stats = s;
            })
            .catch(() => {
                if (!cancelled) stats = null;
            });
        return () => {
            cancelled = true; // a newer selection superseded this fetch
        };
    });

    const TYPE_LABELS: Record<string, string> = {
        image: "Image",
        video: "Video",
        audio: "Audio",
        unknown: "File",
    };

    // Exact totals from the DB. Summing the heavy cache would have been free but
    // wrong: that cache holds a bounded window, so any selection larger than the
    // screen would quietly under-report.
    let summary = $state<SelectionSummary | null>(null);

    $effect(() => {
        const ids = current.kind === "assets" && current.ids.length > 1 ? current.ids : null;
        if (!ids) {
            summary = null;
            return;
        }
        let cancelled = false;
        summary = null;
        assetLibrary
            .fetchSelectionSummary(ids)
            .then((s) => {
                if (!cancelled) summary = s;
            })
            .catch(() => {
                if (!cancelled) summary = null;
            });
        return () => {
            cancelled = true;
        };
    });

    const inputClass =
        "w-full rounded border border-neutral-700 bg-neutral-900 px-2 py-1 text-neutral-200 " +
        "placeholder:text-neutral-600 focus:border-neutral-500 focus:outline-none";
    const legendClass = "mb-1 block text-[10px] font-medium uppercase tracking-wide text-neutral-500";
</script>

{#snippet row(label: string, value: string)}
    <div class="flex items-baseline justify-between gap-3 py-1">
        <span class="shrink-0 text-neutral-500">{label}</span>
        <span class="truncate text-right text-neutral-300" title={value}>{value}</span>
    </div>
{/snippet}

{#snippet notesField()}
    <label class="block">
        <span class={legendClass}>Notes</span>
        <textarea
            rows="3"
            value={draftNotes}
            oninput={(e) => {
                draftNotes = e.currentTarget.value;
                queue("notes", draftNotes);
            }}
            onblur={() => flush(editingKey)}
            placeholder="Add a note…"
            class="resize-y {inputClass}"
        ></textarea>
    </label>
{/snippet}

<aside
    class="flex h-full flex-col gap-3 overflow-y-auto rounded-lg border border-neutral-800
           bg-neutral-900/40 p-3 text-sm"
    aria-label="Inspector"
>
    {#if current.kind === "assets" && current.ids.length === 1}
        {#if asset}
            <div>
                <label class="block">
                    <span class={legendClass}>Name</span>
                    <div class="flex items-center gap-1">
                        <input
                            type="text"
                            value={draftName}
                            oninput={(e) => editName(e.currentTarget.value)}
                            onblur={commitName}
                            spellcheck="false"
                            class={inputClass}
                        />
                        <!-- The extension is shown, never edited: it describes the
                             actual bytes, not the user's label for them. -->
                        {#if asset.extension}
                            <span class="shrink-0 text-xs text-neutral-500"
                                >.{asset.extension}</span
                            >
                        {/if}
                    </div>
                </label>
                <p class="mt-1 text-[10px] text-neutral-600">
                    {TYPE_LABELS[asset.asset_type] ?? "File"} · renaming affects this library only
                </p>
            </div>

            {@render notesField()}

            <label class="block">
                <span class={legendClass}>Source URL</span>
                <div class="flex items-center gap-1">
                    <input
                        type="url"
                        value={draftUrl}
                        oninput={(e) => {
                            draftUrl = e.currentTarget.value;
                            queue("source_url", draftUrl);
                        }}
                        onblur={() => flush(editingKey)}
                        spellcheck="false"
                        placeholder="https://…"
                        class={inputClass}
                    />
                    <button
                        type="button"
                        onclick={openSource}
                        disabled={!openable}
                        title={openable ? "Open in browser" : "Only http(s) links can be opened"}
                        class="shrink-0 rounded px-1.5 py-1 text-neutral-500
                               enabled:hover:bg-neutral-800 enabled:hover:text-neutral-200
                               disabled:cursor-not-allowed disabled:text-neutral-700">↗</button
                    >
                </div>
            </label>

            <div class="h-px bg-neutral-800"></div>

            <TagEditor assetIds={[asset.id]} {legendClass} />

            <div class="h-px bg-neutral-800"></div>

            <!-- Same control as bulk mode, just with a selection of one — so the
                 tri-state collapses to an ordinary checkbox on its own. -->
            <FolderMembership assetIds={[asset.id]} {legendClass} />

            <div class="h-px bg-neutral-800"></div>

            <!-- Images only: nothing else produces a palette, and an empty strip
                 on a video would read as a bug rather than as "not applicable". -->
            {#if asset.asset_type === "image"}
                <PaletteSection assetId={asset.id} {legendClass} />
                <div class="h-px bg-neutral-800"></div>
            {/if}

            <div class="flex flex-col text-xs">
                {@render row("Dimensions", `${asset.width} × ${asset.height}`)}
                {@render row(
                    "Aspect ratio",
                    `${formatAspectRatio(asset.width, asset.height)} · ${describeShape(asset.width, asset.height)}`,
                )}
                {@render row("Size", formatBytes(asset.file_size))}
                {@render row("Format", asset.extension.toUpperCase() || "—")}
            </div>

            <div class="h-px bg-neutral-800"></div>

            <div class="flex flex-col text-xs">
                {@render row("Added", formatTimestamp(asset.imported_date))}
                <!-- "Created" is the filesystem timestamp, not EXIF capture time.
                     Labelled plainly on purpose: when EXIF lands it becomes its
                     own field rather than quietly redefining this one. -->
                {@render row("Created", formatTimestamp(asset.creation_date))}
                {@render row("Modified", formatTimestamp(asset.modified_date))}
            </div>
        {:else}
            <p class="text-xs text-neutral-500">Loading details…</p>
        {/if}
    {:else if current.kind === "assets"}
        <!-- Name, notes and source URL are absent rather than disabled: a greyed
             box showing nothing is a puzzle, and there's no safe answer to
             "append or replace?" for a field shared by 50 rows. -->
        <div>
            <h2 class="text-neutral-100">{current.ids.length} assets selected</h2>
            <p class="mt-0.5 text-xs text-neutral-500">
                {summary ? `${formatBytes(summary.total_bytes)} total` : "Measuring…"}
            </p>
        </div>

        <TagEditor assetIds={current.ids} {legendClass} />
        <FolderMembership assetIds={current.ids} {legendClass} />
    {:else if folder}
        <label class="block">
            <span class={legendClass}>Folder name</span>
            <input
                type="text"
                value={draftName}
                oninput={(e) => editName(e.currentTarget.value)}
                onblur={commitName}
                spellcheck="false"
                class={inputClass}
            />
        </label>

        {@render notesField()}

        <div class="h-px bg-neutral-800"></div>

        <!-- Auto-tags sit above pinning because they change what the folder DOES
             to assets, which is a bigger claim than how it looks in the sidebar. -->
        <FolderAutoTags folderId={folder.id} {legendClass} />

        <div class="h-px bg-neutral-800"></div>

        <!-- Pinning lives here as well as in the right-click menu, because the
             inspector is where you land after clicking a folder — and "put this
             in my sidebar" is a property of the folder you're looking at, not a
             command you should have to remember to right-click for. -->
        <div class="flex flex-col gap-2">
            <span class={legendClass}>Sidebar</span>
            <button
                type="button"
                onclick={() =>
                    assetLibrary.setPinned("folder", folder.id, folder.pin_position === null)}
                class="flex items-center gap-2 self-start rounded border border-neutral-800 px-2
                       py-1 text-xs transition-colors hover:bg-neutral-800
                       {folder.pin_position !== null
                    ? 'text-neutral-200'
                    : 'text-neutral-400'}"
            >
                {#if folder.pin_position !== null}
                    <PinOff class="h-3.5 w-3.5" /> Unpin
                {:else}
                    <Pin class="h-3.5 w-3.5" /> Pin to sidebar
                {/if}
            </button>

            {#if folder.pin_position !== null}
                <PinSwatches kind="folder" id={folder.id} />
            {/if}
        </div>

        <div class="h-px bg-neutral-800"></div>

        <div class="flex flex-col text-xs">
            {#if stats}
                <!-- Counted across the whole subtree, deduplicated: an asset in
                     both a parent and its child is one item, not two. -->
                {@render row("Items", stats.asset_count.toLocaleString())}
                {@render row("Size", formatBytes(stats.total_bytes))}
                {#if stats.descendant_folders > 0}
                    {@render row("Subfolders", stats.descendant_folders.toLocaleString())}
                {/if}
            {:else}
                {@render row("Items", "…")}
            {/if}
            {@render row("Created", formatTimestamp(folder.created_at))}
        </div>
    {:else}
        <!-- An empty state, not a blank panel: a void reads as a bug. This is
             also what "All" and "Uncategorized" resolve to — they're places, and
             a place has nothing to inspect. -->
        <p class="m-auto max-w-[22ch] text-center text-xs text-neutral-600">
            Select an asset or a folder to see its details.
        </p>
    {/if}
</aside>
