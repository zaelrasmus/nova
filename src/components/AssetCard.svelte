<!--
  AssetCard — one tile in the grid. Rendered once per VISIBLE item, so it is the
  hottest component in the app: keep it cheap and free of subscriptions.

  Presentational by design. It takes its geometry as a `style` string the grid
  computed and reports gestures upward; it holds no selection or layout state.

  Images degrade gracefully: ThumbHash blur → thumbnail → original, so a card
  always paints something even before its thumbnail has been generated (which
  happens on view, not at import). Thumbnail URLs carry `thumbVersion` to bust
  the webview cache after a rebuild writes new bytes to the same path.
-->
<script lang="ts">
    import { convertFileSrc } from "@tauri-apps/api/core";
    import { assetLibrary, thumbHashUrl } from "$lib/assets.svelte";
    import type { AssetMetadata } from "$lib/assets.svelte";

    interface Props {
        assetType: "image" | "video" | "audio" | "unknown";
        thumbHash: string | null;
        isAnimated: boolean;
        animate: boolean;
        heavy?: AssetMetadata;
        style: string;
        selected?: boolean;
        /** Asset id and its manifest index, for the grid's reorder hit-testing. */
        dataId?: string;
        dataIndex?: number;
        /**
         * Press. Fires BEFORE `onClick` and is where the selection change
         * normally lands, so a drag begins with the right payload.
         */
        onPointerDown?: (e: PointerEvent) => void;
        /** Release without a drag. Resolves what `onPointerDown` deferred. */
        onClick?: (e: MouseEvent) => void;
        /** Open this asset in the viewer — double-click, or Enter when focused. */
        onOpen?: () => void;
    }

    let {
        assetType,
        thumbHash,
        isAnimated,
        animate,
        heavy,
        style,
        selected = false,
        dataId,
        dataIndex,
        onPointerDown,
        onClick,
        onOpen,
    }: Props = $props();

    const placeholder = $derived(thumbHashUrl(thumbHash));

    // Read from the HEAVY row on purpose. Putting `origin` in the light row would
    // add a field to all 100k manifest entries over IPC to serve a badge on the
    // few dozen tiles actually on screen; the badge simply appears with the rest
    // of the hydrated metadata, exactly as the thumbnail does.
    const online = $derived(heavy?.origin === "remote");
    const unavailable = $derived(online && heavy?.remote_state === "unavailable");
    // Animated original when the toggle is on and the asset is animated;
    // otherwise the static WebP thumbnail. No thumbnail => generic per-type card.
    const previewSrc = $derived(
            animate && isAnimated && heavy?.dest_path
                ? convertFileSrc(heavy.dest_path)
                : heavy?.thumb_path
                  // A rebuild reuses this id.webp path with new bytes, so append the
                  // version to force the webview to refetch instead of caching.
                  ? `${convertFileSrc(heavy.thumb_path)}?v=${assetLibrary.thumbVersion}`
                  // Generation done (thumbHash set) but no thumbnail file was written
                  // because the source is already small — show the original directly.
                  : thumbHash && assetType === "image" && heavy?.dest_path
                    ? convertFileSrc(heavy.dest_path)
                    : null,
        );

    function fadeOnLoad(node: HTMLImageElement) {
            node.style.opacity = "0";
            node.style.transition = "opacity 100ms ease";
            const onLoad = () => (node.style.opacity = "1");
            const onError = () => (node.style.opacity = "0.3");
            node.addEventListener("load", onLoad, { once: true });
            node.addEventListener("error", onError, { once: true });
            if (node.complete && node.naturalWidth > 0) node.style.opacity = "1";
            return {
                destroy() {
                    node.removeEventListener("load", onLoad);
                    node.removeEventListener("error", onError);
                },
            };
        }
</script>

