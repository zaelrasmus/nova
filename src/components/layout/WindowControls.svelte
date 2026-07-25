<script lang="ts">
    import { onMount } from "svelte";
    import { getCurrentWindow } from "@tauri-apps/api/window";

    /**
     * Minimize / maximize / close for a `decorations: false` window.
     *
     * LAYOUT: this is a FIXED overlay in the window's top-right corner, mounted as
     * a sibling of the three panes — never inside one. That's deliberate: the
     * inspector can be hidden, and a close button that disappears with a panel is
     * a trap. The panes only reserve space for it (`--wc-w`, see layout.css).
     *
     * Sizing follows Windows metrics: 46px wide buttons, full header height.
     *
     * DEFERRED: Windows 11 Snap Layouts (the flyout when you hover Maximize) needs
     * native hit-testing — WM_NCHITTEST returning HTMAXBUTTON over this button's
     * rect. That's a Rust-side change; nothing in this layout moves if we add it,
     * because the gutter is reserved by CSS either way.
     */

    let maximized = $state(false);

    // Deliberately NOT $state: the handle is a class instance we only ever call
    // methods on, and it's resolved in onMount because getCurrentWindow() touches
    // the Tauri IPC bridge.
    let win: ReturnType<typeof getCurrentWindow> | null = null;

    onMount(() => {
        const w = getCurrentWindow();
        win = w;
        void w.isMaximized().then((v) => (maximized = v));

        // Track the WINDOW, not our own clicks: Win+↑, double-clicking the drag
        // region, and Aero Snap all maximize without going through these buttons.
        const unlisten = w.onResized(async () => {
            maximized = await w.isMaximized();
        });
        return () => {
            void unlisten.then((fn) => fn());
        };
    });
</script>

<div class="fixed right-0 top-0 z-50 flex h-[var(--chrome-h)] items-stretch">
    <button
        type="button"
        aria-label="Minimize"
        onclick={() => win?.minimize()}
        class="grid w-[46px] place-items-center text-neutral-400 transition-colors
               hover:bg-white/10 hover:text-neutral-100"
    >
        <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor">
            <path d="M0 5h10" />
        </svg>
    </button>

    <button
        type="button"
        aria-label={maximized ? "Restore" : "Maximize"}
        onclick={() => win?.toggleMaximize()}
        class="grid w-[46px] place-items-center text-neutral-400 transition-colors
               hover:bg-white/10 hover:text-neutral-100"
    >
        <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor">
            {#if maximized}
                <!-- Restore: the classic two overlapping frames. -->
                <path d="M2.5 2.5V0.5h7v7h-2" />
                <rect x="0.5" y="2.5" width="7" height="7" />
            {:else}
                <rect x="0.5" y="0.5" width="9" height="9" />
            {/if}
        </svg>
    </button>

    <button
        type="button"
        aria-label="Close"
        onclick={() => win?.close()}
        class="grid w-[46px] place-items-center text-neutral-400 transition-colors
               hover:bg-[#e81123] hover:text-white"
    >
        <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor">
            <path d="M0 0l10 10M10 0L0 10" />
        </svg>
    </button>
</div>
