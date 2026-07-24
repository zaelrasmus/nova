<script lang="ts">
    // ── SPIKE D0 — DELETE THIS ROUTE WHEN THE DRAG & DROP PHASE LANDS ─────────
    //
    // Measures which drag mechanisms actually work inside the Tauri webview, so
    // the DnD architecture is chosen from evidence rather than from docs.
    //
    // Run it TWICE — once with `dragDropEnabled` at its default (true), once with
    // it set to false in tauri.conf.json — and compare. The two config states are
    // mutually exclusive, and this page shows exactly what each one costs.
    //
    // Tauri's own typedoc says: "Disabling it is required to use HTML5 drag and
    // drop on the frontend on Windows." If that's literal, TEST 1 fails at
    // `true`, and pragmatic-drag-and-drop (which IS the HTML5 API) can't be used
    // for internal DnD without giving up path-carrying file drops.
    // ─────────────────────────────────────────────────────────────────────────

    import { getCurrentWebview } from "@tauri-apps/api/webview";

    interface Entry {
        t: string;
        src: "html5-internal" | "html5-external" | "tauri" | "info";
        msg: string;
    }

    let log = $state<Entry[]>([]);
    const stamp = () => new Date().toISOString().slice(11, 23);

    function add(src: Entry["src"], msg: string) {
        log.push({ t: stamp(), src, msg });
        if (log.length > 250) log.splice(0, log.length - 250);
    }

    // ── What we're trying to learn ────────────────────────────────────────────
    // Each flips to true the first time the corresponding event is observed, so
    // the verdict panel reads off facts rather than guesses.
    let sawDragStart = $state(false); // in-page drag can even BEGIN
    let sawInternalOver = $state(false); // ...and a page element sees dragover
    let sawInternalDrop = $state(false); // ...and the drop completes
    let sawExternalOver = $state(false); // OS/browser drag reaches HTML5 handlers
    let sawExternalDrop = $state(false);
    let sawTauriEnter = $state(false); // native handler is live
    let sawTauriDrop = $state(false);

    let externalTypes = $state<string>("—");
    let externalDetail = $state<string>("—");
    let tauriPaths = $state<string[]>([]);
    let hitRaw = $state("—");
    let hitScaled = $state("—");

    const dpr = typeof window !== "undefined" ? window.devicePixelRatio : 1;

    // ── TEST 3 helper: which coordinate space is `position` in? ───────────────
    // Tauri reports a PhysicalPosition; elementFromPoint wants CSS pixels. If the
    // two differ, dividing by devicePixelRatio is the conversion — but that's
    // only PROVABLE when dpr !== 1, since at 100% scaling both are identical.
    function describeEl(el: Element | null): string {
        if (!el) return "∅ (outside the document)";
        const zone = el.closest("[data-hit-id]");
        if (zone) return `TARGET ${zone.getAttribute("data-hit-id")}`;
        return `<${el.tagName.toLowerCase()}> (not a target)`;
    }

    function hitTest(px: number, py: number) {
        hitRaw = describeEl(document.elementFromPoint(px, py));
        hitScaled = describeEl(document.elementFromPoint(px / dpr, py / dpr));
    }

    // ── TEST 3: the native Tauri drag-drop event ──────────────────────────────
    $effect(() => {
        let unlisten: (() => void) | null = null;
        let dead = false;

        getCurrentWebview()
            .onDragDropEvent((e) => {
                const p = e.payload;
                if (p.type === "enter") {
                    sawTauriEnter = true;
                    tauriPaths = p.paths;
                    add(
                        "tauri",
                        `enter — ${p.paths.length} path(s) @ (${p.position.x}, ${p.position.y})`,
                    );
                    hitTest(p.position.x, p.position.y);
                } else if (p.type === "over") {
                    // NOTE: 'over' carries NO paths — only enter/drop do. Any
                    // hover affordance must cache what 'enter' handed us.
                    hitTest(p.position.x, p.position.y);
                } else if (p.type === "drop") {
                    sawTauriDrop = true;
                    tauriPaths = p.paths;
                    add("tauri", `drop — ${p.paths.length} path(s)`);
                    for (const path of p.paths.slice(0, 8)) add("tauri", `    ${path}`);
                    if (p.paths.length > 8) add("tauri", `    …+${p.paths.length - 8} more`);
                    hitTest(p.position.x, p.position.y);
                } else {
                    add("tauri", "leave");
                }
            })
            .then((fn) => {
                if (dead) fn();
                else {
                    unlisten = fn;
                    add("info", "Tauri native drag-drop listener attached.");
                }
            })
            .catch((err) => add("info", `Tauri listener FAILED: ${err}`));

        return () => {
            dead = true;
            unlisten?.();
        };
    });

    // ── TEST 1: purely in-page HTML5 drag ─────────────────────────────────────
    let internalHot = $state(false);

    function onDragStart(e: DragEvent) {
        sawDragStart = true;
        add("html5-internal", "dragstart fired");
        e.dataTransfer?.setData("text/plain", "nova-spike-payload");
        if (e.dataTransfer) e.dataTransfer.effectAllowed = "move";
    }

    function onInternalOver(e: DragEvent) {
        e.preventDefault(); // required, or drop never fires
        if (!sawInternalOver) add("html5-internal", "dragover fired on the target");
        sawInternalOver = true;
        internalHot = true;
    }

    function onInternalDrop(e: DragEvent) {
        e.preventDefault();
        sawInternalDrop = true;
        internalHot = false;
        add("html5-internal", `drop — payload: "${e.dataTransfer?.getData("text/plain")}"`);
    }

    // ── TEST 2: what HTML5 sees from an EXTERNAL drag ─────────────────────────
    let externalHot = $state(false);

    function onExternalOver(e: DragEvent) {
        e.preventDefault();
        if (!sawExternalOver) add("html5-external", "dragover fired from an external source");
        sawExternalOver = true;
        externalHot = true;
        externalTypes = [...(e.dataTransfer?.types ?? [])].join(", ") || "(none)";
    }

    function onExternalDrop(e: DragEvent) {
        e.preventDefault();
        sawExternalDrop = true;
        externalHot = false;
        const dt = e.dataTransfer;
        if (!dt) {
            externalDetail = "no dataTransfer";
            return;
        }
        externalTypes = [...dt.types].join(", ") || "(none)";
        const parts: string[] = [];
        // The decisive question for file ingestion: is there a real PATH here, or
        // only bytes? A media manager needs the path.
        if (dt.files.length) {
            const f = dt.files[0];
            const maybePath = (f as File & { path?: string }).path;
            parts.push(`${dt.files.length} file(s); first="${f.name}" ${f.size}B`);
            parts.push(`.path = ${maybePath ? `"${maybePath}"` : "UNDEFINED (bytes only)"}`);
        }
        const uri = dt.getData("text/uri-list");
        if (uri) parts.push(`uri-list="${uri.slice(0, 200)}"`);
        const txt = dt.getData("text/plain");
        if (txt) parts.push(`text="${txt.slice(0, 120)}"`);
        externalDetail = parts.join(" · ") || "(empty)";
        add("html5-external", `drop — ${externalDetail}`);
    }

    // ── Verdicts ──────────────────────────────────────────────────────────────
    const html5Internal = $derived(
        sawInternalDrop
            ? { label: "WORKS", tone: "ok" }
            : sawDragStart
              ? { label: "PARTIAL — drag starts, drop never lands", tone: "warn" }
              : { label: "not observed yet", tone: "idle" },
    );
    const html5External = $derived(
        sawExternalDrop
            ? { label: "WORKS", tone: "ok" }
            : sawExternalOver
              ? { label: "PARTIAL — dragover only", tone: "warn" }
              : { label: "not observed yet", tone: "idle" },
    );
    const nativeVerdict = $derived(
        sawTauriDrop
            ? { label: "WORKS — paths delivered", tone: "ok" }
            : sawTauriEnter
              ? { label: "PARTIAL — enter but no drop", tone: "warn" }
              : { label: "not observed yet", tone: "idle" },
    );

    const toneClass = (tone: string) =>
        tone === "ok"
            ? "text-emerald-400"
            : tone === "warn"
              ? "text-amber-400"
              : "text-neutral-500";

    function reset() {
        log = [];
        sawDragStart = sawInternalOver = sawInternalDrop = false;
        sawExternalOver = sawExternalDrop = false;
        sawTauriEnter = sawTauriDrop = false;
        externalTypes = externalDetail = "—";
        tauriPaths = [];
        hitRaw = hitScaled = "—";
    }

    async function copyReport() {
        const report = [
            `NOVA D0 SPIKE — ${new Date().toISOString()}`,
            `userAgent: ${navigator.userAgent}`,
            `devicePixelRatio: ${dpr}`,
            ``,
            `TEST 1 in-page HTML5 : ${html5Internal.label}`,
            `TEST 2 external HTML5: ${html5External.label}`,
            `   types  : ${externalTypes}`,
            `   detail : ${externalDetail}`,
            `TEST 3 Tauri native  : ${nativeVerdict.label}`,
            `   paths  : ${tauriPaths.length}`,
            `   hit raw    : ${hitRaw}`,
            `   hit /dpr   : ${hitScaled}`,
            ``,
            `LOG`,
            ...log.map((e) => `${e.t} [${e.src}] ${e.msg}`),
        ].join("\n");
        await navigator.clipboard.writeText(report);
        add("info", "Report copied to clipboard.");
    }
