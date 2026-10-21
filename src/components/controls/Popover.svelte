<!--
  Popover — a panel anchored under a trigger, for the toolbar menus (Sort,
  Display, Quick actions) and the inspector's "Add to folder".

  The panel is `position: fixed` at coordinates measured from the trigger, not
  `absolute` inside it. Every pane header is `overflow: hidden` (so a crowded
  toolbar shrinks instead of spilling), and an absolute panel inside one is
  clipped to the header's 48px strip.

  Dismissal matches the context menus: a full-window click-away catcher, Escape,
  and any choice that calls the `close` the content snippet receives.

  Escape is caught in the CAPTURE phase and stopped there. The grid listens for
  Escape on the window to clear the selection, and closing a menu must not also
  throw away the selection the menu was about to act on.
-->
<script lang="ts">
    import { tick, type Snippet } from "svelte";

    interface Props {
        open?: boolean;
        /** Accessible name for the panel. */
        label: string;
        /** Which trigger edge the panel lines up with. */
        align?: "start" | "end";
        /** Tailwind width class for the panel. */
        width?: string;
        trigger: Snippet<[{ open: boolean; toggle: () => void }]>;
        children: Snippet<[{ close: () => void }]>;
    }
    let {
        open = $bindable(false),
        label,
        align = "end",
        width = "w-60",
        trigger,
        children,
    }: Props = $props();

    const EDGE = 8; // px kept clear of the window edges

    let anchor = $state<HTMLDivElement | null>(null);
    let panel = $state<HTMLDivElement | null>(null);
    let pos = $state({ top: 0, left: 0, right: 0 });

    /**
     * Under the trigger, kept inside the window: shifted sideways when it would
     * run past an edge, and flipped above the trigger when there's more room
     * there than below. Runs again once the panel has rendered, because only
     * then is its size known.
     */
    function place() {
        if (!anchor) return;
        const r = anchor.getBoundingClientRect();
        const w = panel?.offsetWidth ?? 0;
        const h = panel?.offsetHeight ?? 0;
        const vw = window.innerWidth;
        const vh = window.innerHeight;

        const below = r.bottom + 6;
        const above = r.top - 6 - h;
        const top = below + h > vh - EDGE && above >= EDGE ? above : below;

        const left = Math.max(EDGE, Math.min(r.left, vw - w - EDGE));
        const right = Math.max(EDGE, Math.min(vw - r.right, vw - w - EDGE));
        pos = { top, left, right };
    }

    const close = () => (open = false);
    const toggle = () => {
        if (!open) {
            place();
            void tick().then(place);
        }
        open = !open;
    };

    // Content can change size while open (a membership toggle reflows chips),
    // so keep the panel in place as it does.
    $effect(() => {
        if (!open || !panel) return;
        const observer = new ResizeObserver(() => place());
        observer.observe(panel);
        if (anchor) observer.observe(anchor);
        return () => observer.disconnect();
    });

    $effect(() => {
        if (!open) return;
        const onKey = (e: KeyboardEvent) => {
            if (e.key !== "Escape") return;
            e.preventDefault();
            e.stopPropagation();
            close();
        };
        window.addEventListener("keydown", onKey, true);
        return () => window.removeEventListener("keydown", onKey, true);
    });
</script>

<svelte:window onresize={() => open && place()} />

<div bind:this={anchor} class="relative shrink-0">
    {@render trigger({ open, toggle })}
</div>

{#if open}
    <button
        type="button"
        tabindex="-1"
        aria-label="Close {label}"
        class="fixed inset-0 z-[90] cursor-default"
        onclick={close}
    ></button>
    <div
        bind:this={panel}
        role="dialog"
        aria-label={label}
        style:top="{pos.top}px"
        style:left={align === "start" ? `${pos.left}px` : null}
        style:right={align === "end" ? `${pos.right}px` : null}
        class="nv-menu fixed z-[91] {width}"
    >
        {@render children({ close })}
    </div>
{/if}
