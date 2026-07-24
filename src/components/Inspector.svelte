<script lang="ts">
    import { assetLibrary, describeShape } from "$lib/assets.svelte";
    import { selection } from "$lib/selection.svelte";
    import { formatAspectRatio, formatBytes, formatTimestamp } from "$lib/format";

    // The inspector renders the selection; it never owns it. Every mode below is
    // a branch of one union, so "3 assets and a folder" can't be reached.
    const current = $derived(selection.current);

    const singleId = $derived(
        current.kind === "assets" && current.ids.length === 1 ? current.ids[0] : null,
    );
    const asset = $derived(singleId ? assetLibrary.heavy.get(singleId) : undefined);

    // The selected row is normally on screen and already hydrated, but a
    // selection outlives its window — scroll far enough and the LRU evicts it.
    $effect(() => {
        if (singleId && !assetLibrary.heavy.has(singleId)) assetLibrary.ensure([singleId]);
    });

    // Looked up rather than stored: if the folder is deleted while selected, this
    // goes undefined and the panel falls back to the empty state on its own.
    const folder = $derived(
        current.kind === "folder"
            ? assetLibrary.folders.find((f) => f.id === current.id)
            : undefined,
    );

    // Total bytes for a multi-selection, but ONLY when every row happens to be
    // hydrated. Summing the cached subset would silently under-report, and
    // hydrating thousands of rows to answer it is the wrong trade — the real fix
    // is one aggregate query, which lands with bulk edit.
    const bulkBytes = $derived.by(() => {
        if (current.kind !== "assets") return null;
        let total = 0;
        for (const id of current.ids) {
            const row = assetLibrary.heavy.get(id);
            if (!row) return null;
            total += row.file_size;
        }
        return total;
    });

    const TYPE_LABELS: Record<string, string> = {
        image: "Image",
        video: "Video",
        audio: "Audio",
        unknown: "File",
    };
</script>

{#snippet row(label: string, value: string)}
    <div class="flex items-baseline justify-between gap-3 py-1">
        <span class="shrink-0 text-neutral-500">{label}</span>
        <span class="truncate text-right text-neutral-300" title={value}>{value}</span>
    </div>
{/snippet}

<aside
    class="flex h-full flex-col overflow-y-auto rounded-lg border border-neutral-800
           bg-neutral-900/40 p-3 text-sm"
    aria-label="Inspector"
>
    {#if current.kind === "assets" && current.ids.length === 1}
        {#if asset}
            <h2 class="truncate text-neutral-100" title={asset.filename}>{asset.filename}</h2>
            <p class="mt-0.5 text-xs text-neutral-500">
                {TYPE_LABELS[asset.asset_type] ?? "File"} · {asset.extension.toUpperCase()}
            </p>

            <div class="my-3 h-px bg-neutral-800"></div>

            <div class="flex flex-col text-xs">
                {@render row("Dimensions", `${asset.width} × ${asset.height}`)}
                {@render row(
                    "Aspect ratio",
                    `${formatAspectRatio(asset.width, asset.height)} · ${describeShape(asset.width, asset.height)}`,
                )}
                {@render row("Size", formatBytes(asset.file_size))}
            </div>

            <div class="my-3 h-px bg-neutral-800"></div>

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
        <h2 class="text-neutral-100">{current.ids.length} assets selected</h2>
        {#if bulkBytes !== null}
            <p class="mt-0.5 text-xs text-neutral-500">{formatBytes(bulkBytes)} total</p>
        {/if}
        <p class="mt-3 text-xs text-neutral-600">
            Batch editing arrives with the bulk inspector.
        </p>
    {:else if folder}
        <h2 class="truncate text-neutral-100" title={folder.name}>{folder.name}</h2>
        <p class="mt-0.5 text-xs text-neutral-500">Folder</p>
        <p class="mt-3 text-xs text-neutral-600">
            Item count, size and notes arrive with the folder inspector.
        </p>
    {:else}
        <!-- An empty state, not a blank panel: a void reads as a bug. This is
             also what "All" and "Uncategorized" resolve to — they're places, and
             a place has nothing to inspect. -->
        <p class="m-auto max-w-[22ch] text-center text-xs text-neutral-600">
            Select an asset or a folder to see its details.
        </p>
    {/if}
</aside>