<!-- Shared preview renderer: any type that has a thumbnail. Images are generated
     in Rust; video keyframes and audio waveforms are captured in the webview
     (see mediathumbs.ts) but land in the same `thumb_path`, so this is unaware
     of which produced it. -->
{#snippet thumbnail()}
    {#if placeholder}
        <img
            src={placeholder}
            alt=""
            aria-hidden="true"
            class="absolute inset-0 w-full h-full object-cover"
        />
    {/if}
    <img
        use:fadeOnLoad
        src={previewSrc}
        alt={heavy?.filename ?? ""}
        class="relative w-full h-full object-cover"
        draggable="false"
        decoding="async"
    />
{/snippet}


<!-- Corner badge marking a tile as playable. Only shown once a media asset HAS a
     thumbnail: before that the generic card's big glyph already says what it is,
     and afterwards a video frame is indistinguishable from a photo without it.
     Inline SVG rather than a lucide component — this renders once per visible
     tile, and a component instance per card is exactly what this file avoids. -->
{#snippet mediaBadge(kind: "video" | "audio")}
    <span
        class="pointer-events-none absolute bottom-1 left-1 grid h-5 w-5 place-items-center
               rounded-full bg-black/60 text-white ring-1 ring-white/15"
    >
        <svg viewBox="0 0 24 24" fill="currentColor" class="h-3 w-3" aria-hidden="true">
            {#if kind === "video"}
                <path d="M8 5v14l11-7z" />
            {:else}
                <path d="M12 3v10.55A4 4 0 1 0 14 17V7h4V3h-6z" />
            {/if}
        </svg>
    </span>
{/snippet}

<!-- Online asset: the bytes live at a URL, not on disk. Shown on the TILE rather
     than only in the Inspector, because at 100k assets "which of these need the
     network?" has to be answerable at a glance. A rotted link is struck through
     in amber — knowing something is gone matters more than it looking tidy. -->
{#snippet onlineBadge()}
    <span
        class="pointer-events-none absolute right-1 top-1 z-10 grid h-5 w-5 place-items-center
               rounded-full bg-black/60 ring-1 ring-white/15
               {unavailable ? 'text-amber-400' : 'text-white/80'}"
        title={unavailable
            ? "This link is no longer reachable"
            : "Online asset — the file streams from the web"}
    >
        <svg viewBox="0 0 24 24" fill="currentColor" class="h-3 w-3" aria-hidden="true">
            <path
                d="M6.5 19A4.5 4.5 0 0 1 6 10.03 6 6 0 0 1 17.7 8.6 4.75 4.75 0 0 1 17.5 19h-11z"
            />
            {#if unavailable}
                <!-- Struck through: reachable and gone must not look alike. -->
                <path d="M3 3l18 18" stroke="currentColor" stroke-width="2.5" fill="none" />
            {/if}
        </svg>
    </span>
{/snippet}

<!-- Image whose thumbnail is still being generated in the background. -->
{#snippet pendingImage()}
    <div class="skeleton-shimmer h-full w-full"></div>
{/snippet}

<!-- Generic card for types without a preview yet. -->
{#snippet generic(icon: string)}
    <div
        class="flex flex-col items-center justify-center gap-1.5 w-full h-full
               bg-neutral-800 text-neutral-400 p-2"
    >
        <span class="text-3xl leading-none">{icon}</span>
        {#if heavy?.filename}
            <span class="text-[10px] text-neutral-500 truncate max-w-full">{heavy.filename}</span>
        {/if}
    </div>
{/snippet}

<!-- The shell: owns positioning, focus, interaction. #12's DnD attaches here,
     unaffected by which interior snippet renders. -->
<div
    {style}
    role="option"
    aria-selected={selected}
    data-asset-id={dataId}
    data-asset-index={dataIndex}
    tabindex="0"
    class="absolute top-0 overflow-hidden rounded-md bg-neutral-900 cursor-pointer select-none
           ring-offset-2 ring-offset-white
           focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-neutral-400
           {selected ? 'ring-2 ring-blue-500' : 'hover:ring-2 hover:ring-neutral-400'}"
    onpointerdown={onPointerDown}
    onclick={onClick}
    ondblclick={onOpen}
    onkeydown={(e) => {
        // Enter opens the viewer (Finder-style). Space is deliberately left to
        // bubble to the grid's global handler, where it means "QuickLook".
        if (e.key === "Enter") {
            e.preventDefault();
            onOpen?.();
        }
    }}
>
    {#if online}
        {@render onlineBadge()}
    {/if}

    {#if placeholder || previewSrc}
        {@render thumbnail()}
        {#if assetType === "video" || assetType === "audio"}
            {@render mediaBadge(assetType)}
        {/if}
    {:else if assetType === "audio"}
        {@render generic("🎵")}
    {:else if assetType === "video"}
        {@render generic("🎬")}
    {:else if assetType === "image"}
        {@render pendingImage()}
    {:else}
        {@render generic("📄")}
    {/if}
</div>
