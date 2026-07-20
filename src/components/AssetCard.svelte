<script lang="ts">
    import { convertFileSrc } from "@tauri-apps/api/core";

    interface AssetMetadata {
        id: string;
        asset_type: "image" | "audio" | "video" | "unknown";
        filename: string;
        extension: string;
        dest_path: string;
        imported_date: string;
        creation_date: string;
        modified_date: string;

        // TODO: Add to the SELECT query in assets.rs once ready.
        // Required for accurate masonry height estimation in AssetGrid.
        width?: number;
        height?: number;
    }

    interface Props {
        asset: AssetMetadata;
        style: string;
        onClick?: () => void;
    }

    let { asset, style, onClick }: Props = $props();

    // ── Fade-in action ────────────────────────────────────────────────────────
    // Svelte `use:` action — runs once when the img node mounts.
    // Sets opacity 0 → 1 via CSS transition once the image bytes have loaded.
    // No library needed: the virtualizer guarantees the node only exists in
    // the DOM when it's in the viewport, so "mounted = visible".
    function fadeOnLoad(node: HTMLImageElement) {
        node.style.opacity = "0";
        node.style.transition = "opacity 200ms ease";

        const onLoad = () => (node.style.opacity = "1");
        const onError = () => (node.style.opacity = "0.3"); // visible but clearly broken

        node.addEventListener("load", onLoad, { once: true });
        node.addEventListener("error", onError, { once: true });

        // If the browser already cached the image, `load` won't fire.
        if (node.complete && node.naturalWidth > 0) {
            node.style.opacity = "1";
        }

        return {
            destroy() {
                node.removeEventListener("load", onLoad);
                node.removeEventListener("error", onError);
            },
        };
    }
</script>

<!--
  position:absolute + transform:translateY is TanStack Virtual's standard
  positioning pattern. Do not change it — the virtualizer owns these values.
-->
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
    <img
        src={convertFileSrc(asset.dest_path)}
        alt={asset.filename}
        class="w-full h-full object-cover"
        draggable="false"
    />

    <!-- ── FUTURE: Pragmatic Drag and Drop (Atlassian) ───────────────────────
    When implementing reordering, wrap this card with a draggable() from
    @atlaskit/pragmatic-drag-and-drop and add the dropTarget() to the grid
    container. The asset id is the drag data payload.

    import { draggable } from "@atlaskit/pragmatic-drag-and-drop/element/adapter";

    function makeDraggable(node: HTMLElement) {
      return draggable({
        element: node,
        getInitialData: () => ({ assetId: asset.id }),
      });
    }
  ────────────────────────────────────────────────────────────────────────── -->
</div>
