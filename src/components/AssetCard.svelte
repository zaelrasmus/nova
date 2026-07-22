<script lang="ts">
    import { convertFileSrc } from "@tauri-apps/api/core";
    import { thumbHashUrl } from "$lib/assets.svelte";
    import type { AssetMetadata } from "$lib/assets.svelte";

    interface Props {
        assetType: "image" | "video" | "audio" | "unknown";
        thumbHash: string | null;
        isAnimated: boolean;
        animate: boolean;
        heavy?: AssetMetadata;
        style: string;
        onClick?: () => void;
    }

    let { assetType, thumbHash, isAnimated, animate, heavy, style, onClick }: Props = $props();

    const placeholder = $derived(thumbHashUrl(thumbHash));
    // Animated original when the toggle is on and the asset is animated;
    // otherwise the static WebP thumbnail. No thumbnail => generic per-type card.
    const previewSrc = $derived(
            animate && isAnimated && heavy?.dest_path
                ? convertFileSrc(heavy.dest_path)
                : heavy?.thumb_path
                  ? convertFileSrc(heavy.thumb_path)
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

<!-- Shared preview renderer: any type that has a thumbnail (images now;
     videos/audio once 11d generates keyframes/waveforms). -->
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


<!-- Image whose thumbnail is still being generated in the background. -->
{#snippet pendingImage()}
    <div class="w-full h-full bg-neutral-800/60 animate-pulse"></div>
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
    role="button"
    tabindex="0"
    class="absolute top-0 overflow-hidden rounded-md bg-neutral-900 cursor-pointer select-none
           ring-offset-neutral-950 hover:ring-2 hover:ring-neutral-500 hover:ring-offset-1
           focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-neutral-400"
    onclick={onClick}
    onkeydown={(e) => e.key === "Enter" && onClick?.()}
>
    {#if placeholder || previewSrc}
        {@render thumbnail()}
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