</script>

<svelte:head><title>Nova — D0 drag &amp; drop spike</title></svelte:head>

<div class="min-h-screen bg-neutral-950 p-6 text-neutral-200">
    <header class="mb-5 flex items-baseline justify-between">
        <div>
            <h1 class="text-lg font-semibold">D0 — Drag &amp; Drop capability spike</h1>
            <p class="text-xs text-neutral-500">
                Temporary route. Run once with <code>dragDropEnabled</code> unset/true, once with it
                false.
            </p>
        </div>
        <div class="flex gap-2">
            <button
                onclick={copyReport}
                class="rounded border border-neutral-700 px-2 py-1 text-xs hover:bg-neutral-800"
                >Copy report</button
            >
            <button
                onclick={reset}
                class="rounded border border-neutral-700 px-2 py-1 text-xs hover:bg-neutral-800"
                >Reset</button
            >
            <a
                href="/"
                class="rounded border border-neutral-700 px-2 py-1 text-xs hover:bg-neutral-800"
                >← App</a
            >
        </div>
    </header>

    <!-- Verdicts ------------------------------------------------------------ -->
    <div class="mb-5 grid grid-cols-3 gap-3">
        {#snippet verdict(title: string, v: { label: string; tone: string }, note: string)}
            <div class="rounded-lg border border-neutral-800 bg-neutral-900/50 p-3">
                <div class="text-[11px] uppercase tracking-wide text-neutral-500">{title}</div>
                <div class="mt-1 text-sm font-medium {toneClass(v.tone)}">{v.label}</div>
                <div class="mt-1 text-[11px] leading-snug text-neutral-600">{note}</div>
            </div>
        {/snippet}
        {@render verdict(
            "1 · In-page HTML5",
            html5Internal,
            "Decides whether pragmatic-drag-and-drop is usable for grid/tree DnD.",
        )}
        {@render verdict(
            "2 · External → HTML5",
            html5External,
            "Decides whether browser/Figma URL drops are reachable at all.",
        )}
        {@render verdict(
            "3 · Tauri native",
            nativeVerdict,
            "The path-carrying file import channel. Should work whenever the flag is true.",
        )}
    </div>

    <div class="grid grid-cols-2 gap-4">
        <!-- TEST 1 --------------------------------------------------------- -->
        <section class="rounded-lg border border-neutral-800 p-4">
            <h2 class="mb-1 text-sm font-medium">Test 1 — drag inside the page</h2>
            <p class="mb-3 text-xs text-neutral-500">
                Drag the chip into the dashed box. Nothing leaves the webview.
            </p>
            <div class="flex items-center gap-4">
                <div
                    draggable="true"
                    ondragstart={onDragStart}
                    ondragend={() => add("html5-internal", "dragend fired")}
                    role="button"
                    tabindex="0"
                    class="cursor-grab select-none rounded-md bg-blue-600 px-3 py-2 text-xs
                           font-medium text-white active:cursor-grabbing"
                >
                    ⠿ drag me
                </div>
                <!-- svelte-ignore a11y_no_static_element_interactions -->
                <div
                    ondragover={onInternalOver}
                    ondragleave={() => (internalHot = false)}
                    ondrop={onInternalDrop}
                    class="flex h-20 flex-1 items-center justify-center rounded-md border-2 border-dashed
                           text-xs transition-colors
                           {internalHot
                        ? 'border-blue-500 bg-blue-500/10 text-blue-300'
                        : 'border-neutral-700 text-neutral-500'}"
                >
                    {sawInternalDrop ? "✓ received the drop" : "drop target"}
                </div>
            </div>
        </section>

        <!-- TEST 2 --------------------------------------------------------- -->
        <section class="rounded-lg border border-neutral-800 p-4">
            <h2 class="mb-1 text-sm font-medium">Test 2 — external drag, seen by HTML5</h2>
            <p class="mb-3 text-xs text-neutral-500">
                Drop a file from Explorer, then an image dragged out of a browser tab.
            </p>
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <div
                ondragover={onExternalOver}
                ondragleave={() => (externalHot = false)}
                ondrop={onExternalDrop}
                class="flex h-20 items-center justify-center rounded-md border-2 border-dashed text-xs
                       transition-colors
                       {externalHot
                    ? 'border-emerald-500 bg-emerald-500/10 text-emerald-300'
                    : 'border-neutral-700 text-neutral-500'}"
            >
                {sawExternalDrop ? "✓ HTML5 saw an external drop" : "drop a file or web image here"}
            </div>
            <dl class="mt-2 space-y-0.5 text-[11px]">
                <div class="flex gap-2">
                    <dt class="w-14 shrink-0 text-neutral-600">types</dt>
                    <dd class="break-all text-neutral-400">{externalTypes}</dd>
                </div>
                <div class="flex gap-2">
                    <dt class="w-14 shrink-0 text-neutral-600">detail</dt>
                    <dd class="break-all text-neutral-400">{externalDetail}</dd>
                </div>
            </dl>
        </section>

        <!-- TEST 3 --------------------------------------------------------- -->
        <section class="col-span-2 rounded-lg border border-neutral-800 p-4">
            <h2 class="mb-1 text-sm font-medium">
                Test 3 — Tauri native event + coordinate hit-testing
            </h2>
            <p class="mb-3 text-xs text-neutral-500">
                Hover a file from Explorer over the numbered boxes WITHOUT releasing, then drop. The
                native event reports physical pixels and no DOM target, so D1's folder highlighting
                depends on converting them correctly.
                {#if dpr === 1}
                    <span class="text-amber-500"
                        >devicePixelRatio is 1, so both readings match and this test can't
                        distinguish — set Windows display scaling to 125% or 150% and reload to make
                        it conclusive.</span
                    >
                {/if}
            </p>
            <div class="mb-3 grid grid-cols-4 gap-2">
                {#each [1, 2, 3, 4] as n (n)}
                    <div
                        data-hit-id={n}
                        class="flex h-16 items-center justify-center rounded-md border border-neutral-700
                               bg-neutral-900 text-xs text-neutral-500"
                    >
                        target {n}
                    </div>
                {/each}
            </div>
            <dl class="grid grid-cols-2 gap-x-4 gap-y-0.5 text-[11px]">
                <div class="flex gap-2">
                    <dt class="w-28 shrink-0 text-neutral-600">dpr</dt>
                    <dd class="text-neutral-400">{dpr}</dd>
                </div>
                <div class="flex gap-2">
                    <dt class="w-28 shrink-0 text-neutral-600">paths seen</dt>
                    <dd class="text-neutral-400">{tauriPaths.length}</dd>
                </div>
                <div class="flex gap-2">
                    <dt class="w-28 shrink-0 text-neutral-600">elementFromPoint(x,y)</dt>
                    <dd class="text-neutral-400">{hitRaw}</dd>
                </div>
                <div class="flex gap-2">
                    <dt class="w-28 shrink-0 text-neutral-600">…(x/dpr, y/dpr)</dt>
                    <dd class="text-neutral-400">{hitScaled}</dd>
                </div>
            </dl>
        </section>
    </div>

    <!-- Log ----------------------------------------------------------------- -->
    <section class="mt-4 rounded-lg border border-neutral-800">
        <h2 class="border-b border-neutral-800 px-4 py-2 text-sm font-medium">
            Event log <span class="text-xs font-normal text-neutral-600">({log.length})</span>
        </h2>
        <div class="max-h-64 overflow-y-auto px-4 py-2 font-mono text-[11px] leading-relaxed">
            {#if log.length === 0}
                <p class="text-neutral-600">Nothing yet — start dragging.</p>
            {/if}
            {#each log as e, i (i)}
                <div class="flex gap-2">
                    <span class="shrink-0 text-neutral-700">{e.t}</span>
                    <span
                        class="w-24 shrink-0 {e.src === 'tauri'
                            ? 'text-purple-400'
                            : e.src === 'html5-internal'
                              ? 'text-blue-400'
                              : e.src === 'html5-external'
                                ? 'text-emerald-400'
                                : 'text-neutral-600'}">{e.src}</span
                    >
                    <span class="break-all text-neutral-300">{e.msg}</span>
                </div>
            {/each}
        </div>
    </section>
</div>
