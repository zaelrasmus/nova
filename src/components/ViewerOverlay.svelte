<script lang="ts">
    import { untrack } from "svelte";
    import { convertFileSrc } from "@tauri-apps/api/core";
    import { toast } from "svelte-sonner";
    import { assetLibrary, thumbHashUrl } from "$lib/assets.svelte";
    import { viewer } from "$lib/viewer.svelte";
    import { PanZoom } from "$lib/panzoom.svelte";

    const current = $derived(viewer.current);

    // Hydrate the heavy row (for dest_path / thumb_path) whenever the asset
    // changes. Cheap — usually already cached from the grid.
    $effect(() => {
        const id = current?.id;
        if (id) untrack(() => assetLibrary.ensure([id]));
    });

    const heavy = $derived(current ? assetLibrary.heavy.get(current.id) : undefined);

    // Layered sources, cheapest-first: a ThumbHash blur (instant), the cached
    // thumbnail, then the full-resolution original. The full-res image carries
    // crossorigin="anonymous" so the V3 eyedropper can sample it without tainting
    // the canvas (proven in the V0 spike).
    const placeholder = $derived(thumbHashUrl(current?.thumb_hash ?? null));
    const thumbUrl = $derived(heavy?.thumb_path ? convertFileSrc(heavy.thumb_path) : null);
    const fullUrl = $derived(heavy?.dest_path ? convertFileSrc(heavy.dest_path) : null);

    const isImage = $derived(current?.asset_type === "image");
    const fullscreen = $derived(viewer.mode === "fullscreen");

    // Fade the full-res image in once it decodes, so switching assets doesn't
    // flash the low-res thumbnail underneath.
    let fullLoaded = $state(false);
    $effect(() => {
        void fullUrl;
        fullLoaded = false;
    });

    // Nearest-neighbour toggle for inspecting pixel art / icons without blur.
    let pixelated = $state(false);

    // ── Background modes (B) ─────────────────────────────────────────────────
    // For judging transparent PNG/SVG/WebP against different backdrops.
    const BG_MODES = ["checker", "black", "white", "gray"] as const;
    type BgMode = (typeof BG_MODES)[number];
    let bg = $state<BgMode>("checker");
    function cycleBg() {
        bg = BG_MODES[(BG_MODES.indexOf(bg) + 1) % BG_MODES.length];
    }
    const BG_STYLE: Record<BgMode, string> = {
        // A CSS checkerboard — no image asset needed.
        checker:
            "background-color:#808080;background-image:" +
            "linear-gradient(45deg,#6b6b6b 25%,transparent 25%)," +
            "linear-gradient(-45deg,#6b6b6b 25%,transparent 25%)," +
            "linear-gradient(45deg,transparent 75%,#6b6b6b 75%)," +
            "linear-gradient(-45deg,transparent 75%,#6b6b6b 75%);" +
            "background-size:20px 20px;" +
            "background-position:0 0,0 10px,10px -10px,-10px 0;",
        black: "background-color:#000;",
        white: "background-color:#fff;",
        gray: "background-color:#1f1f1f;",
    };

    // ── Eyedropper ─────────────────────────────────────────────────────────────
    // A toggle tool (so click-to-copy doesn't fight click-to-close/pan). While
    // active: the HUD tracks the pixel under the cursor and a click copies its
    // HEX. Reads via a reused 1×1 canvas + the crossorigin image (V0 proved this
    // is taint-free), so a 50MP image costs nothing to sample.
    let eyedropper = $state(false);
    let eye = $state<{ hex: string; rgb: string; x: number; y: number } | null>(null);
    let sampleCanvas: HTMLCanvasElement | null = null;
    let sampleCtx: CanvasRenderingContext2D | null = null;
    let eyeRaf = 0;

    function ensureCanvas(): CanvasRenderingContext2D | null {
        if (!sampleCtx) {
            sampleCanvas = document.createElement("canvas");
            sampleCanvas.width = 1;
            sampleCanvas.height = 1;
            sampleCtx = sampleCanvas.getContext("2d", { willReadFrequently: true });
        }
        return sampleCtx;
    }

    const hex2 = (n: number) => n.toString(16).padStart(2, "0");

    /** Sample the pixel under a client-space point. rAF-throttled by the caller. */
    function sampleAt(clientX: number, clientY: number) {
        const img = imgEl;
        const stage = stageEl;
        if (!img || !stage || !pz) return;
        const r = stage.getBoundingClientRect();
        const px = pz.toImagePixel(clientX - r.left, clientY - r.top);
        if (!px) {
            eye = null;
            return;
        }
        const ctx = ensureCanvas();
        if (!ctx) return;
        try {
            ctx.drawImage(img, px.x, px.y, 1, 1, 0, 0, 1, 1);
            const [red, green, blue] = ctx.getImageData(0, 0, 1, 1).data;
            eye = {
                hex: `#${hex2(red)}${hex2(green)}${hex2(blue)}`.toUpperCase(),
                rgb: `${red}, ${green}, ${blue}`,
                x: clientX,
                y: clientY,
            };
        } catch {
            eye = null; // shouldn't happen (crossorigin), but never throw on move
        }
    }

    function onStageMove(e: PointerEvent) {
        if (!eyedropper) return;
        // One sample per frame, max — mousemove fires far faster than we need.
        cancelAnimationFrame(eyeRaf);
        const { clientX, clientY } = e;
        eyeRaf = requestAnimationFrame(() => sampleAt(clientX, clientY));
    }

    async function copyEye() {
        if (!eye) return;
        try {
            await navigator.clipboard.writeText(eye.hex);
            toast.success(`Copied ${eye.hex}`);
        } catch {
            toast.error("Couldn't copy to clipboard.");
        }
    }

    // Turning the tool off clears the HUD.
    $effect(() => {
        if (!eyedropper) eye = null;
    });

    // ── Pan / zoom ─────────────────────────────────────────────────────────────
    let stageEl = $state<HTMLDivElement | null>(null);
    let imgEl = $state<HTMLImageElement | null>(null);
    let pz = $state<PanZoom | null>(null);

    // One controller for the lifetime of the image element; it re-fits itself on
    // each new source via the img `load` event, so navigation just works.
    $effect(() => {
        const stage = stageEl;
        const img = imgEl;
        if (!stage || !img) {
            pz = null;
            return;
        }
        const instance = new PanZoom(stage, img, { min: 0.1, max: 32 });
        pz = instance;
        return () => instance.destroy();
    });

    // Keyboard: active only while open, torn down with the overlay.
    $effect(() => {
        if (!viewer.isOpen) return;
        const onKey = (e: KeyboardEvent) => {
            switch (e.key) {
                case "Escape":
                    e.preventDefault();
                    viewer.close();
                    break;
                case "ArrowRight":
                    e.preventDefault();
                    viewer.next();
                    break;
                case "ArrowLeft":
                    e.preventDefault();
                    viewer.prev();
                    break;
                case " ":
                    e.preventDefault();
                    viewer.toggleQuickLook();
                    break;
                case "f":
                case "F":
                    e.preventDefault();
                    viewer.toggleFullscreen();
                    break;
                case "1":
                    e.preventDefault();
                    pz?.actualSize();
                    break;
                case "2":
                case "0":
                    e.preventDefault();
                    pz?.fit();
                    break;
                case "+":
                case "=": // unshifted '+' on most layouts
                    e.preventDefault();
                    pz?.zoomIn();
                    break;
                case "-":
                    e.preventDefault();
                    pz?.zoomOut();
                    break;
                case "b":
                case "B":
                    e.preventDefault();
                    cycleBg();
                    break;
            }
        };
        window.addEventListener("keydown", onKey);
        return () => window.removeEventListener("keydown", onKey);
    });
