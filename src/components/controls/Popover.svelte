<!--
  Popover — a panel anchored under a trigger, for the toolbar menus (Sort,
  Display, Quick actions) and the inspector's "Add to folder".

  The panel is `position: fixed` at coordinates measured from the trigger, not
  `absolute` inside it. Every pane header is `overflow: hidden` (so a crowded
  toolbar shrinks instead of spilling), and an absolute panel inside one is
  clipped to the header's 48px strip.

  Dismissal matches the context menus: a full-window click-away catcher, Escape,
  and any choice that calls the `close` the content snippet receives.
-->
<script lang="ts">
    import type { Snippet } from "svelte";

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

    let anchor = $state<HTMLDivElement | null>(null);
    let pos = $state({ top: 0, left: 0, right: 0 });

    function place() {
        if (!anchor) return;
        const r = anchor.getBoundingClientRect();
        pos = { top: r.bottom + 6, left: r.left, right: window.innerWidth - r.right };
    }

    const close = () => (open = false);
    const toggle = () => {
        if (!open) place();
        open = !open;
    };

    function onKey(e: KeyboardEvent) {
        if (open && e.key === "Escape") {
            e.preventDefault();
            close();
        }
    }
</script>

<svelte:window onkeydown={onKey} onresize={() => open && place()} />

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
