<script lang="ts">
    import { convertFileSrc } from "@tauri-apps/api/core";
    import type { AssetMetadata } from "$lib/assets.svelte";

    interface Props {
        heavy?: AssetMetadata;
        style: string;
        onClick?: () => void;
    }

    let { heavy, style, onClick }: Props = $props();


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
    {#if heavy}
           <img
               use:fadeOnLoad
               src={convertFileSrc(heavy.dest_path)}
               alt={heavy.filename}
               class="w-full h-full object-cover"
               draggable="false"
               decoding="async"
           />
       {:else}
           <!-- Placeholder until the heavy row hydrates. BlurHash lands here in. -->
           <div class="w-full h-full bg-neutral-800 animate-pulse"></div>
       {/if}
</div>
