<!--
  Add-from-URL, and the receipt that precedes it.

  The receipt is the point of this dialog. Nova knows nothing about Twitter or
  Discord or any other host; it makes ONE ranged GET and shows the user exactly
  what came back — real type, real size, whether seeking will work, whether the
  link is the signed kind that dies tomorrow. That is what lets it be honest
  about sites it has never heard of, with no per-site logic to maintain.

  Nothing is created until the user agrees to what they were shown. A dead link
  never becomes an asset.
-->
<script lang="ts">
    import { onMount } from "svelte";
    import { listen, type UnlistenFn } from "@tauri-apps/api/event";
    import { Image, Film, Music, File, TriangleAlert, LoaderCircle, Globe } from "@lucide/svelte";
    import {
        probeUrl,
        importFromUrl,
        addRemoteAsset,
        type UrlProbe,
        type UrlImportResult,
    } from "$lib/remote";
    import type { AssetMetadata } from "$lib/assets.svelte";
    import { formatBytes } from "$lib/format";

    interface Props {
        /** The pasted link. Probed on mount. */
        url: string;
        /** Folder to import into, or null for the library at large. */
        targetFolder?: string | null;
        onclose: () => void;
        /** Fires once the import has committed, so the caller can refresh + toast. */
        onimported: (result: UrlImportResult) => void;
        /** Fires when a link was saved WITHOUT downloading. */
        onlinked: (asset: AssetMetadata) => void;
        /** Brackets the download+import, so the caller can block a second import. */
        onbusy?: (busy: boolean) => void;
    }

    const { url, targetFolder = null, onclose, onimported, onlinked, onbusy }: Props = $props();

    type Phase = "probing" | "ready" | "failed" | "downloading" | "saving";

    let phase = $state<Phase>("probing");
    let probe = $state<UrlProbe | null>(null);
    let error = $state<string | null>(null);
    let stem = $state("");

    // [received, total] — total is null when the server declined to say, which is
    // why the bar falls back to an indeterminate stripe rather than showing 0%.
    let received = $state(0);
    let total = $state<number | null>(null);
    let unlisten: UnlistenFn | null = null;

    const pct = $derived(total && total > 0 ? Math.min(100, (received / total) * 100) : null);

    const TYPE_ICON = { image: Image, video: Film, audio: Music, unknown: File } as const;
    const TypeIcon = $derived(TYPE_ICON[probe?.asset_type ?? "unknown"]);

    /** "JPEG image", "MP4 video" — the sniffed extension plus what it is. */
    const typeLabel = $derived.by(() => {
        if (!probe) return "";
        const ext = probe.extension ? probe.extension.toUpperCase() : "Unknown";
        const kind = probe.asset_type === "unknown" ? "file" : probe.asset_type;
        return `${ext} ${kind}`;
    });

    /**
     * Below this, keeping a local copy is close to free, so Download leads.
     * A suggestion about emphasis ONLY — never a gate. Both choices are offered
     * at every size, and nothing is ever fetched without a click: a mistyped or
     * mispasted link must not be able to pull down a file on its own.
     */
    const SMALL_ENOUGH_BYTES = 25 * 1024 * 1024;

    const preferDownload = $derived(
        probe !== null &&
            (probe.looks_temporary || (probe.size !== null && probe.size <= SMALL_ENOUGH_BYTES)),
    );

    /** A redirect worth mentioning — same URL back is noise. */
    const redirected = $derived(
        probe !== null && probe.final_url !== probe.original_url ? probe.final_url : null,
    );

    onMount(() => {
        void run();
        return () => unlisten?.();
    });

    async function run() {
        phase = "probing";
        error = null;
        try {
            const result = await probeUrl(url);
            probe = result;
            // Seed the editable name with the stem only; the extension is Rust's
            // to decide and is shown as a suffix the user can't edit.
            const dot = result.filename.lastIndexOf(".");
            stem = dot > 0 ? result.filename.slice(0, dot) : result.filename;
            phase = "ready";
        } catch (e) {
            error = typeof e === "string" ? e : "That link couldn't be checked.";
            phase = "failed";
        }
    }

    /**
     * Save the link without fetching it.
     *
     * Offered at EVERY size, not just for large files. Space is a reason of its
     * own: a 500MB video you want catalogued but can't afford on disk is exactly
     * the case this exists for, and a small file the user simply doesn't want a
     * copy of is theirs to decline.
     */
    async function saveLink() {
        if (!probe) return;
        phase = "saving";
        onbusy?.(true);
        try {
            const asset = await addRemoteAsset(url, stem.trim() || null, targetFolder);
            onlinked(asset);
            onclose();
        } catch (e) {
            error = typeof e === "string" ? e : "Couldn't save that link.";
            phase = "failed";
        } finally {
            onbusy?.(false);
        }
    }

    async function download() {
        if (!probe) return;
        phase = "downloading";
        received = 0;
        total = probe.size;
        onbusy?.(true);

        unlisten = await listen<[number, number | null]>("url-download-progress", (event) => {
            received = event.payload[0];
            total = event.payload[1] ?? probe?.size ?? null;
        });

        try {
            const result = await importFromUrl(url, stem.trim() || null, targetFolder);
            onimported(result);
            onclose();
        } catch (e) {
            error = typeof e === "string" ? e : "The download failed.";
            phase = "failed";
        } finally {
            unlisten?.();
            unlisten = null;
            onbusy?.(false);
        }
    }
