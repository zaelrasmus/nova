<!--
  ViewerOverlay — the lightbox (QuickLook and Fullscreen), rendered above the
  grid. Navigation state lives in `viewer.svelte.ts`; this is the presentation.

  Images load in three layers, cheapest first, so something is on screen
  immediately: ThumbHash blur → cached thumbnail → full-resolution original,
  which fades in once decoded.

  Video and audio hand off entirely to MediaPlayer; this file keeps only the
  shared chrome (title, counter, close, prev/next) and the keyboard split
  described above the key handler.

  PanZoom OWNS the image's inline `style` and writes the transform imperatively
  every frame. Never put a Svelte `style=` on that <img> — the two writers fight.
  Use classes instead, and note it also needs `max-width: none` to escape
  Tailwind Preflight.
-->
<script lang="ts">
    import { untrack } from "svelte";
    import { convertFileSrc } from "@tauri-apps/api/core";
    import { toast } from "svelte-sonner";
    import { assetLibrary, thumbHashUrl, type SubtreeSection } from "$lib/assets.svelte";
    import { viewer } from "$lib/viewer.svelte";
    import { PanZoom } from "$lib/panzoom.svelte";
    import { isRemote, remoteAssetUrl, keepRemoteOffline, setMediaDuration } from "$lib/remote";
    import MediaPlayer from "./MediaPlayer.svelte";
    import {
        settings,
        VIEWER_BACKGROUNDS,
        type ViewerBackground,
    } from "../routes/settings.svelte";

    const current = $derived(viewer.current);

    // Hydrate the heavy row (for dest_path / thumb_path) whenever the asset
    // changes. Cheap — usually already cached from the grid.
    $effect(() => {
        const id = current?.id;
        if (id) untrack(() => assetLibrary.ensure([id]));
    });

    const heavy = $derived(current ? assetLibrary.heavy.get(current.id) : undefined);

    // ── Reading position ───────────────────────────────────────────────────────────────────
    // "Ch 01 · 12 / 24" — a page's place in an order a PERSON chose. Same rule as
    // the grid's numbers: a folder, Manual sort, nothing narrowing the list.
    // Anywhere else the plain "12 / 240" counter stays, since an index into a
    // date sort or a filtered list isn't anyone's page number.
    const scope = $derived(assetLibrary.scope);
    const openFolderId = $derived(scope.kind === "folder" ? scope.id : null);
    const reading = $derived(
        openFolderId !== null &&
            assetLibrary.sort.order_by === "manual" &&
            !assetLibrary.hasFilters &&
            !assetLibrary.nameFiltering,
    );
    const folderName = (id: string) =>
        assetLibrary.folders.find((f) => f.id === id)?.name ?? "Folder";

    // A merged view is its subfolders' runs back to back, so a page's chapter
    // follows from its index. Refetched with the list it describes; a token
    // drops answers for a list we've already left.
    let sections = $state<SubtreeSection[] | null>(null);
    let sectionsToken = 0;
    $effect(() => {
        const id = openFolderId;
        const wanted = viewer.isOpen && reading && assetLibrary.includesSubfolders;
        void assetLibrary.manifest;
        void assetLibrary.membershipVersion;
        const mine = ++sectionsToken;
        if (!wanted || id === null) {
            sections = null;
            return;
        }
        assetLibrary
            .fetchSubtreeSections(id)
            .then((s) => {
                if (mine === sectionsToken) sections = s;
            })
            .catch(() => {
                if (mine === sectionsToken) sections = null;
            });
    });

    const place = $derived.by(() => {
        if (!reading || openFolderId === null || viewer.count === 0) return null;
        const n = viewer.count;
        // Place in MANUAL order: descending shows the list backwards, but page
        // 1 is still the first page, exactly as the grid numbers it.
        const m = assetLibrary.sort.is_ascending ? viewer.index : n - 1 - viewer.index;
        const whole = { name: folderName(openFolderId), at: m + 1, of: n };
        if (!assetLibrary.includesSubfolders || !sections) return whole;
        // Sections that don't tile this exact list (mid-reload) would name the
        // wrong chapter; the whole-list count is never wrong.
        if (sections.reduce((sum, s) => sum + s.count, 0) !== n) return whole;
        let start = 0;
        for (const s of sections) {
            if (m < start + s.count) return { name: folderName(s.folder_id), at: m - start + 1, of: s.count };
            start += s.count;
        }
        return whole;
    });

    // ── Next folder ──────────────────────────────────────────────────────────────────────
    // At the last item, the next sibling in sidebar order — the next chapter.
    // `folders` already arrives in sidebar order (parent, position, name, id),
    // so filtering to the siblings keeps it.
    // Opt-in (Settings › Display): off, the last page is simply the end.
    const nextFolder = $derived.by(() => {
        if (!settings.preferences.viewerNextFolder) return null;
        if (openFolderId === null || viewer.count === 0 || viewer.index !== viewer.count - 1) {
            return null;
        }
        const me = assetLibrary.folders.find((f) => f.id === openFolderId);
        if (!me) return null;
        const siblings = assetLibrary.folders.filter((f) => f.parent_id === me.parent_id);
        return siblings[siblings.findIndex((f) => f.id === me.id) + 1] ?? null;
    });

    // Open the next folder and keep reading. Timing can't tell us when the new
    // list is in: the old rows linger until `setScope` reaches its load, and
    // chunks can trail the call resolving. So the viewer closes, and reopens on
    // the first row that arrives FOR THAT FOLDER. A folder with nothing of its
    // own (a volume of chapters) simply leaves it closed — never "open" on
    // nothing while still owning the keyboard.
    //
    // A jump is one-shot: it is dropped as soon as the target finishes loading
    // empty, or the user ends up anywhere but the folder they left or the one
    // they're heading to — otherwise rows arriving later (an import, turning
    // on Subfolders) would pop the viewer open unasked.
    let jump = $state<{
        from: string;
        folderId: string;
        mode: Exclude<typeof viewer.mode, "closed">;
        reached: boolean;
    } | null>(null);
    async function openNextFolder() {
        const target = nextFolder;
        if (!target || viewer.mode === "closed" || openFolderId === null) return;
        jump = { from: openFolderId, folderId: target.id, mode: viewer.mode, reached: false };
        viewer.close();
        await assetLibrary.setScope({ kind: "folder", id: target.id });
        // Superseded by another navigation while the scope was resolving.
        if (jump?.folderId === target.id && openFolderId !== target.id) jump = null;
    }
    $effect(() => {
        const j = jump;
        if (j === null) return;
        if (openFolderId === j.folderId) {
            j.reached = true;
            if (viewer.count > 0) {
                jump = null;
                viewer.open(0, j.mode);
            } else if (!assetLibrary.isLoading) {
                jump = null; // loaded, and nothing of its own: stay closed
            }
        } else if (j.reached || openFolderId !== j.from) {
            jump = null; // the user went elsewhere
        }
    });

    // ── Preload ──────────────────────────────────────────────────────────────────────────
    // Decode the next two images at full resolution so turning the page is
    // instant. Local files only: an online asset would cost a download per
    // page you might never reach. `crossOrigin` matches the visible <img> — the
    // cache keys CORS and plain requests apart, so a mismatch fetches twice.
    // Holding the elements keeps their decoded bitmaps alive; replacing them
    // lets the previous pair go.
    const PRELOAD_AHEAD = 2;
    let preloaded: HTMLImageElement[] = [];
    let preloadKey = "";
    $effect(() => {
        if (!viewer.isOpen) {
            preloaded = [];
            preloadKey = "";
            return;
        }
        const ahead = viewer.list
            .slice(viewer.index + 1, viewer.index + 1 + PRELOAD_AHEAD)
            .filter((r) => r.asset_type === "image");
        if (ahead.length === 0) {
            preloaded = [];
            preloadKey = "";
            return;
        }
        untrack(() => assetLibrary.ensure(ahead.map((r) => r.id)));
        const urls: string[] = [];
        for (const r of ahead) {
            const h = assetLibrary.heavy.get(r.id); // re-runs once hydrated
            if (h && !isRemote(h) && h.dest_path) urls.push(convertFileSrc(h.dest_path));
        }
        const key = urls.join("\n");
        if (key === preloadKey) return;
        preloadKey = key;
        preloaded = urls.map((url) => {
            const img = new Image();
            img.crossOrigin = "anonymous";
            img.decoding = "async";
            img.src = url;
            img.decode().catch(() => {}); // a failed preload just means a normal load later
            return img;
        });
    });

    // Layered sources, cheapest-first: a ThumbHash blur (instant), the cached
    // thumbnail, then the full-resolution original. The full-res image carries
    // crossorigin="anonymous" so the V3 eyedropper can sample it without tainting
    // the canvas (proven in the V0 spike).
    const placeholder = $derived(thumbHashUrl(current?.thumb_hash ?? null));
    // The thumbnail is ALWAYS local, even for an online asset — that is what
    // keeps the grid and this overlay's first paint working with no network.
    const thumbUrl = $derived(heavy?.thumb_path ? convertFileSrc(heavy.thumb_path) : null);
    // Full resolution is the one thing that may not be on disk. For an online
    // asset it streams through the proxy instead; every consumer below is
    // unaware of which, which is the whole point of resolving it here.
    const fullUrl = $derived(
        !heavy
            ? null
            : isRemote(heavy)
              ? remoteAssetUrl(heavy.id)
              : heavy.dest_path
                ? convertFileSrc(heavy.dest_path)
                : null,
    );

    /** Bytes are not on disk — the viewer streams them through the proxy. */
    const onlineAsset = $derived(isRemote(heavy));

    let keepingOffline = $state(false);

    /** Download an online asset into the library. One-way: it becomes local. */
    async function keepOffline(id: string) {
        if (keepingOffline) return;
        keepingOffline = true;
        try {
            const updated = await keepRemoteOffline(id);
            // Patch the heavy row so the player's src flips to the local file and
            // the Keep-offline button removes itself, with no reload.
            assetLibrary.heavy.set(id, updated);
            toast.success("Saved to your library");
        } catch (e) {
            toast.error(typeof e === "string" ? e : "Couldn't download that file.");
        } finally {
            keepingOffline = false;
        }
    }

    /** Rust decodes no media, so the player is the only source of a duration. */
    function reportDuration(id: string, ms: number) {
        if (!Number.isFinite(ms) || ms <= 0) return;
        // Backend no-ops when it already has one, so a repeat costs one UPDATE.
        void setMediaDuration(id, ms).catch(() => {});
    }

    const isImage = $derived(current?.asset_type === "image");
    /** Video and audio share a player, and share its keyboard claim (see below). */
    const isMedia = $derived(current?.asset_type === "video" || current?.asset_type === "audio");
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
    // Every opening starts from the user's default (Settings › Display); `B`
    // is a temporary override for this viewing, not a new default.
    let bg = $state<ViewerBackground>(settings.preferences.viewerBackground);
    $effect(() => {
        if (viewer.isOpen) bg = untrack(() => settings.preferences.viewerBackground);
    });
    function cycleBg() {
        bg = VIEWER_BACKGROUNDS[(VIEWER_BACKGROUNDS.indexOf(bg) + 1) % VIEWER_BACKGROUNDS.length];
    }
    const BG_STYLE: Record<ViewerBackground, string> = {
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
    //
    // Media SHARES this keymap with MediaPlayer, which registers its own window
    // listener. The split: Space and the bare arrows mean play/pause and seek
    // when a clip is open — that expectation is too strong to override — so this
    // handler stands down on them and asset navigation moves to Shift+arrows.
    // Escape and F keep the same meaning for every asset type. Both listeners
    // fire on every key, so each side has to opt out explicitly.
    $effect(() => {
        if (!viewer.isOpen) return;
        const onKey = (e: KeyboardEvent) => {
            switch (e.key) {
                case "Escape":
                    e.preventDefault();
                    viewer.close();
                    break;
                case "ArrowRight":
                    if (isMedia && !e.shiftKey) return; // the player seeks instead
                    e.preventDefault();
                    // Past the last page, into the next chapter — but never on
                    // key repeat, or holding → to flip fast would carry you
                    // out of the folder.
                    if (nextFolder && !e.repeat) void openNextFolder();
                    else viewer.next();
                    break;
                case "ArrowLeft":
                    if (isMedia && !e.shiftKey) return;
                    e.preventDefault();
                    viewer.prev();
                    break;
                case " ":
                    if (isMedia) return; // play/pause
                    e.preventDefault();
                    viewer.toggleQuickLook();
                    break;
                case "f":
                case "F":
                    e.preventDefault();
                    viewer.toggleFullscreen();
                    break;
                // Everything below drives PanZoom, so it is image-only — and the
                // digits in particular have to reach the player, where they seek.
                case "1":
                    if (!isImage) return;
                    e.preventDefault();
                    pz?.actualSize();
                    break;
                case "2":
                case "0":
                    if (!isImage) return;
                    e.preventDefault();
                    pz?.fit();
                    break;
                case "+":
                case "=": // unshifted '+' on most layouts
                    if (!isImage) return;
                    e.preventDefault();
                    pz?.zoomIn();
                    break;
                case "-":
                    if (!isImage) return;
                    e.preventDefault();
                    pz?.zoomOut();
                    break;
                case "b":
                case "B":
                    if (!isImage) return;
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
        {:else if isMedia}
            {#if fullUrl}
                <!-- Keyed by asset id on purpose: moving to the next clip has to
                     build a NEW element and controller rather than re-point a
                     live one, which is what releases the previous file handle and
                     resets the transport state cleanly. -->
                {#key current.id}
                    <MediaPlayer
                        src={fullUrl}
                        kind={current.asset_type === "video" ? "video" : "audio"}
                        filename={current.filename}
                        extension={heavy?.extension}
                        fileSize={heavy?.file_size}
                        poster={thumbUrl}
                        destPath={onlineAsset ? null : (heavy?.dest_path ?? null)}
                        remote={onlineAsset}
                        seekable={!onlineAsset || heavy?.supports_range !== false}
                        onduration={(ms) => reportDuration(current.id, ms)}
                        onKeepOffline={onlineAsset ? () => keepOffline(current.id) : undefined}
                        {keepingOffline}
                        {fullscreen}
                        onToggleFullscreen={() => viewer.toggleFullscreen()}
                        onBackdropClick={() => viewer.close()}
                    />
                {/key}
            {:else}
                <!-- Heavy row still hydrating; it usually arrives in the same
                     frame, so anything louder than this would just flash. -->
                <div class="flex h-full w-full items-center justify-center text-xs text-neutral-600">
                    Loading…
                </div>
            {/if}
        {:else}
            <div class="flex h-full w-full flex-col items-center justify-center gap-3 text-neutral-400">
                <span class="text-5xl">📄</span>
                <span class="text-sm">{current.filename}</span>
                <span class="text-xs text-neutral-600">No preview for this file type.</span>
            </div>
        {/if}

        <!-- ── Chrome ─────────────────────────────────────────────────────── -->

        <div
            class="pointer-events-none absolute left-4 top-3 flex items-center gap-2 text-xs text-white/70"
        >
            <span class="max-w-[40vw] truncate">{current.filename}</span>
            {#if place}
                <span class="max-w-[25vw] truncate text-white/60">{place.name}</span>
                <span class="tabular-nums text-white/40">· {place.at} / {place.of}</span>
            {:else}
                <span class="tabular-nums text-white/40">{viewer.index + 1} / {viewer.count}</span>
            {/if}
        </div>

        {#if nextFolder}
            <!-- Last page: carry on into the next chapter (also →). -->
            <button
                type="button"
                onclick={openNextFolder}
                title="Open the next folder (→)"
                class="absolute bottom-16 right-4 z-10 max-w-[40vw] truncate rounded-full bg-white/15 px-3 py-1.5
                       text-xs font-medium text-white shadow-lg ring-1 ring-white/20 hover:bg-white/25"
            >
                Next: {nextFolder.name} →
            </button>
        {/if}

        <div class="absolute right-3 top-3 flex items-center gap-1">
            <!-- Media carries its own fullscreen button in the player bar; two
                 controls for one action in the same view reads as a bug. -->
            {#if !isMedia}
                <button
                    type="button"
                    onclick={() => viewer.toggleFullscreen()}
                    title={fullscreen ? "Exit fullscreen (F)" : "Fullscreen (F)"}
                    class="rounded-md bg-white/10 px-2 py-1 text-xs text-white hover:bg-white/20"
                >
                    {fullscreen ? "⤢ Exit" : "⤢ Fullscreen"}
                </button>
            {/if}
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
                title={isMedia ? "Previous (Shift+←)" : "Previous (←)"}
                class="absolute left-3 top-1/2 -translate-y-1/2 rounded-full bg-white/10 px-3 py-3
                       text-lg text-white hover:bg-white/20 disabled:opacity-20"
                aria-label="Previous">‹</button
            >
            <button
                type="button"
                onclick={() => viewer.next()}
                disabled={viewer.index === viewer.count - 1}
                title={isMedia ? "Next (Shift+→)" : "Next (→)"}
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
