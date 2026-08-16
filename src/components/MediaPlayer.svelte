<!--
  MediaPlayer — video and audio playback inside the viewer.

  The decoder is the system webview's; everything you see is ours. Dropping the
  `controls` attribute turns <video> into a bare pixel surface, and the chrome
  below drives it through `MediaController` (media.svelte.ts). That split is why
  the player can match the app instead of looking like a browser default.

  Two presentations, one control bar:
    · video — full-bleed frame, chrome floating over it, auto-hiding while playing
    · audio — a centred "now playing" card whose waveform IS the scrubber

  Unlike the image path there is NO PanZoom here, so nothing else is writing to
  the element's inline style and the usual "never bind style=" rule doesn't
  apply to this component.
-->
<script lang="ts">
    import { untrack } from "svelte";
    import { revealItemInDir } from "@tauri-apps/plugin-opener";
    import {
        Play,
        Pause,
        Volume2,
        Volume1,
        VolumeX,
        Repeat,
        Maximize2,
        Minimize2,
        LoaderCircle,
        TriangleAlert,
        Music,
        FolderOpen,
    } from "@lucide/svelte";
    import { MediaController, computeWaveform, SEEK_STEP_S, SEEK_JUMP_S } from "$lib/media.svelte";
    import { formatDuration, formatBytes } from "$lib/format";
    import { settings } from "../routes/settings.svelte";

    interface Props {
        /** Asset-protocol URL of the original file. */
        src: string;
        kind: "video" | "audio";
        filename: string;
        extension?: string;
        fileSize?: number;
        /** Shown behind a video until its first frame decodes. */
        poster?: string | null;
        /** Real path on disk, for the "Show in Explorer" escape hatch. */
        destPath?: string | null;
        fullscreen: boolean;
        onToggleFullscreen: () => void;
        /** Click on the empty area around the media. */
        onBackdropClick?: () => void;
    }

    let {
        src,
        kind,
        filename,
        extension,
        fileSize,
        poster = null,
        destPath = null,
        fullscreen,
        onToggleFullscreen,
        onBackdropClick,
    }: Props = $props();

    // ── Controller lifecycle ──────────────────────────────────────────────────
    // One controller per element. The parent keys this component by asset id, so
    // "next asset" remounts rather than mutating a live element — which keeps the
    // controller's state machine honest and drops the previous file handle.

    let el = $state<HTMLMediaElement | null>(null);
    let ctrl = $state<MediaController | null>(null);

    $effect(() => {
        const node = el;
        if (!node) {
            ctrl = null;
            return;
        }
        // untrack: the constructor READS the volume preferences, and persisting a
        // volume change writes them. Tracked, that round trip would tear down and
        // rebuild the player on every drag of the volume slider.
        const instance = untrack(
            () =>
                new MediaController(node, {
                    volume: settings.preferences.mediaVolume,
                    muted: settings.preferences.mediaMuted,
                    autoplay: true,
                    onVolumeChange: persistVolume,
                }),
        );
        ctrl = instance;
        return () => instance.destroy();
    });

    // Volume fires continuously while the slider moves; the disk write doesn't
    // need to. Same "one write per gesture" rule the pane resizers follow.
    let volumeTimer: ReturnType<typeof setTimeout> | undefined;
    function persistVolume(volume: number, muted: boolean) {
        clearTimeout(volumeTimer);
        volumeTimer = setTimeout(() => {
            if (settings.preferences.mediaVolume !== volume) void settings.set("mediaVolume", volume);
            if (settings.preferences.mediaMuted !== muted) void settings.set("mediaMuted", muted);
        }, 250);
    }

    // ── Waveform (audio only) ─────────────────────────────────────────────────
    // Decoded from its own copy of the file, off the playback path entirely —
    // see the note on `computeWaveform`. null means "couldn't", and the card
    // quietly falls back to the same scrubber the video uses.

    let peaks = $state<Float32Array | null>(null);

    $effect(() => {
        if (kind !== "audio") return;
        const url = src;
        let cancelled = false;
        void computeWaveform(url).then((result) => {
            if (!cancelled) peaks = result;
        });
        return () => {
            cancelled = true;
        };
    });

    let waveWidth = $state(0);
    let baseCanvas = $state<HTMLCanvasElement | null>(null);
    let playedCanvas = $state<HTMLCanvasElement | null>(null);

    const WAVE_HEIGHT = 56;
    const BAR_W = 2;
    const BAR_GAP = 1;

    /** Paint one pass of the waveform. Called twice: once dim, once in accent. */
    function drawWave(canvas: HTMLCanvasElement | null, data: Float32Array, w: number, fill: string) {
        if (!canvas || w <= 0) return;
        const dpr = window.devicePixelRatio || 1;
        canvas.width = Math.floor(w * dpr);
        canvas.height = Math.floor(WAVE_HEIGHT * dpr);
        const ctx = canvas.getContext("2d");
        if (!ctx) return;
        ctx.scale(dpr, dpr);
        ctx.clearRect(0, 0, w, WAVE_HEIGHT);
        ctx.fillStyle = fill;

        const step = BAR_W + BAR_GAP;
        const bars = Math.max(1, Math.floor(w / step));
        const mid = WAVE_HEIGHT / 2;
        for (let i = 0; i < bars; i++) {
            // The peak array is a fixed length; resample it onto however many
            // bars actually fit, so the shape survives any card width.
            const peak = data[Math.floor((i / bars) * data.length)] ?? 0;
            // A floor of 1px keeps silence visible as a centre line rather than
            // a gap that reads as "the waveform failed to load".
            const h = Math.max(1, peak * (WAVE_HEIGHT - 4));
            ctx.fillRect(i * step, mid - h / 2, BAR_W, h);
        }
    }

    $effect(() => {
        const data = peaks;
        const w = waveWidth;
        if (!data || w <= 0) return;
        drawWave(baseCanvas, data, w, "rgba(255,255,255,0.22)");
        drawWave(playedCanvas, data, w, "#3b82f6"); // blue-500, the app's accent
    });

    // ── Scrubbing ─────────────────────────────────────────────────────────────
    // Shared by the video scrubber and the audio waveform: both are just a
    // horizontal strip where x maps to a fraction of the duration.

    let scrubbing = $state(false);
    let scrubFraction = $state(0);
    let hoverFraction = $state<number | null>(null);
    let seekRaf = 0;

    const duration = $derived(ctrl?.duration ?? 0);
    const progress = $derived(
        scrubbing ? scrubFraction : duration > 0 ? (ctrl?.currentTime ?? 0) / duration : 0,
    );
    const bufferedPct = $derived(duration > 0 ? ((ctrl?.bufferedTo ?? 0) / duration) * 100 : 0);
    /** What the clock shows: the scrub target while dragging, else the playhead. */
    const displayTime = $derived(scrubbing ? scrubFraction * duration : (ctrl?.currentTime ?? 0));

    function fractionAt(e: PointerEvent): number {
        const node = e.currentTarget as HTMLElement;
        const r = node.getBoundingClientRect();
        if (r.width <= 0) return 0;
        return Math.min(Math.max((e.clientX - r.left) / r.width, 0), 1);
    }

    function onScrubDown(e: PointerEvent) {
        if (duration <= 0) return;
        e.stopPropagation();
        (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
        scrubbing = true;
        scrubFraction = fractionAt(e);
        ctrl?.seekToFraction(scrubFraction);
        bumpChrome();
    }

    function onScrubMove(e: PointerEvent) {
        hoverFraction = fractionAt(e);
        if (!scrubbing) return;
        scrubFraction = hoverFraction;
        // pointermove outruns what the decoder can service; one seek per frame is
        // already more than enough to feel continuous.
        if (seekRaf) return;
        seekRaf = requestAnimationFrame(() => {
            seekRaf = 0;
            ctrl?.seekToFraction(scrubFraction);
        });
    }

    function onScrubUp(e: PointerEvent) {
        if (!scrubbing) return;
        scrubbing = false;
        (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
        ctrl?.seekToFraction(scrubFraction);
    }

    // ── Auto-hiding chrome ────────────────────────────────────────────────────
    // Video only, and only while playing: hiding the controls of a paused clip
    // just makes them hard to find again.

    const HIDE_MS = 2200;
    let chromeVisible = $state(true);
    let hideTimer: ReturnType<typeof setTimeout> | undefined;

    function bumpChrome() {
        chromeVisible = true;
        clearTimeout(hideTimer);
        if (kind !== "video") return;
        if (!ctrl?.playing || scrubbing) return;
        hideTimer = setTimeout(() => (chromeVisible = false), HIDE_MS);
    }

    // Pausing must bring the chrome straight back, not wait out the timer.
    $effect(() => {
        void ctrl?.playing;
        bumpChrome();
    });

    $effect(() => () => clearTimeout(hideTimer));

    // ── Keyboard ──────────────────────────────────────────────────────────────
    // The player owns the transport keys; ViewerOverlay stands down on those and
    // keeps asset navigation on Shift+arrows while media is open. Anything with a
    // modifier is left alone — Ctrl+Shift+1..9 are the app's quick actions.

    $effect(() => {
        const onKey = (e: KeyboardEvent) => {
            const target = e.target as HTMLElement | null;
            if (target?.closest("input, textarea, [contenteditable='true']")) return;
            if (e.ctrlKey || e.metaKey || e.altKey) return;

            const c = ctrl;
            if (!c) return;

            switch (e.key) {
                case " ":
                case "k":
                case "K":
                    e.preventDefault();
                    c.toggle();
                    break;
                case "ArrowRight":
                    if (e.shiftKey) return; // viewer navigates instead
                    e.preventDefault();
                    c.seekBy(SEEK_STEP_S);
                    break;
                case "ArrowLeft":
                    if (e.shiftKey) return;
                    e.preventDefault();
                    c.seekBy(-SEEK_STEP_S);
                    break;
                case "l":
                case "L":
                    e.preventDefault();
                    c.seekBy(SEEK_JUMP_S);
                    break;
                case "j":
                case "J":
                    e.preventDefault();
                    c.seekBy(-SEEK_JUMP_S);
                    break;
                case "ArrowUp":
                    e.preventDefault();
                    c.nudgeVolume(0.05);
                    break;
                case "ArrowDown":
                    e.preventDefault();
                    c.nudgeVolume(-0.05);
                    break;
                case "m":
                case "M":
                    e.preventDefault();
                    c.toggleMute();
                    break;
                default:
                    // 0-9 jump to that tenth of the clip, as every web player does.
                    if (e.key >= "0" && e.key <= "9") {
                        e.preventDefault();
                        c.seekToFraction(Number(e.key) / 10);
                    }
                    return;
            }
            bumpChrome();
        };
        window.addEventListener("keydown", onKey);
        return () => window.removeEventListener("keydown", onKey);
    });

    // ── Presentation helpers ──────────────────────────────────────────────────

    const BTN =
        "rounded-md p-1.5 text-white/75 transition-colors hover:bg-white/10 hover:text-white " +
        "focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-white/40";
    const BTN_ON = "bg-white/15 text-white";

    // Capitalised because it IS the component the template renders — Svelte 5
    // resolves a capitalised identifier as a component, which saves a {@const}
    // (illegal inside a <button>) or the deprecated <svelte:component>.
    const VolumeIcon = $derived(
        ctrl?.muted || (ctrl?.volume ?? 1) === 0 ? VolumeX : (ctrl?.volume ?? 1) < 0.5 ? Volume1 : Volume2,
    );

    async function reveal() {
        if (!destPath) return;
        try {
            await revealItemInDir(destPath);
        } catch {
            // Nothing useful to say if the shell refuses; the path is still shown.
        }
    }
</script>

<!-- ── Scrubber ─────────────────────────────────────────────────────────────
     A 4px visual track inside a 16px hit area — a hairline you have to aim at
     is the single most common way a custom player feels worse than the native
     one. -->
{#snippet scrubber()}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
        role="slider"
        tabindex="-1"
        aria-label="Seek"
        aria-valuemin={0}
        aria-valuemax={Math.round(duration)}
        aria-valuenow={Math.round(displayTime)}
        aria-valuetext={formatDuration(displayTime)}
        class="group/scrub relative -my-1.5 cursor-pointer py-1.5 {duration > 0 ? '' : 'pointer-events-none opacity-40'}"
        onpointerdown={onScrubDown}
        onpointermove={onScrubMove}
        onpointerup={onScrubUp}
        onpointercancel={onScrubUp}
        onpointerleave={() => (hoverFraction = null)}
    >
        <div class="relative h-1 w-full overflow-hidden rounded-full bg-white/15">
            <div class="absolute inset-y-0 left-0 bg-white/25" style="width:{bufferedPct}%"></div>
            <div class="absolute inset-y-0 left-0 bg-blue-500" style="width:{progress * 100}%"></div>
        </div>
        <!-- Knob: only materialises on hover/drag, so a resting bar stays a clean line. -->
        <div
            class="pointer-events-none absolute top-1/2 h-3 w-3 -translate-x-1/2 -translate-y-1/2 rounded-full
                   bg-white shadow ring-1 ring-black/20 transition-transform
                   {scrubbing ? 'scale-100' : 'scale-0 group-hover/scrub:scale-100'}"
            style="left:{progress * 100}%"
        ></div>
        {#if hoverFraction !== null && duration > 0}
            <div
                class="pointer-events-none absolute bottom-full mb-2 -translate-x-1/2 rounded
                       bg-neutral-950/95 px-1.5 py-0.5 text-[11px] tabular-nums text-white
                       ring-1 ring-white/10"
                style="left:{hoverFraction * 100}%"
            >
                {formatDuration(hoverFraction * duration)}
            </div>
        {/if}
    </div>
{/snippet}

<!-- ── Control bar ──────────────────────────────────────────────────────────
     Same surface language as the viewer's zoom toolbar: a dark translucent slab
     with a hairline ring, so the two never read as parts of different apps. -->
{#snippet controls(withScrubber: boolean)}
    <div class="flex flex-col gap-2">
        {#if withScrubber}
            {@render scrubber()}
        {/if}

        <div class="flex items-center gap-1">
            <button
                type="button"
                onclick={() => ctrl?.toggle()}
                title={ctrl?.playing ? "Pause (Space)" : "Play (Space)"}
                aria-label={ctrl?.playing ? "Pause" : "Play"}
                class={BTN}
            >
                {#if ctrl?.playing}
                    <Pause class="h-5 w-5" />
                {:else}
                    <Play class="h-5 w-5" />
                {/if}
            </button>

            <span class="ml-1 select-none text-xs tabular-nums text-white/70">
                {formatDuration(displayTime)}
                <span class="text-white/30">/</span>
                {formatDuration(duration)}
            </span>

            <div class="flex-1"></div>

            <!-- Volume: the slider stays collapsed until the cluster is hovered,
                 so the resting bar isn't half taken up by a control nobody is
                 using. -->
            <div class="group/vol flex items-center gap-1">
                <button
                    type="button"
                    onclick={() => ctrl?.toggleMute()}
                    title={ctrl?.muted ? "Unmute (M)" : "Mute (M)"}
                    aria-label={ctrl?.muted ? "Unmute" : "Mute"}
                    class={BTN}
                >
                    <VolumeIcon class="h-4 w-4" />
                </button>
                <input
                    type="range"
                    min="0"
                    max="1"
                    step="0.01"
                    value={ctrl?.muted ? 0 : (ctrl?.volume ?? 1)}
                    oninput={(e) => ctrl?.setVolume(Number(e.currentTarget.value))}
                    aria-label="Volume"
                    class="h-1 w-0 cursor-pointer accent-blue-500 opacity-0 transition-all duration-150
                           group-hover/vol:w-20 group-hover/vol:opacity-100
                           focus-visible:w-20 focus-visible:opacity-100"
                />
            </div>

            <button
                type="button"
                onclick={() => ctrl?.cycleRate()}
                title="Playback speed"
                class="{BTN} min-w-[2.5rem] text-xs tabular-nums {ctrl && ctrl.rate !== 1 ? BTN_ON : ''}"
            >
                {ctrl?.rate ?? 1}×
            </button>

            <button
                type="button"
                onclick={() => ctrl?.toggleLoop()}
                title="Loop"
                aria-pressed={ctrl?.looping ?? false}
                class="{BTN} {ctrl?.looping ? BTN_ON : ''}"
            >
                <Repeat class="h-4 w-4" />
            </button>

            <button
                type="button"
                onclick={onToggleFullscreen}
                title={fullscreen ? "Exit fullscreen (F)" : "Fullscreen (F)"}
                aria-label={fullscreen ? "Exit fullscreen" : "Fullscreen"}
                class={BTN}
            >
                {#if fullscreen}
                    <Minimize2 class="h-4 w-4" />
                {:else}
                    <Maximize2 class="h-4 w-4" />
                {/if}
            </button>
        </div>
    </div>
{/snippet}

<!-- ── Unsupported format ───────────────────────────────────────────────────
     Reached when the webview refuses the container or codec — MKV everywhere,
     ProRes MOV, AVI, and H.264 on Linux builds without the right GStreamer
     plugins. Says which file and offers the one thing that still works. -->
{#snippet unsupported()}
    <div class="flex h-full w-full flex-col items-center justify-center gap-3 px-8 text-center">
        <TriangleAlert class="h-8 w-8 text-amber-400/80" />
        <div class="text-sm text-neutral-200">Can't preview this format</div>
        <div class="max-w-md text-xs leading-relaxed text-neutral-500">
            {#if extension}<span class="font-mono uppercase text-neutral-400">{extension}</span> isn't
                playable by the system's media engine.{:else}This file isn't playable by the system's
                media engine.{/if}
            The file itself is fine — open it in another app to view it.
        </div>
        {#if destPath}
            <button
                type="button"
                onclick={reveal}
                class="mt-1 inline-flex items-center gap-1.5 rounded-md bg-white/10 px-3 py-1.5
                       text-xs text-white transition-colors hover:bg-white/20"
            >
                <FolderOpen class="h-3.5 w-3.5" />
                Show in Explorer
            </button>
        {/if}
    </div>
{/snippet}

{#if kind === "video"}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <div
        class="absolute inset-0 flex items-center justify-center overflow-hidden
               {chromeVisible ? '' : 'cursor-none'}"
        onpointermove={bumpChrome}
        onclick={(e) => {
            if (e.target === e.currentTarget) onBackdropClick?.();
        }}
    >
        <!-- svelte-ignore a11y_media_has_caption -->
        <!-- No caption track: these are arbitrary user files, and there is no
             sidecar subtitle story yet. -->
        <video
            bind:this={el}
            {src}
            poster={poster ?? undefined}
            preload="auto"
            playsinline
            class="max-h-full max-w-full select-none object-contain {ctrl?.failed ? 'invisible' : ''}"
            onclick={() => ctrl?.toggle()}
            ondblclick={onToggleFullscreen}
        ></video>

        {#if ctrl?.failed}
            <div class="absolute inset-0">{@render unsupported()}</div>
        {:else}
            <!-- Centre affordance: the obvious target when the clip is paused,
                 gone the moment it plays so it never covers the picture. -->
            {#if ctrl && !ctrl.playing && !ctrl.waiting}
                <button
                    type="button"
                    onclick={() => ctrl?.play()}
                    aria-label="Play"
                    class="absolute grid h-16 w-16 place-items-center rounded-full bg-black/45
                           text-white ring-1 ring-white/20 backdrop-blur-sm transition
                           hover:scale-105 hover:bg-black/60"
                >
                    <Play class="h-7 w-7 translate-x-0.5" />
                </button>
            {/if}

            {#if ctrl?.waiting}
                <LoaderCircle class="pointer-events-none absolute h-10 w-10 animate-spin text-white/70" />
            {/if}
        {/if}

        {#if !ctrl?.failed}
            <div
                class="absolute bottom-4 left-1/2 w-[min(46rem,calc(100%-2rem))] -translate-x-1/2
                       rounded-xl bg-neutral-900/90 px-3 py-2.5 shadow-2xl ring-1 ring-white/10
                       backdrop-blur transition-opacity duration-200
                       {chromeVisible ? 'opacity-100' : 'pointer-events-none opacity-0'}"
            >
                {@render controls(true)}
            </div>
        {/if}
    </div>
{:else}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <div
        class="absolute inset-0 flex items-center justify-center p-8"
        onclick={(e) => {
            if (e.target === e.currentTarget) onBackdropClick?.();
        }}
    >
        <audio bind:this={el} {src} preload="auto"></audio>

        <div
            class="w-full max-w-xl rounded-2xl bg-neutral-900/85 p-5 shadow-2xl ring-1 ring-white/10
                   backdrop-blur"
        >
            {#if ctrl?.failed}
                <div class="h-56">{@render unsupported()}</div>
            {:else}
                <!-- Identity row -->
                <div class="flex items-center gap-3.5">
                    <div
                        class="grid h-14 w-14 shrink-0 place-items-center rounded-xl bg-gradient-to-br
                               from-neutral-700/70 to-neutral-900 ring-1 ring-white/10"
                    >
                        <Music class="h-6 w-6 text-white/50" />
                    </div>
                    <div class="min-w-0">
                        <div class="truncate text-sm font-medium text-neutral-100">{filename}</div>
                        <div class="mt-0.5 text-xs text-neutral-500">
                            {#if extension}<span class="uppercase">{extension}</span>{/if}
                            {#if extension && fileSize}<span class="text-neutral-700"> · </span>{/if}
                            {#if fileSize}{formatBytes(fileSize)}{/if}
                        </div>
                    </div>
                    {#if ctrl?.waiting}
                        <LoaderCircle class="ml-auto h-4 w-4 shrink-0 animate-spin text-white/40" />
                    {/if}
                </div>

                <!-- Waveform doubles as the scrubber. When it couldn't be decoded
                     the control bar grows the plain scrubber instead, so the card
                     never shows a dead strip. -->
                {#if peaks}
                    <!-- svelte-ignore a11y_no_static_element_interactions -->
                    <div
                        bind:clientWidth={waveWidth}
                        role="slider"
                        tabindex="-1"
                        aria-label="Seek"
                        aria-valuemin={0}
                        aria-valuemax={Math.round(duration)}
                        aria-valuenow={Math.round(displayTime)}
                        aria-valuetext={formatDuration(displayTime)}
                        class="relative mt-5 cursor-pointer"
                        style="height:{WAVE_HEIGHT}px"
                        onpointerdown={onScrubDown}
                        onpointermove={onScrubMove}
                        onpointerup={onScrubUp}
                        onpointercancel={onScrubUp}
                        onpointerleave={() => (hoverFraction = null)}
                    >
                        <canvas
                            bind:this={baseCanvas}
                            class="absolute inset-0 h-full w-full"
                            style="width:{waveWidth}px;height:{WAVE_HEIGHT}px"
                        ></canvas>
                        <!-- The played half is the same drawing in the accent
                             colour, revealed by a clip — so progress costs a CSS
                             property change, not a canvas repaint per frame. -->
                        <canvas
                            bind:this={playedCanvas}
                            class="absolute inset-0 h-full w-full"
                            style="width:{waveWidth}px;height:{WAVE_HEIGHT}px;
                                   clip-path:inset(0 {100 - progress * 100}% 0 0)"
                        ></canvas>
                        {#if hoverFraction !== null && duration > 0}
                            <div
                                class="pointer-events-none absolute -top-1 -translate-x-1/2 -translate-y-full
                                       rounded bg-neutral-950/95 px-1.5 py-0.5 text-[11px] tabular-nums
                                       text-white ring-1 ring-white/10"
                                style="left:{hoverFraction * 100}%"
                            >
                                {formatDuration(hoverFraction * duration)}
                            </div>
                        {/if}
                    </div>
                {/if}

                <div class="mt-3">
                    {@render controls(!peaks)}
                </div>
            {/if}
        </div>
    </div>
{/if}
