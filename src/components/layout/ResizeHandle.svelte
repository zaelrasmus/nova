<script lang="ts">
    import { layout } from "$lib/layout.svelte";

    /**
     * Drag handle on a pane's edge.
     *
     * Pointer events (not HTML5 drag), consistent with the rest of Nova's DnD —
     * and pointer capture means the drag survives the cursor leaving the 6px
     * strip, which it always does.
     *
     * The reported width assumes the pane is flush against a window edge, which
     * is true for both side panes: for the sidebar the width IS clientX; for the
     * inspector it's the distance from the right edge.
     */
    interface Props {
        /** Which window edge the pane is anchored to. */
        edge: "left" | "right";
        /** Current width in px — only needed so the keyboard nudge has a base. */
        value: number;
        label: string;
        onresize: (px: number) => void;
    }

    const { edge, value, label, onresize }: Props = $props();

    let dragging = $state(false);

    function widthFrom(clientX: number): number {
        return edge === "left" ? clientX : window.innerWidth - clientX;
    }

    function start(e: PointerEvent) {
        e.preventDefault();
        (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
        dragging = true;
        // Disables the grid-template-columns transition for the duration, so the
        // handle tracks the cursor exactly instead of easing behind it.
        layout.resizing = true;
    }

    function move(e: PointerEvent) {
        if (!dragging) return;
        onresize(widthFrom(e.clientX));
    }

    function end() {
        if (!dragging) return;
        dragging = false;
        layout.resizing = false;
        layout.persist(); // one disk write per drag, not per frame
    }
</script>

<!-- A focusable separator IS the ARIA "window splitter" pattern — arrow keys
     resize it, because a mouse-only handle is unreachable by keyboard. Svelte's
     rule only knows the non-focusable kind of separator, hence the ignore. -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div
    role="separator"
    aria-orientation="vertical"
    aria-label={label}
    aria-valuenow={Math.round(value)}
    tabindex="0"
    class="absolute inset-y-0 z-20 w-1.5 cursor-col-resize transition-colors
           hover:bg-blue-500/40 focus-visible:bg-blue-500/60 focus-visible:outline-none
           {edge === 'left' ? 'right-0' : 'left-0'}
           {dragging ? 'bg-blue-500/60' : ''}"
    onpointerdown={start}
    onpointermove={move}
    onpointerup={end}
    onpointercancel={end}
    onkeydown={(e) => {
        const step = e.shiftKey ? 32 : 8;
        if (e.key === "ArrowLeft") {
            onresize(edge === "left" ? value - step : value + step);
        } else if (e.key === "ArrowRight") {
            onresize(edge === "left" ? value + step : value - step);
        } else {
            return;
        }
        e.preventDefault();
        layout.persist();
    }}
></div>
