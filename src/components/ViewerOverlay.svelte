<script lang="ts">
    import { untrack } from "svelte";
    import { convertFileSrc } from "@tauri-apps/api/core";
    import { assetLibrary, thumbHashUrl } from "$lib/assets.svelte";
    import { viewer } from "$lib/viewer.svelte";

    const current = $derived(viewer.current);

    // Hydrate the heavy row (for dest_path / thumb_path) whenever the asset
    // changes. Cheap — usually already cached from the grid.
    $effect(() => {
        const id = current?.id;
        if (id) untrack(() => assetLibrary.ensure([id]));
    });

    const heavy = $derived(current ? assetLibrary.heavy.get(current.id) : undefined);

    // Layered sources, cheapest-first: a ThumbHash blur (instant, no network),
    // then the cached thumbnail, then the full-resolution original. The full-res
    // image carries crossorigin="anonymous" so the V3 eyedropper can sample it
    // without tainting the canvas (proven in the V0 spike).
    const placeholder = $derived(thumbHashUrl(current?.thumb_hash ?? null));
    const thumbUrl = $derived(heavy?.thumb_path ? convertFileSrc(heavy.thumb_path) : null);
    const fullUrl = $derived(heavy?.dest_path ? convertFileSrc(heavy.dest_path) : null);

    const isImage = $derived(current?.asset_type === "image");

    // Fade the full-res image in once it decodes, so switching assets doesn't
    // flash the low-res thumbnail underneath.
    let fullLoaded = $state(false);
    $effect(() => {
        void fullUrl; // reset the fade whenever the source changes
        fullLoaded = false;
    });

    // Keyboard: active only while open. Registered here (not globally) so it
    // tears down with the overlay — no dangling window listener.
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
            }
        };
        window.addEventListener("keydown", onKey);
        return () => window.removeEventListener("keydown", onKey);
    });

    const fullscreen = $derived(viewer.mode === "fullscreen");
</script>

{#if viewer.isOpen && current}
    <!-- QuickLook is absolute → stays within the grid column (sidebars remain
         visible). Fullscreen is fixed → covers the whole window. One component,
         two positioning contexts. -->
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <div
        class="z-[80] flex items-center justify-center overflow-hidden
               {fullscreen ? 'fixed inset-0 bg-black' : 'absolute inset-0 bg-black/90'}"
        onclick={(e) => {
            // Click the backdrop (not the image) to close.
            if (e.target === e.currentTarget) viewer.close();
        }}
    >
        <!-- Image stage -->
        {#if isImage}
            <div class="relative flex h-full w-full items-center justify-center p-8">
                {#if placeholder}
                    <img
                        src={placeholder}
                        alt=""
                        aria-hidden="true"
                        class="pointer-events-none absolute max-h-full max-w-full object-contain
                               opacity-100 blur-lg transition-opacity"
                        style={fullLoaded ? "opacity:0" : ""}
                    />
                {/if}
                {#if thumbUrl}
                    <img
                        src={thumbUrl}
                        alt=""
                        aria-hidden="true"
                        class="pointer-events-none absolute max-h-full max-w-full object-contain"
                        style={fullLoaded ? "opacity:0" : ""}
                    />
                {/if}
                {#if fullUrl}
                    <img
                        src={fullUrl}
                        alt={current.filename}
                        crossorigin="anonymous"
                        draggable="false"
                        onload={() => (fullLoaded = true)}
                        class="relative max-h-full max-w-full object-contain transition-opacity duration-150"
                        style={fullLoaded ? "opacity:1" : "opacity:0"}
                    />
                {/if}
            </div>
        {:else}
            <!-- Non-images (video/audio/unknown) — V1 shows a placeholder; the
                 dedicated players are deferred (§4). -->
            <div class="flex flex-col items-center gap-3 text-neutral-400">
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

        <!-- Filename + counter -->
        <div
            class="pointer-events-none absolute left-4 top-3 flex items-center gap-2 text-xs text-white/70"
        >
            <span class="max-w-[40vw] truncate">{current.filename}</span>
            <span class="text-white/40">{viewer.index + 1} / {viewer.count}</span>
        </div>

        <!-- Top-right actions -->
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

        <!-- Prev / next -->
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
    </div>
{/if}