</script>

{#if viewer.isOpen && current}
    <!-- QuickLook is absolute → stays within the grid column. Fullscreen is fixed
         → covers the window.

         The two need DIFFERENT depths, and the reason is the point of QuickLook:
         it stays inside the grid so the sidebar and inspector remain usable, so a
         sidebar flyout (z-85) has to sit ON TOP of it. Fullscreen is the opposite
         — it's modal, and covers everything including the menus.

         App depth scale: 60 QuickLook · 85 rail flyouts · 90 context menus ·
         100 fullscreen viewer · 110 drag previews. -->
    <div
        class={fullscreen
            ? "fixed inset-0 z-[100] bg-black"
            : "absolute inset-0 z-[60] bg-black/90"}
    >
        {#if isImage}
            <!-- Pan/zoom stage. Click the empty area (not the image) to close;
                 a drag pans instead of closing. -->
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <!-- svelte-ignore a11y_click_events_have_key_events -->
            <div
                bind:this={stageEl}
                onpointermove={onStageMove}
                onclick={(e) => {
                    // Eyedropper: a click on the image copies the sampled colour.
                    if (eyedropper && !pz?.didDrag && e.target === imgEl) {
                        copyEye();
                        return;
                    }
                    // Otherwise a click on the empty backdrop closes.
                    if (e.target === stageEl && !pz?.didDrag) viewer.close();
                }}
                style={BG_STYLE[bg]}
                class="absolute inset-0 overflow-hidden [touch-action:none]
                       {eyedropper ? 'cursor-crosshair' : 'cursor-grab'}"
            >
                <!-- Loading layers: centred, NOT pan/zoomed — they only bridge the
                     gap until the full-res image decodes, then fade out. -->
                {#if !fullLoaded}
                    {#if placeholder}
                        <img
                            src={placeholder}
                            alt=""
                            aria-hidden="true"
                            class="pointer-events-none absolute inset-0 m-auto max-h-full max-w-full
                                   object-contain blur-lg"
                        />
                    {/if}
                    {#if thumbUrl}
                        <img
                            src={thumbUrl}
                            alt=""
                            aria-hidden="true"
                            class="pointer-events-none absolute inset-0 m-auto max-h-full max-w-full
                                   object-contain"
                        />
                    {/if}
                {/if}

                {#if fullUrl}
                    <!-- No `style=` binding here: PanZoom owns this element's
                         inline style (position/size/transform), and a reactive
                         style attribute would WIPE all of it on every update.
                         The fade + pixelation ride on CLASSES instead, which
                         Svelte writes to a separate attribute. -->
                    <img
                        bind:this={imgEl}
                        src={fullUrl}
                        alt={current.filename}
                        crossorigin="anonymous"
                        draggable="false"
                        onload={() => (fullLoaded = true)}
                        class="cursor-grab select-none transition-opacity duration-150
                               {fullLoaded ? 'opacity-100' : 'opacity-0'}
                               {pixelated ? '[image-rendering:pixelated]' : ''}"
                    />
                {/if}
            </div>
        {:else}
            <div class="flex h-full w-full flex-col items-center justify-center gap-3 text-neutral-400">
                <span class="text-5xl"
                    >{current.asset_type === "video"
                        ? "🎬"
                        : current.asset_type === "audio"
                          ? "🎵"
                          : "📄"}</span
                >
                <span class="text-sm">{current.filename}</span>
                <span class="text-xs text-neutral-600">Preview for this type is coming later.</span>
            </div>
        {/if}

        <!-- ── Chrome ─────────────────────────────────────────────────────── -->

        <div
            class="pointer-events-none absolute left-4 top-3 flex items-center gap-2 text-xs text-white/70"
        >
            <span class="max-w-[40vw] truncate">{current.filename}</span>
            <span class="text-white/40">{viewer.index + 1} / {viewer.count}</span>
        </div>

        <div class="absolute right-3 top-3 flex items-center gap-1">
            <button
                type="button"
                onclick={() => viewer.toggleFullscreen()}
                title={fullscreen ? "Exit fullscreen (F)" : "Fullscreen (F)"}
                class="rounded-md bg-white/10 px-2 py-1 text-xs text-white hover:bg-white/20"
            >
                {fullscreen ? "⤢ Exit" : "⤢ Fullscreen"}
            </button>
            <button
                type="button"
                onclick={() => viewer.close()}
                title="Close (Esc)"
                class="rounded-md bg-white/10 px-2 py-1 text-sm text-white hover:bg-white/20">✕</button
            >
        </div>

        {#if viewer.count > 1}
            <button
                type="button"
                onclick={() => viewer.prev()}
                disabled={viewer.index === 0}
                title="Previous (←)"
                class="absolute left-3 top-1/2 -translate-y-1/2 rounded-full bg-white/10 px-3 py-3
                       text-lg text-white hover:bg-white/20 disabled:opacity-20"
                aria-label="Previous">‹</button
            >
            <button
                type="button"
                onclick={() => viewer.next()}
                disabled={viewer.index === viewer.count - 1}
                title="Next (→)"
                class="absolute right-3 top-1/2 -translate-y-1/2 rounded-full bg-white/10 px-3 py-3
                       text-lg text-white hover:bg-white/20 disabled:opacity-20"
                aria-label="Next">›</button
            >
        {/if}

        <!-- Float toolbar: zoom readout + controls. Images only. -->
        {#if isImage && pz}
            <div
                class="absolute bottom-4 left-1/2 flex -translate-x-1/2 items-center gap-1 rounded-lg
                       bg-neutral-900/90 px-1.5 py-1 text-xs text-white shadow-xl ring-1 ring-white/10"
            >
                <button
                    type="button"
                    onclick={() => pz?.zoomOut()}
                    title="Zoom out (−)"
                    class="rounded px-2 py-1 hover:bg-white/10">−</button
                >
                <span class="w-12 text-center tabular-nums text-white/80">{pz.pct}%</span>
                <button
                    type="button"
                    onclick={() => pz?.zoomIn()}
                    title="Zoom in (+)"
                    class="rounded px-2 py-1 hover:bg-white/10">+</button
                >
                <div class="mx-0.5 h-4 w-px bg-white/15"></div>
                <button
                    type="button"
                    onclick={() => pz?.actualSize()}
                    title="Actual size (1)"
                    class="rounded px-2 py-1 hover:bg-white/10">1:1</button
                >
                <button
                    type="button"
                    onclick={() => pz?.fit()}
                    title="Fit to screen (2 / 0)"
                    class="rounded px-2 py-1 hover:bg-white/10 {pz.fitted ? 'bg-white/15' : ''}"
                    >Fit</button
                >
                <div class="mx-0.5 h-4 w-px bg-white/15"></div>
                <button
                    type="button"
                    onclick={() => (pixelated = !pixelated)}
                    title="Pixel-perfect (nearest neighbour)"
                    aria-pressed={pixelated}
                    class="rounded px-2 py-1 hover:bg-white/10 {pixelated ? 'bg-white/15' : ''}"
                    >⊞</button
                >
                <button
                    type="button"
                    onclick={() => (eyedropper = !eyedropper)}
                    title="Color eyedropper — click a pixel to copy its HEX"
                    aria-pressed={eyedropper}
                    class="rounded px-2 py-1 hover:bg-white/10 {eyedropper ? 'bg-white/15' : ''}"
                    >🎨</button
                >
                <button
                    type="button"
                    onclick={cycleBg}
                    title="Background (B)"
                    class="rounded px-2 py-1 capitalize hover:bg-white/10">{bg}</button
                >
            </div>
        {/if}

        <!-- Eyedropper HUD — follows the cursor while the tool is active. -->
        {#if eyedropper && eye}
            <div
                class="pointer-events-none fixed z-[90] flex items-center gap-2 rounded-md
                       bg-neutral-900/95 px-2 py-1 text-xs text-white shadow-xl ring-1 ring-white/10"
                style="left:{eye.x + 16}px; top:{eye.y + 16}px"
            >
                <span
                    class="h-5 w-5 shrink-0 rounded ring-1 ring-white/20"
                    style="background:{eye.hex}"
                ></span>
                <div class="leading-tight">
                    <div class="font-mono tabular-nums">{eye.hex}</div>
                    <div class="font-mono text-[10px] text-white/50">rgb({eye.rgb})</div>
                </div>
            </div>
        {/if}
    </div>
{/if}
