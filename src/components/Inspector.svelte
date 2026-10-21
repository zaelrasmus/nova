<!--
  Inspector — the right pane. Shows and edits whatever is selected.

  Four mutually exclusive modes, branched off the ONE union `selection.current`
  so "3 assets and a folder" is unrepresentable: nothing · one asset · many
  assets · one folder.

  It RENDERS the selection and never owns it (see selection.svelte.ts), so
  collapsing this panel can't destroy what the user picked.

  Text edits are local drafts saved on a debounce, so typing never waits on IPC.
  Bulk edits route through `run_steps`, which is what gives them undo — note
  they're currently silent (no toast), unlike the grid's bulk actions.
-->
<script lang="ts">
    import { untrack } from "svelte";
    import { toast } from "svelte-sonner";
    import { convertFileSrc } from "@tauri-apps/api/core";
    import { openUrl } from "@tauri-apps/plugin-opener";
    import { assetLibrary, describeShape, filenameStem, thumbHashUrl } from "$lib/assets.svelte";
    import type {
        AssetMetadata,
        AssetPatch,
        FolderPatch,
        FolderStats,
        SelectionSummary,
    } from "$lib/assets.svelte";
    import { selection } from "$lib/selection.svelte";
    import { viewer } from "$lib/viewer.svelte";
    import { formatAspectRatio, formatBytes, formatTimestamp, formatDuration } from "$lib/format";
    import { keepRemoteOffline, verifyRemoteAsset } from "$lib/remote";
    import {
        ChevronDown,
        Cloud,
        ExternalLink,
        File as FileIcon,
        Film,
        Image as ImageIcon,
        Maximize2,
        Music,
        Pin,
        PinOff,
    } from "@lucide/svelte";
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

    // ── Online-asset actions ────────────────────────────────────────────────
    // Both patch the heavy row in place on success, so the panel and the grid
    // badge update without a manifest reload.
    let busyRemote = $state<"download" | "verify" | null>(null);

    async function keepOffline(id: string) {
        if (busyRemote) return;
        busyRemote = "download";
        try {
            assetLibrary.heavy.set(id, await keepRemoteOffline(id));
            toast.success("Saved to your library");
        } catch (e) {
            toast.error(typeof e === "string" ? e : "Couldn't download that file.");
        } finally {
            busyRemote = null;
        }
    }

    async function recheck(id: string) {
        if (busyRemote) return;
        busyRemote = "verify";
        try {
            const state = await verifyRemoteAsset(id);
            const heavy = assetLibrary.heavy.get(id);
            if (heavy) assetLibrary.heavy.set(id, { ...heavy, remote_state: state });
            if (state === "ok") toast.success("The link is still reachable.");
            else toast.warning("That link could not be reached.");
        } catch (e) {
            toast.error(typeof e === "string" ? e : "Couldn't check that link.");
        } finally {
            busyRemote = null;
        }
    }

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

    // ── Previews ─────────────────────────────────────────────────────────────
    // Same fallback chain as AssetCard (thumbnail → original for small images),
    // minus animation: a looping GIF beside the fields you're editing is noise.

    function previewSrc(a: AssetMetadata): string | null {
        if (a.thumb_path) {
            // A rebuild reuses this id.webp path with new bytes; the version busts
            // the webview cache.
            return `${convertFileSrc(a.thumb_path)}?v=${assetLibrary.thumbVersion}`;
        }
        // Generation done but no thumbnail written because the source is already
        // small — the original is the preview.
        if (a.thumb_hash && a.asset_type === "image" && a.dest_path) {
            return convertFileSrc(a.dest_path);
        }
        return null;
    }

    const TYPE_ICONS = { image: ImageIcon, video: Film, audio: Music, unknown: FileIcon };

    /** The src that finished loading, so the ThumbHash blur steps aside for it. */
    let loadedSrc = $state<string | null>(null);
    const singleSrc = $derived(asset ? previewSrc(asset) : null);

    // A few of the selected rows, fanned as a stack. Hydrated on demand for the
    // same reason as the single row: the selection can outlive its window.
    const stackIds = $derived(
        current.kind === "assets" && current.ids.length > 1 ? current.ids.slice(0, 3) : [],
    );
    $effect(() => {
        const missing = stackIds.filter((id) => !assetLibrary.heavy.has(id));
        if (missing.length > 0) assetLibrary.ensure(missing);
    });

    /** Rotation and x-offset per card, by stack size; the last card is in front. */
    const FAN: Record<number, [number, number][]> = {
        2: [
            [-6, -22],
            [4, 18],
        ],
        3: [
            [-7, -34],
            [5, 34],
            [0, 0],
        ],
    };

    /** Fit a card inside the stack, keeping the asset's shape when it's known. */
    function fit(w: number, h: number, maxW = 140, maxH = 120): [number, number] {
        if (w <= 0 || h <= 0) return [maxH, maxH];
        const s = Math.min(maxW / w, maxH / h);
        return [Math.round(w * s), Math.round(h * s)];
    }

    let detailsOpen = $state(true);

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
        "w-full rounded-md border border-line bg-raised px-2.5 py-1.5 text-[12.5px] text-neutral-200 " +
        "transition-[border-color,box-shadow] placeholder:text-neutral-600 hover:border-line-strong " +
        "focus:border-brand-line focus:outline-none focus:ring-[3px] focus:ring-brand-soft";
    const legendClass = "mb-2 block text-[11.5px] font-semibold text-neutral-500";
    // The editable title: plain text until you reach for it.
    const titleClass =
        "-ml-2 h-8 min-w-0 flex-1 rounded-md border border-transparent bg-transparent px-2 " +
        "text-[15px] font-semibold tracking-[-0.01em] text-neutral-100 transition-[background-color,border-color,box-shadow] " +
        "hover:bg-raised focus:border-brand-line focus:bg-raised focus:outline-none " +
        "focus:ring-[3px] focus:ring-brand-soft";
    const sectionClass = "mt-4 border-t border-line pt-4";
    const factClass =
        "rounded-[5px] bg-raised px-1.5 py-0.5 text-[11px] tabular-nums text-neutral-400";
    const buttonClass =
        "inline-flex items-center gap-1.5 rounded-md border border-line px-2.5 py-1 text-xs " +
        "text-neutral-300 transition-colors hover:bg-wash hover:text-neutral-100 disabled:opacity-40";
    // A quiet checkerboard, so transparent art reads as transparent.
    const checkerStyle =
        "background-color: #131316; background-image: conic-gradient(#18181c 25%, transparent 0 50%, " +
        "#18181c 0 75%, transparent 0); background-size: 16px 16px;";
