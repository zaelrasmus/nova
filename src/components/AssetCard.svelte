<script lang="ts">
    import { convertFileSrc } from "@tauri-apps/api/core";
    import { thumbHashUrl } from "$lib/assets.svelte";
    import type { AssetMetadata } from "$lib/assets.svelte";

    interface Props {
        thumbHash: string | null;
        heavy?: AssetMetadata;
        style: string;
        onClick?: () => void;
    }

    let { thumbHash, heavy, style, onClick }: Props = $props();

    const placeholder = $derived(thumbHashUrl(thumbHash));
    // Prefer the Webp thumbnail; fallback to the original if no thumb exists
    const imgSrc = $derived( heavy ? convertFileSrc(heavy.thumb_path || heavy.dest_path) : null ,);

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


<div
    {style}
    role="button"
    tabindex="0"
    class="absolute top-0 overflow-hidden rounded-md bg-neutral-900
         cursor-pointer select-none
         ring-offset-neutral-950
         hover:ring-2 hover:ring-neutral-500 hover:ring-offset-1
         focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-neutral-400"
    onclick={onClick}
    onkeydown={(e) => e.key === "Enter" && onClick?.()}
>
    <!-- Instant ThumbHash placeholder (paints on jump, before the thumb loads). -->
    {#if placeholder}
           <img
               src={placeholder}
               alt=""
               aria-hidden="true"
               class="absolute inset-0 w-full h-full object-cover"
           />
       {:else}
       <div class="absolute inset-0 bg-neutral-800"></div>
       {/if}

        <!-- Real thumbnail fades in over the placeholder once hydrated. -->
        {#if imgSrc}
                <img
                    use:fadeOnLoad
                    src={imgSrc}
                    alt={heavy?.filename ?? ""}
                    class="relative w-full h-full object-cover"
                    draggable="false"
                    decoding="async"
                />
            {/if}
</div>