</script>

<svelte:window
    onkeydown={(e) => {
        // Escape cancels — except mid-download, where it would leave the backend
        // writing into a dialog that no longer exists to report the result.
        if (e.key === "Escape" && phase !== "downloading") onclose();
    }}
/>

<div class="fixed inset-0 z-[95] grid place-items-center bg-black/60 p-6">
    <button
        type="button"
        tabindex="-1"
        aria-label="Cancel"
        class="absolute inset-0 cursor-default"
        onclick={() => phase !== "downloading" && onclose()}
    ></button>

    <div
        role="dialog"
        aria-modal="true"
        aria-label="Add from URL"
        class="relative flex w-[520px] max-w-full flex-col gap-4 rounded-xl border border-neutral-800
               bg-neutral-950 p-5 shadow-2xl"
    >
        <div>
            <h2 class="text-sm font-semibold text-neutral-100">Add from URL</h2>
            <p class="mt-1 flex items-center gap-1.5 text-xs text-neutral-500">
                <Globe class="h-3 w-3 shrink-0" />
                <span class="truncate">{probe?.host || new URL(url).host}</span>
            </p>
        </div>

        {#if phase === "probing"}
            <div class="flex items-center gap-2.5 py-6 text-sm text-neutral-400">
                <LoaderCircle class="h-4 w-4 animate-spin" />
                Checking the link…
            </div>
        {:else if phase === "failed"}
            <div
                class="flex gap-3 rounded-lg border border-amber-900/50 bg-amber-950/20 p-3
                       text-xs leading-relaxed text-amber-200/90"
            >
                <TriangleAlert class="mt-0.5 h-4 w-4 shrink-0 text-amber-400/80" />
                <div>{error}</div>
            </div>
        {:else if probe?.is_html}
            <!-- The most common paste mistake by a wide margin, and the one place
                 a precise message replaces a support conversation. -->
            <div
                class="flex gap-3 rounded-lg border border-amber-900/50 bg-amber-950/20 p-3
                       text-xs leading-relaxed text-amber-200/90"
            >
                <TriangleAlert class="mt-0.5 h-4 w-4 shrink-0 text-amber-400/80" />
                <div>
                    <div class="font-medium">That's a web page, not a media file.</div>
                    <div class="mt-1 text-amber-200/70">
                        Open the page, right-click the image or video you want, and copy the
                        media address — then paste that instead.
                    </div>
                </div>
            </div>
        {:else if probe}
            <!-- The receipt -->
            <div class="flex items-center gap-3 rounded-lg border border-neutral-800 bg-neutral-900/60 p-3">
                <div class="grid h-10 w-10 shrink-0 place-items-center rounded-lg bg-neutral-800 text-neutral-400">
                    <TypeIcon class="h-5 w-5" />
                </div>
                <div class="min-w-0">
                    <div class="text-sm text-neutral-100">{typeLabel}</div>
                    <div class="mt-0.5 text-xs text-neutral-500">
                        {probe.size !== null ? formatBytes(probe.size) : "size unknown"}
                        <span class="text-neutral-700"> · </span>
                        {probe.supports_range ? "seeking supported" : "no seeking"}
                    </div>
                </div>
            </div>

            {#if redirected}
                <p class="text-[11px] leading-relaxed text-neutral-600">
                    Redirects to <span class="break-all text-neutral-500">{redirected}</span>
                </p>
            {/if}

            {#if probe.looks_temporary}
                <div
                    class="flex gap-3 rounded-lg border border-amber-900/50 bg-amber-950/20 p-3
                           text-xs leading-relaxed text-amber-200/90"
                >
                    <TriangleAlert class="mt-0.5 h-4 w-4 shrink-0 text-amber-400/80" />
                    <div>
                        This link expires{probe.expires_in ? ` in ${probe.expires_in}` : ""}.
                        <span class="text-amber-200/70">
                            Downloading it now is the only way to keep it.
                        </span>
                    </div>
                </div>
            {/if}

            {#if phase === "downloading"}
                <div class="flex flex-col gap-2 py-1">
                    <div class="flex items-center justify-between text-xs text-neutral-400">
                        <span>Downloading…</span>
                        <span class="tabular-nums">
                            {formatBytes(received)}{total ? ` / ${formatBytes(total)}` : ""}
                        </span>
                    </div>
                    <div class="h-1 w-full overflow-hidden rounded-full bg-neutral-800">
                        {#if pct !== null}
                            <div
                                class="h-full rounded-full bg-blue-500 transition-[width] duration-150"
                                style="width:{pct}%"
                            ></div>
                        {:else}
                            <!-- No Content-Length: show motion, never a fake percentage. -->
                            <div class="h-full w-1/3 animate-pulse rounded-full bg-blue-500"></div>
                        {/if}
                    </div>
                </div>
            {:else}
                <label class="flex flex-col gap-1.5">
                    <span class="text-xs text-neutral-400">Name</span>
                    <div class="flex items-center gap-2">
                        <input
                            type="text"
                            bind:value={stem}
                            spellcheck="false"
                            class="min-w-0 flex-1 rounded border border-neutral-800 bg-neutral-900 px-2 py-1.5
                                   text-sm text-neutral-100 outline-none focus:border-neutral-600"
                        />
                        {#if probe.extension}
                            <span class="shrink-0 text-xs tabular-nums text-neutral-600">
                                .{probe.extension}
                            </span>
                        {/if}
                    </div>
                </label>
            {/if}
        {/if}

        <div class="flex items-center justify-end gap-3">
            {#if phase === "failed"}
                <button
                    type="button"
                    onclick={onclose}
                    class="rounded border border-neutral-800 px-3 py-1.5 text-xs text-neutral-300
                           transition-colors hover:bg-neutral-800">Close</button
                >
                <button
                    type="button"
                    onclick={run}
                    class="rounded bg-blue-600 px-3 py-1.5 text-xs font-medium text-white
                           transition-colors hover:bg-blue-500">Try again</button
                >
            {:else if probe?.is_html}
                <button
                    type="button"
                    onclick={onclose}
                    class="rounded bg-blue-600 px-3 py-1.5 text-xs font-medium text-white
                           transition-colors hover:bg-blue-500">Close</button
                >
            {:else}
                <button
                    type="button"
                    onclick={onclose}
                    disabled={phase === "downloading" || phase === "saving"}
                    class="rounded border border-neutral-800 px-3 py-1.5 text-xs text-neutral-300
                           transition-colors hover:bg-neutral-800 disabled:opacity-40">Cancel</button
                >
                <!-- Always both, at every size. Which one is EMPHASISED is the
                     only thing size and expiry change: a link that will die, or
                     a file small enough that keeping it costs nothing, leads
                     with Download; anything large leads with the link. -->
                <button
                    type="button"
                    onclick={saveLink}
                    disabled={phase !== "ready"}
                    title="Catalogue the link. Nothing is stored on disk but a thumbnail."
                    class="rounded px-3 py-1.5 text-xs font-medium transition-colors
                           disabled:opacity-40
                           {preferDownload
                        ? 'border border-neutral-800 text-neutral-300 hover:bg-neutral-800'
                        : 'bg-blue-600 text-white hover:bg-blue-500'}"
                >
                    {phase === "saving" ? "Saving…" : "Save link only"}
                </button>
                <button
                    type="button"
                    onclick={download}
                    disabled={phase !== "ready"}
                    class="rounded px-3 py-1.5 text-xs font-medium transition-colors
                           disabled:opacity-40
                           {preferDownload
                        ? 'bg-blue-600 text-white hover:bg-blue-500'
                        : 'border border-neutral-800 text-neutral-300 hover:bg-neutral-800'}"
                >
                    {phase === "downloading" ? "Downloading…" : "Download"}
                </button>
            {/if}
        </div>
    </div>
</div>