</script>

{#snippet row(label: string, value: string)}
    <div class="flex h-[26px] items-center justify-between gap-3">
        <span class="shrink-0 text-neutral-500">{label}</span>
        <span class="truncate text-right tabular-nums text-neutral-300" title={value}>{value}</span>
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
            class="block min-h-16 resize-y leading-[1.45] {inputClass}"
        ></textarea>
    </label>
{/snippet}

<aside class="flex h-full flex-col overflow-y-auto p-3.5 pb-10 text-sm" aria-label="Inspector">
    {#if current.kind === "assets" && current.ids.length === 1}
        {#if asset}
            {@const TypeIcon = TYPE_ICONS[asset.asset_type] ?? FileIcon}
            {@const placeholder = thumbHashUrl(asset.thumb_hash)}
            <!-- Double-click opens the viewer, the same gesture as on the tile. -->
            <div
                role="group"
                aria-label={asset.filename}
                ondblclick={() => viewer.openById(asset.id)}
                class="group relative h-[200px] shrink-0 overflow-hidden rounded-lg ring-1 ring-inset ring-line"
                style={checkerStyle}
            >
                {#if placeholder && loadedSrc !== singleSrc}
                    <img
                        src={placeholder}
                        alt=""
                        aria-hidden="true"
                        class="absolute inset-3 h-[calc(100%-1.5rem)] w-[calc(100%-1.5rem)] object-contain"
                    />
                {/if}
                {#if singleSrc}
                    <img
                        src={singleSrc}
                        alt=""
                        draggable="false"
                        onload={() => (loadedSrc = singleSrc)}
                        class="absolute inset-3 h-[calc(100%-1.5rem)] w-[calc(100%-1.5rem)] object-contain"
                    />
                {:else if !placeholder}
                    <div class="grid h-full place-items-center text-neutral-600">
                        <TypeIcon class="h-8 w-8" />
                    </div>
                {/if}
                <button
                    type="button"
                    onclick={() => viewer.openById(asset.id)}
                    title="Open"
                    aria-label="Open in viewer"
                    class="absolute right-2 bottom-2 grid h-7 w-7 place-items-center rounded-md
                           bg-[rgb(12_12_14/0.7)] text-neutral-200 opacity-0
                           shadow-[inset_0_0_0_1px_rgb(255_255_255/0.1)] backdrop-blur
                           transition-opacity group-hover:opacity-100 hover:bg-[rgb(30_30_34/0.9)]
                           focus-visible:opacity-100"
                >
                    <Maximize2 class="h-3.5 w-3.5" />
                </button>
            </div>

            <div class="mt-3.5 flex items-center gap-0.5">
                <input
                    type="text"
                    value={draftName}
                    oninput={(e) => editName(e.currentTarget.value)}
                    onblur={commitName}
                    spellcheck="false"
                    aria-label="Name"
                    title="Renaming affects this library only"
                    class={titleClass}
                />
                <!-- The extension is shown, never edited: it describes the
                     actual bytes, not the user's label for them. -->
                {#if asset.extension}
                    <span class="shrink-0 text-[13px] font-medium text-neutral-600"
                        >.{asset.extension}</span
                    >
                {/if}
            </div>
            <div class="mt-1.5 flex flex-wrap gap-1.5">
                <!-- Videos record 0×0 until a frame is captured; no chip beats "0 × 0". -->
                {#if asset.width > 0 && asset.height > 0}
                    <span class={factClass}>{asset.width} × {asset.height}</span>
                    <span class={factClass}>
                        {formatAspectRatio(asset.width, asset.height)} ·
                        {describeShape(asset.width, asset.height)}
                    </span>
                {/if}
                <span class={factClass}>{formatBytes(asset.file_size)}</span>
                {#if asset.duration_ms}
                    <span class={factClass}>{formatDuration(asset.duration_ms / 1000)}</span>
                {/if}
            </div>

            <section class={sectionClass}>
                <TagEditor assetIds={[asset.id]} {legendClass} />
            </section>

            <!-- Same control as bulk mode, just with a selection of one — so the
                 tri-state collapses to plain membership on its own. -->
            <section class={sectionClass}>
                <FolderMembership assetIds={[asset.id]} {legendClass} />
            </section>

            <!-- Images only: nothing else produces a palette, and an empty strip
                 on a video would read as a bug rather than as "not applicable". -->
            {#if asset.asset_type === "image"}
                <section class={sectionClass}>
                    <PaletteSection assetId={asset.id} {legendClass} />
                </section>
            {/if}

            <section class={sectionClass}>
                {@render notesField()}
            </section>

            <section class={sectionClass}>
                <label class="block">
                    <span class={legendClass}>Source URL</span>
                    <div class="relative flex items-center">
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
                            class="pr-9 {inputClass}"
                        />
                        <button
                            type="button"
                            onclick={openSource}
                            disabled={!openable}
                            title={openable ? "Open in browser" : "Only http(s) links can be opened"}
                            aria-label="Open in browser"
                            class="absolute right-0.5 grid h-[26px] w-[26px] place-items-center rounded-md
                                   text-neutral-500 enabled:hover:bg-wash enabled:hover:text-neutral-200
                                   disabled:cursor-not-allowed disabled:text-neutral-700"
                        >
                            <ExternalLink class="h-3.5 w-3.5" />
                        </button>
                    </div>
                </label>
            </section>

            <!-- ── Online asset ───────────────────────────────────────────────
                 Shown only when the bytes are elsewhere. The state line is the
                 honest part: "reachable", "not checked lately" and "gone" are
                 three different facts and the user acts differently on each. -->
            {#if asset.origin === "remote"}
                <section class="flex flex-col gap-2 {sectionClass}">
                    <div class="flex items-center gap-1.5">
                        <Cloud
                            class="h-3.5 w-3.5 {asset.remote_state === 'unavailable'
                                ? 'text-amber-400'
                                : 'text-neutral-500'}"
                        />
                        <span class="text-[11.5px] font-semibold text-neutral-400">Online asset</span>
                    </div>
                    <p class="text-[11px] leading-relaxed text-neutral-500">
                        {#if asset.remote_state === "unavailable"}
                            <span class="text-amber-300/90">
                                This link couldn't be reached when it was last checked.
                            </span>
                            The thumbnail and everything you've filed about it are still here.
                        {:else}
                            The file streams from the web and takes no space here beyond its
                            thumbnail.{#if asset.supports_range === false}
                                This source doesn't support seeking.{/if}
                        {/if}
                    </p>
                    {#if asset.remote_url}
                        <p class="break-all text-[11px] text-neutral-600">{asset.remote_url}</p>
                    {/if}
                    <div class="flex items-center gap-2">
                        <button
                            type="button"
                            onclick={() => keepOffline(asset.id)}
                            disabled={busyRemote !== null}
                            class="rounded-md bg-raised-2 px-2.5 py-1 text-xs font-medium text-neutral-100
                                   shadow-[inset_0_0_0_1px_var(--nv-line-strong)] transition-colors
                                   hover:bg-wash-strong disabled:opacity-40"
                        >
                            {busyRemote === "download" ? "Downloading…" : "Keep offline"}
                        </button>
                        <button
                            type="button"
                            onclick={() => recheck(asset.id)}
                            disabled={busyRemote !== null}
                            class={buttonClass}
                        >
                            {busyRemote === "verify" ? "Checking…" : "Check again"}
                        </button>
                    </div>
                </section>
            {/if}

            <section class={sectionClass}>
                <button
                    type="button"
                    onclick={() => (detailsOpen = !detailsOpen)}
                    aria-expanded={detailsOpen}
                    class="mb-2 flex w-full items-center gap-1.5 text-left text-[11.5px] font-semibold
                           text-neutral-500 transition-colors hover:text-neutral-300"
                >
                    Details
                    <ChevronDown
                        class="h-3 w-3 transition-transform {detailsOpen ? '' : '-rotate-90'}"
                    />
                </button>
                {#if detailsOpen}
                    <div class="flex flex-col text-xs">
                        {@render row("Format", asset.extension.toUpperCase() || "—")}
                        {@render row("Type", TYPE_LABELS[asset.asset_type] ?? "File")}
                        {@render row("Dimensions", `${asset.width} × ${asset.height}`)}
                        {@render row(
                            "Aspect ratio",
                            `${formatAspectRatio(asset.width, asset.height)} · ${describeShape(asset.width, asset.height)}`,
                        )}
                        {@render row("File size", formatBytes(asset.file_size))}
                        {#if asset.duration_ms}
                            {@render row("Duration", formatDuration(asset.duration_ms / 1000))}
                        {/if}
                        {@render row("Added", formatTimestamp(asset.imported_date))}
                        <!-- "Created" is the filesystem timestamp, not EXIF capture time.
                             Labelled plainly on purpose: when EXIF lands it becomes its
                             own field rather than quietly redefining this one. -->
                        {@render row("Created", formatTimestamp(asset.creation_date))}
                        {@render row("Modified", formatTimestamp(asset.modified_date))}
                    </div>
                {/if}
            </section>
        {:else}
            <p class="text-xs text-neutral-500">Loading details…</p>
        {/if}
    {:else if current.kind === "assets"}
        <!-- Name, notes and source URL are absent rather than disabled: a greyed
             box showing nothing is a puzzle, and there's no safe answer to
             "append or replace?" for a field shared by 50 rows. -->
        {@const fan = FAN[stackIds.length] ?? []}
        <div class="relative grid h-[160px] shrink-0 place-items-center" aria-hidden="true">
            {#each stackIds as id, i (id)}
                {@const a = assetLibrary.heavy.get(id)}
                {@const [w, h] = fit(a?.width ?? 0, a?.height ?? 0)}
                {@const src = a ? (previewSrc(a) ?? thumbHashUrl(a.thumb_hash)) : null}
                <div
                    class="absolute overflow-hidden rounded-lg bg-raised
                           shadow-[0_8px_24px_rgb(0_0_0/0.55),0_0_0_1px_rgb(255_255_255/0.08)]"
                    style="width: {w}px; height: {h}px; z-index: {i}; transform: translateX({fan[i]?.[1] ??
                        0}px) rotate({fan[i]?.[0] ?? 0}deg)"
                >
                    {#if src}
                        <img {src} alt="" draggable="false" class="h-full w-full object-cover" />
                    {/if}
                </div>
            {/each}
        </div>
        <div class="mt-1.5 text-center">
            <h2 class="text-sm font-semibold text-neutral-100">
                {current.ids.length} assets selected
            </h2>
            <p class="mt-0.5 text-xs text-neutral-500">
                {summary ? `${formatBytes(summary.total_bytes)} total` : "Measuring…"}
            </p>
        </div>

        <section class={sectionClass}>
            <TagEditor assetIds={current.ids} {legendClass} />
        </section>
        <section class={sectionClass}>
            <FolderMembership assetIds={current.ids} {legendClass} />
        </section>
    {:else if folder}
        <div class="flex items-center">
            <input
                type="text"
                value={draftName}
                oninput={(e) => editName(e.currentTarget.value)}
                onblur={commitName}
                spellcheck="false"
                aria-label="Folder name"
                class={titleClass}
            />
        </div>

        <section class={sectionClass}>
            {@render notesField()}
        </section>

        <!-- Auto-tags sit above pinning because they change what the folder DOES
             to assets, which is a bigger claim than how it looks in the sidebar. -->
        <section class={sectionClass}>
            <FolderAutoTags folderId={folder.id} {legendClass} />
        </section>

        <!-- Pinning lives here as well as in the right-click menu, because the
             inspector is where you land after clicking a folder — and "put this
             in my sidebar" is a property of the folder you're looking at, not a
             command you should have to remember to right-click for. -->
        <section class={sectionClass}>
            <span class={legendClass}>Sidebar</span>
            <button
                type="button"
                onclick={() =>
                    assetLibrary.setPinned("folder", folder.id, folder.pin_position === null)}
                class={buttonClass}
            >
                {#if folder.pin_position !== null}
                    <PinOff class="h-3.5 w-3.5" /> Unpin
                {:else}
                    <Pin class="h-3.5 w-3.5" /> Pin to sidebar
                {/if}
            </button>

            {#if folder.pin_position !== null}
                <div class="mt-2">
                    <PinSwatches kind="folder" id={folder.id} />
                </div>
            {/if}
        </section>

        <section class="flex flex-col text-xs {sectionClass}">
            <span class={legendClass}>Details</span>
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
        </section>
    {:else}
        <!-- An empty state, not a blank panel: a void reads as a bug. This is
             also what "All" and "Uncategorized" resolve to — they're places, and
             a place has nothing to inspect. -->
        <div
            class="m-auto flex max-w-[24ch] flex-col items-center gap-2.5 text-center text-xs
                   text-neutral-600"
        >
            <ImageIcon class="h-5 w-5" />
            <p>Select an asset or a folder to see its details.</p>
        </div>
    {/if}
</aside>
