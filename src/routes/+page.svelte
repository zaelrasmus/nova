<script lang="ts">
    import { onMount } from "svelte";
    import { invoke } from "@tauri-apps/api/core";
    import { listen } from "@tauri-apps/api/event";
    import { open } from "@tauri-apps/plugin-dialog";
    import { cubicOut } from "svelte/easing";
    import { tweened } from "svelte/motion";
    import { QueryClient, QueryClientProvider } from "@tanstack/svelte-query";
    import { toast } from "svelte-sonner";
    import { PanelLeft, PanelRight, Settings, Download } from "@lucide/svelte";

    import { assetLibrary, type ManifestScope } from "$lib/assets.svelte";
    import { runAction, undoLatest, undoRun } from "../components/actions/run";
    import { dropzone } from "$lib/dropzone.svelte";
    import type { DropTarget } from "$lib/droptarget";
    import { drag, DRAG_SCROLL_ATTR } from "$lib/dragdrop.svelte";
    import { selection } from "$lib/selection.svelte";
    import { layout } from "$lib/layout.svelte";

    import AssetGrid from "$components/AssetGrid.svelte";
    import FolderTree from "$components/FolderTree.svelte";
    import SavedFilters from "$components/SavedFilters.svelte";
    import Inspector from "$components/Inspector.svelte";
    import TagManager from "$components/TagManager.svelte";
    import SearchBar from "$components/SearchBar.svelte";
    import GridToolbar from "$components/GridToolbar.svelte";
    import SystemViews from "$components/SystemViews.svelte";
    import PinnedFolders from "$components/PinnedFolders.svelte";
    import SmartFolders from "$components/SmartFolders.svelte";
    import WindowControls from "$components/layout/WindowControls.svelte";
    import ResizeHandle from "$components/layout/ResizeHandle.svelte";
    import SidebarRail from "$components/layout/SidebarRail.svelte";
    import LibraryMenu from "$components/layout/LibraryMenu.svelte";
    import { Button } from "$components/ui/button";
    import * as Dialog from "$components/ui/dialog";
    import * as Tabs from "$components/ui/tabs";
    import { SETTINGS_SECTIONS, DEFAULT_SECTION_ID } from "./settings-sections";
    import AppearanceSection from "../components/settings/AppearanceSection.svelte";
    import ImportSection from "../components/settings/ImportSection.svelte";
    import DisplaySection from "../components/settings/DisplaySection.svelte";
    import { libraryManager } from "./settings.svelte";

    interface LibraryInfo {
        db_path: string;
        root_path: string;
    }

    type ImportStage = "Scanning" | "ProcessingMetadata" | "CopyingFiles" | "Finalizing";

    interface ImportProgress {
        stage: ImportStage;
        current: number;
        total: number;
        message: string;
    }

    interface ImportResult {
        assets: any[];
        folders: any[];
        path_links: { [key: string]: string };
        /** Files whose bytes the library already held; skipped, not copied. */
        duplicates: number;
        /** Of those, how many were in the Trash and came back. */
        restored: number;
    }

    // Tauri commands throw the serialized AppError string on failure.
    // This function is the single place that bridges a caught Tauri error to a toast.
    //
    // Rule: NEVER show `error` directly — it's already a safe generic message
    // from AppError::frontend_message(), but typing it explicitly makes the
    // contract clear to any future developer reading this code.
    function handleCommandError(error: unknown, fallback = "An unexpected error occurred.") {
        const message = typeof error === "string" ? error : fallback;
        toast.error(message);
    }

    const queryClient = new QueryClient({
        defaultOptions: { queries: { refetchOnWindowFocus: false } },
    });

    // To add a new settings section: import its component and register it below.
    const sectionComponents: Record<string, any> = {
        appearance: AppearanceSection,
        import: ImportSection,
        display: DisplaySection,
    };

    // Import progress state
    let isImporting = $state(false);
    // Whether to recreate the source folder tree in-app on import. Off = every
    // asset is imported "free" (uncategorized).
    let importFolders = $state(true);
    let current = $state(0);
    let total = $state(0);
    let statusMessage = $state("Preparing...");
    let currentStage = $state<ImportStage | null>(null);

    const smoothPercentage = tweened(0, { duration: 400, easing: cubicOut });
    let displayPercent = $derived(Math.round($smoothPercentage));

    // The Tag Manager is a full-screen view, opened over the library.
    let tagManagerOpen = $state(false);

    let noLibraryConnected = $derived(!libraryManager.state.activeLibrary);

    // ── Commands ────────────────────────────────────────────────────────────

    async function createLibrary() {
        const name = prompt("Library name:");
        if (!name) return;

        const location = await open({ directory: true, multiple: false });
        if (!location) return;

        // toast.promise handles loading → success/error automatically.
        // The promise must resolve with a value that the success callback receives.
        toast.promise<LibraryInfo>(
            invoke<LibraryInfo>("create_library", { location, name }).then(async (info) => {
                await libraryManager.switchLibrary(info.root_path);
                return info;
            }),
            {
                success: () => `Library "${name}" created successfully.`,
                error: (e) => (typeof e === "string" ? e : "Failed to create library."),
            },
        );
    }

    async function addExistingLibrary() {
        const selected = await open({ directory: true, multiple: false });
        if (!selected) return;

        try {
            await libraryManager.switchLibrary(selected);
            toast.success("Library opened.");
        } catch (e) {
            handleCommandError(e);
        }
    }

    /**
     * Run an import command behind the progress overlay and the result toast.
     *
     * Shared by the dialog and by drag & drop: the two differ only in which
     * command they call and with what, while the progress plumbing, the reload,
     * and the reporting are identical — and the plumbing is the fiddly part
     * (a listener that must be torn down on every exit path).
     *
     * `reveal` is where to navigate afterwards: wherever the assets actually
     * landed. Staying put would leave the user staring at an unchanged grid
     * wondering whether the import worked.
     */
    async function runImport(
        command: string,
        args: Record<string, unknown>,
        reveal: ManifestScope,
    ) {
        // The guard lives HERE, not at the call sites. The pipeline holds a
        // single DB handle and one progress channel, so two concurrent imports
        // interleave both — and `handleImport` awaits a file dialog before it
        // gets here, which is a wide window for a drop to start one underneath
        // it. Two runs would also mean two `import-progress` listeners writing
        // the same state, with the first `finally` unlistening while the other
        // is still emitting.
        if (isImporting) {
            toast.error("An import is already running.");
            return;
        }

        // Reset progress state
        current = 0;
        total = 0;
        currentStage = null;
        statusMessage = "Preparing...";
        smoothPercentage.set(0);
        isImporting = true;

        const unlisten = await listen<ImportProgress>("import-progress", (event) => {
            const p = event.payload;
            current = p.current;
            total = p.total;
            statusMessage = p.message;
            currentStage = p.stage;
            smoothPercentage.set(p.total > 0 ? (p.current / p.total) * 100 : 0);
        });

        const importPromise = invoke<ImportResult>(command, args).then(async (result) => {
            // Import is now near-instant (no thumbnailing). Show where the
            // assets landed and refresh the folder tree so newly created folders
            // appear; the reload re-runs the grid's on-view effect, generating
            // thumbnails for the visible window while the rest follow as the
            // user scrolls.
            await assetLibrary.setScope(reveal);
            await assetLibrary.loadFolders();
            return result; // pass through so toast.promise still receives it
        });

        toast.promise<ImportResult>(importPromise, {
            loading: "Import in progress...",
            success: (result) => {
                const parts = [
                    `Imported ${result.assets.length} assets across ${result.folders.length} folders.`,
                ];
                // Always say so when files were skipped — otherwise re-importing
                // a folder looks like the import silently failed.
                const skipped = result.duplicates - result.restored;
                if (skipped > 0) parts.push(`Skipped ${skipped} already in the library.`);
                // Reported separately from "skipped": dropping a file you'd
                // deleted brings it back, and that's a different outcome from
                // one that was already there.
                if (result.restored > 0) {
                    parts.push(`Restored ${result.restored} from the Trash.`);
                }
                return parts.join(" ");
            },
            error: (e) => (typeof e === "string" ? e : "Import failed. Please try again."),
        });
        // We can't await toast.promise directly and also run finally,
        // so we manage isImporting with the underlying invoke call.
        try {
            await importPromise;
        } catch {
            // Error is already handled by toast.promise above.
        } finally {
            isImporting = false;
            unlisten();
        }
    }

    /**
     * A drop landed. Turns the paths and the resolved target into an import.
     *
     * Lives here rather than in the dropzone store because this is where "what a
     * drop MEANS" belongs — the store only knows what the cursor is over. It's
     * also where the import overlay and the progress listener already are.
     */
    async function handleDrop(paths: string[], target: DropTarget) {
        if (!libraryManager.state.activeLibrary) {
            toast.error("Open a library before importing.");
            return;
        }
        // An OS drop onto a pinned smart folder is refused for the same reason
        // an internal one is: membership is derived, so there is nothing to put
        // it into. Said out loud rather than quietly importing to the library at
        // large, which would look like the drop landed somewhere it didn't.
        if (target.kind === "smart") {
            toast.error("Smart folders collect matching assets automatically.");
            return;
        }

        const targetFolder = target.kind === "folder" ? target.id : null;
        await runImport(
            "import_dropped_paths",
            { paths, targetFolder, importFolders },
            // Reveal where they landed: the folder they were dropped on, or the
            // whole library for a neutral drop.
            targetFolder ? { kind: "folder", id: targetFolder } : { kind: "all" },
        );
    }

    // One listener for the whole app — there is a single webview, and the native
    // drag-drop event is window-scoped, not per-element.
    $effect(() => {
        let unlisten: (() => void) | null = null;
        let dead = false;
        dropzone
            .attach(handleDrop)
            .then((fn) => (dead ? fn() : (unlisten = fn)))
            .catch((e) => console.error("Failed to attach drop listener", e));
        return () => {
            dead = true;
            unlisten?.();
        };
    });

    async function handleImport() {
        const selectedSource = await open({
            directory: true,
            multiple: false,
            title: "Select folder to import",
        });
        if (!selectedSource) return;

        await runImport(
            "import_assets",
            { sourcePath: selectedSource, importFolders },
            { kind: "all" },
        );
    }

    // As background thumbnails finish, patch the just-completed rows into the
    // manifest in place (ThumbHash placeholder appears, then the real thumbnail
    // re-hydrates). No full reload → no grid flash.
    onMount(() => {
        // Pane widths are persisted preferences; read them once the store has
        // resolved from disk (see layout.svelte.ts).
        void layout.hydrate();

        const unlisten = listen<{
            current: number;
            total: number;
            ready: { id: string; thumb_hash: string; thumb_path: string }[];
        }>("thumbnail-progress", (event) => {
            assetLibrary.applyThumbnails(event.payload.ready);
            assetLibrary.reportThumbProgress(event.payload.current, event.payload.total);
        });
        return () => {
            unlisten.then((fn) => fn());
        };
    });

    // ── Layout ──────────────────────────────────────────────────────────────

    // Header label for the current place. A scope is a place, so it reads like
    // one — the count is what changes when a filter narrows it.
    const scopeLabel = $derived.by(() => {
        const scope = assetLibrary.scope;
        if (scope.kind === "all") return "All assets";
        if (scope.kind === "uncategorized") return "Uncategorized";
        if (scope.kind === "smart") {
            return assetLibrary.smartFolders.find((f) => f.id === scope.id)?.name ?? "Smart folder";
        }
        if (scope.kind === "smart_group") {
            const group = assetLibrary.smartFolderGroups.find((g) => g.id === scope.id);
            // "Everything in X" rather than just "X": a group header and the
            // group's union are different places, and the title is the only
            // thing telling you which one you're looking at.
            return group ? `Everything in ${group.name}` : "Group";
        }
        if (scope.kind === "trash") return "Trash";
        return assetLibrary.folders.find((f) => f.id === scope.id)?.name ?? "Folder";
    });

    const inspectorLabel = $derived(
        selection.assetCount > 1 ? `${selection.assetCount} selected` : "Inspector",
    );

    /** Panel shortcuts. Guarded so they don't fire while typing in the search box. */
    /**
     * Delete key: move the selection to the Trash.
     *
     * Ids snapshotted before the await, like every other bulk operation — the
     * grid re-streams as rows leave the view.
     */
    async function trashSelection() {
        const assetIds = selection.assetIds;
        if (assetIds.length === 0) return;
        // Restoring is what you'd do next, so pressing Delete inside the Trash
        // restoring instead would be a keystroke that means two opposite things.
        // It simply does nothing there; the menu has both verbs.
        if (assetLibrary.scope.kind === "trash") return;
        try {
            const summary = await assetLibrary.setAssetsTrashed(assetIds, true);
            const runId = summary.run_id;
            const what = `Moved ${summary.asset_count.toLocaleString()} ${
                summary.asset_count === 1 ? "asset" : "assets"
            } to the Trash`;
            if (runId && summary.is_undoable) {
                toast.success(what, {
                    action: { label: "Undo", onClick: () => void undoRun(runId) },
                });
            } else {
                toast.success(what);
            }
        } catch (e) {
            handleCommandError(e, "Couldn't move those assets to the Trash.");
        }
    }

    function onKeydown(e: KeyboardEvent) {
        const el = e.target as HTMLElement | null;
        if (el?.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(el?.tagName ?? "")) return;

        // Delete carries no modifier, so it's checked before the Ctrl gate.
        // Only ever moves to the Trash — the permanent version lives in the
        // Trash's own menu, behind a confirmation, and must not be one keystroke
        // away from a selection you can't fully see.
        if ((e.key === "Delete" || e.key === "Backspace") && selection.assetCount > 0) {
            e.preventDefault();
            void trashSelection();
            return;
        }

        if (!e.ctrlKey && !e.metaKey) return;

        // Ctrl+Shift+1..9 runs the action bound to that digit. Checked before the
        // unshifted bindings because `e.key` is the digit either way and only the
        // modifier tells them apart.
        if (e.shiftKey && /^[1-9]$/.test(e.key)) {
            const action = assetLibrary.quickActions.find((a) => a.shortcut === +e.key);
            if (action) {
                e.preventDefault();
                void runAction(action);
            }
            return;
        }

        if (e.key === "z" && !e.shiftKey) {
            // Undo the newest recorded run, whether it came from a quick action
            // or from a bulk drag. Deliberately NOT a general app undo stack —
            // it reverses the changes you can't see, which is the only kind
            // Nova makes invisibly.
            e.preventDefault();
            void undoLatest();
        } else if (e.key === "b") {
            e.preventDefault();
            layout.toggleSidebar();
        } else if (e.key === "i") {
            e.preventDefault();
            layout.toggleInspector();
        }
    }
</script>

<svelte:window onkeydown={onKeydown} />

<QueryClientProvider client={queryClient}>
    <!--
        ═══════════════════════════════════════════════════════════════════════
        THE SHELL — three panes, no global header.

          sidebar | grid | inspector   +   window controls (fixed overlay)

        Every pane header is --chrome-h tall so the three line up into one strip;
        that strip is the titlebar (`decorations: false`) and its empty space is
        the window drag region. Column widths come from layout.svelte.ts as the
        two inline custom properties below. See layout.css for the rest.
        ═══════════════════════════════════════════════════════════════════════
    -->
    <div
        class="layout"
        data-sidebar={layout.sidebarMode}
        data-inspector={layout.inspectorHidden ? "hidden" : "shown"}
        data-resizing={layout.resizing ? "" : undefined}
        style="--sidebar-col: {layout.sidebarCol}px; --inspector-col: {layout.inspectorCol}px"
    >
        <!-- ── SIDEBAR ──────────────────────────────────────────────────────
             Three modes (see layout.svelte.ts): expanded, rail, hidden. The
             toggle button only does shown/hidden; the rail is reached by
             dragging the resize handle past the snap threshold. -->
        <aside class="pane sidebar border-r border-neutral-800 bg-neutral-950">
            <header class="pane-header">
                <!-- Drag surface. Full-bleed and BEHIND the controls, because
                     Tauri matches the attribute on the exact element the pointer
                     hit — see the note in layout.css. -->
                <div class="drag-region" data-tauri-drag-region></div>

                {#if layout.sidebarMode === "expanded"}
                    <LibraryMenu oncreate={createLibrary} onopen={addExistingLibrary} />
                {/if}
                <div class="h-full flex-1" data-tauri-drag-region></div>
                <button
                    type="button"
                    title="Hide sidebar (Ctrl+B)"
                    aria-label="Hide sidebar"
                    onclick={() => layout.toggleSidebar()}
                    class="grid h-7 w-7 shrink-0 place-items-center rounded text-neutral-500
                           transition-colors hover:bg-neutral-800 hover:text-neutral-200"
                >
                    <PanelLeft class="h-4 w-4" />
                </button>
            </header>

            {#if layout.sidebarMode === "rail"}
                <SidebarRail onManageTags={() => (tagManagerOpen = true)} />
            {:else}
                <!-- EXPANDED — the same hierarchy the rail implies, spelled out.
                     Curated things first (smart views, then pins), the exhaustive
                     tree last where it can take the remaining height. That order
                     is what Finder, Notion and VS Code all settle on: the
                     shortlist above the structure.

                     Note the tree and the pins swap places between the two modes.
                     The rule is consistent even though the picture isn't: fixed
                     controls sit in the top group, and the ONE variable-length
                     list takes the space that's left. In the rail the tree is a
                     button; here it's a panel.

                     Auto-scrolls while a drag hovers its edges: the folder you
                     want is often below the fold, and there's no way to scroll a
                     sidebar with a pointer already holding something. -->
                <nav
                    class="flex min-h-0 flex-1 flex-col overflow-y-auto pb-2 [scrollbar-width:thin]"
                    {...{ [DRAG_SCROLL_ATTR]: "" }}
                >
                    <SystemViews variant="expanded" />

                    <div class="mx-3 my-2 h-px shrink-0 bg-neutral-800"></div>

                    <p
                        class="px-3 pb-1 text-[10px] font-semibold uppercase tracking-wider
                               text-neutral-600"
                    >
                        Pinned
                    </p>
                    <PinnedFolders variant="expanded" />

                    <div class="mx-3 my-2 h-px shrink-0 bg-neutral-800"></div>

                    <p
                        class="px-3 pb-1 text-[10px] font-semibold uppercase tracking-wider
                               text-neutral-600"
                    >
                        Smart folders
                    </p>
                    <div class="px-2">
                        <SmartFolders />
                    </div>

                    <div class="mx-3 my-2 h-px shrink-0 bg-neutral-800"></div>

                    <p
                        class="px-3 pb-1 text-[10px] font-semibold uppercase tracking-wider
                               text-neutral-600"
                    >
                        Folders
                    </p>
                    <div class="px-2">
                        <FolderTree />
                    </div>

                    <button
                        type="button"
                        onclick={() => (tagManagerOpen = true)}
                        class="mx-2 mt-2 rounded px-2 py-1.5 text-left text-sm text-neutral-400
                               transition-colors hover:bg-neutral-800 hover:text-neutral-200"
                    >
                        Manage tags…
                    </button>

                    <div class="mt-2 px-2">
                        <SavedFilters />
                    </div>
                </nav>
            {/if}

            <!-- Footer: settings live at the bottom of the sidebar, not in a
                 header — it's a destination you visit rarely, not a control. -->
            <div class="shrink-0 border-t border-neutral-800 p-2">
                <Dialog.Root>
                    <Dialog.Trigger
                        type="button"
                        class="flex w-full items-center gap-2 rounded px-2 py-1.5 text-sm
                               text-neutral-400 transition-colors hover:bg-neutral-800
                               hover:text-neutral-200 {layout.sidebarMode === 'rail'
                            ? 'justify-center'
                            : ''}"
                        aria-label="Settings"
                    >
                        <Settings class="h-4 w-4 shrink-0" />
                        {#if layout.sidebarMode === "expanded"}<span>Settings</span>{/if}
                    </Dialog.Trigger>
                    <Dialog.Content
                        class="sm:max-w-[calc(100%-10rem)] flex gap-0 p-0 overflow-hidden h-[85vh]
                               bg-neutral-950 border border-neutral-800 rounded-xl shadow-2xl flex-col"
                    >
                        <Dialog.Title class="sr-only">Settings</Dialog.Title>

                        <Tabs.Root
                            value={DEFAULT_SECTION_ID}
                            orientation="vertical"
                            class="flex flex-1 overflow-hidden h-full w-full"
                        >
                            <Tabs.List
                                class="w-52 shrink-0 h-full rounded-none border-r border-neutral-800
                                       bg-neutral-900/60 justify-start pt-4 gap-0.5 px-2"
                            >
                                <p
                                    class="px-3 pb-3 w-full text-[11px] font-semibold uppercase tracking-widest text-neutral-500"
                                >
                                    Settings
                                </p>

                                {#each SETTINGS_SECTIONS as section}
                                    {#if section.dividerAbove}
                                        <div
                                            class="my-2 mx-1 h-px w-full bg-neutral-800"
                                            aria-hidden="true"
                                        ></div>
                                    {/if}

                                    <Tabs.Trigger
                                        value={section.id}
                                        class="gap-2.5 px-3 text-neutral-400
                                               hover:bg-neutral-800/60 hover:text-neutral-200
                                               data-[state=active]:bg-neutral-800
                                               data-[state=active]:text-neutral-100"
                                    >
                                        {section.label}
                                    </Tabs.Trigger>
                                {/each}
                            </Tabs.List>

                            <!-- Content panels — each section gets its own Tabs.Content -->
                            <div class="flex flex-col flex-1 min-w-0 overflow-hidden h-full">
                                {#each SETTINGS_SECTIONS as section}
                                    {@const SectionComponent = sectionComponents[section.id]}
                                    <Tabs.Content
                                        value={section.id}
                                        class="flex flex-col flex-1 overflow-hidden mt-0"
                                    >
                                        <div class="px-6 py-4 border-b border-neutral-800 shrink-0">
                                            <h2 class="text-sm font-semibold text-neutral-100">
                                                {section.label}
                                            </h2>
                                        </div>
                                        <div
                                            class="flex-1 overflow-y-auto px-6 py-5 text-neutral-200"
                                        >
                                            <SectionComponent />
                                        </div>
                                    </Tabs.Content>
                                {/each}
                            </div>
                        </Tabs.Root>
                    </Dialog.Content>
                </Dialog.Root>
            </div>

            <ResizeHandle
                edge="left"
                value={layout.sidebarWidth}
                label="Resize sidebar"
                onresize={(px) => layout.resizeSidebar(px)}
            />
        </aside>

        <!-- ── GRID ─────────────────────────────────────────────────────────
             One header row: place + count · drag region · search · view controls.
             Everything below it is assets. -->
        <main class="pane grid-pane bg-neutral-950">
            <header class="pane-header border-b border-neutral-800">
                <div class="drag-region" data-tauri-drag-region></div>

                {#if layout.sidebarHidden}
                    <!-- The way back. Sits at the same x the sidebar's own toggle
                         does, so the control never appears to move — only to
                         change sides of the border. -->
                    <button
                        type="button"
                        title="Show sidebar (Ctrl+B)"
                        aria-label="Show sidebar"
                        onclick={() => layout.toggleSidebar()}
                        class="grid h-7 w-7 shrink-0 place-items-center rounded text-neutral-500
                               transition-colors hover:bg-neutral-800 hover:text-neutral-200"
                    >
                        <PanelLeft class="h-4 w-4" />
                    </button>
                {/if}

                <!-- Labels carry the attribute too: they're not interactive, so
                     there's no reason a click on the title shouldn't drag. -->
                <span
                    class="shrink-0 truncate px-1 text-sm font-medium text-neutral-200"
                    data-tauri-drag-region
                >
                    {scopeLabel}
                </span>
                <!-- Say "(filtered)" when narrowed, so a small view never reads as
                     a small library. -->
                <span class="shrink-0 text-xs text-neutral-500" data-tauri-drag-region>
                    {assetLibrary.displayed.length}
                    {assetLibrary.hasFilters || assetLibrary.nameFiltering ? "filtered" : "assets"}
                    {#if selection.assetCount > 0}
                        <span class="text-blue-400">· {selection.assetCount} selected</span>
                    {/if}
                </span>

                <div class="h-full flex-1" data-tauri-drag-region></div>

                <!-- Search sits with the grid, not in the sidebar: it's a lens on
                     the current scope, not a way to jump somewhere else. -->
                <div class="w-64 shrink-0"><SearchBar /></div>

                <button
                    type="button"
                    onclick={handleImport}
                    disabled={isImporting || noLibraryConnected}
                    title="Import a folder"
                    aria-label="Import"
                    class="grid h-7 w-7 shrink-0 place-items-center rounded text-neutral-500
                           transition-colors hover:bg-neutral-800 hover:text-neutral-200
                           disabled:opacity-40"
                >
                    <Download class="h-4 w-4" />
                </button>

                <GridToolbar />

                {#if layout.inspectorHidden}
                    <button
                        type="button"
                        title="Show inspector (Ctrl+I)"
                        aria-label="Show inspector"
                        onclick={() => layout.toggleInspector()}
                        class="grid h-7 w-7 shrink-0 place-items-center rounded text-neutral-500
                               transition-colors hover:bg-neutral-800 hover:text-neutral-200"
                    >
                        <PanelRight class="h-4 w-4" />
                    </button>
                {/if}
            </header>

            {#if noLibraryConnected}
                <!-- Persistent state, not a transient event: nothing in the app
                     works until this is resolved, so it takes the whole pane
                     instead of being an alert bar above a dead grid. -->
                <div class="flex flex-1 flex-col items-center justify-center gap-4 text-center">
                    <div>
                        <p class="text-sm font-medium text-neutral-200">No library connected</p>
                        <p class="mt-1 text-xs text-neutral-500">
                            Create a new library or open an existing one to get started.
                        </p>
                    </div>
                    <div class="flex gap-2">
                        <Button onclick={createLibrary}>New library</Button>
                        <Button variant="secondary" onclick={addExistingLibrary}>
                            Open existing…
                        </Button>
                    </div>
                </div>
            {:else}
                <AssetGrid />
            {/if}
        </main>

        <!-- ── INSPECTOR ────────────────────────────────────────────────────
             Always mounted, width-collapsed when hidden: remounting would reset
             its scroll and drop in-flight field writes. -->
        <aside class="pane inspector border-l border-neutral-800 bg-neutral-950">
            <header class="pane-header border-b border-neutral-800">
                <div class="drag-region" data-tauri-drag-region></div>

                <span class="truncate text-xs text-neutral-400" data-tauri-drag-region>
                    {inspectorLabel}
                </span>
                <div class="h-full flex-1" data-tauri-drag-region></div>
                <button
                    type="button"
                    title="Hide inspector (Ctrl+I)"
                    aria-label="Hide inspector"
                    onclick={() => layout.toggleInspector()}
                    class="grid h-7 w-7 shrink-0 place-items-center rounded text-neutral-500
                           transition-colors hover:bg-neutral-800 hover:text-neutral-200"
                >
                    <PanelRight class="h-4 w-4" />
                </button>
            </header>

            <div class="flex-1 overflow-y-auto [scrollbar-width:thin]">
                <Inspector />
            </div>

            <ResizeHandle
                edge="right"
                value={layout.inspectorWidth}
                label="Resize inspector"
                onresize={(px) => layout.resizeInspector(px)}
            />
        </aside>

        <!-- Window buttons. A sibling of the three panes and always mounted, so
             hiding the inspector can never take Close away with it. The panes
             only reserve the gutter (--wc-w in layout.css). -->
        <WindowControls />
    </div>

    {#if tagManagerOpen}
        <TagManager onClose={() => (tagManagerOpen = false)} />
    {/if}

    <!-- ── Floating status, bottom-right ────────────────────────────────────
         Outside the panes on purpose: progress belongs to the app, not to a
         column that might be collapsed when it arrives. -->
    {#if isImporting}
        <div
            class="fixed bottom-4 right-4 z-40 w-72 space-y-3 rounded-lg border border-neutral-700
                   bg-neutral-900/95 p-4 shadow-xl"
        >
            <div class="flex items-start justify-between gap-3">
                <div class="flex min-w-0 flex-col">
                    <span class="text-[10px] font-bold uppercase tracking-wider text-blue-400">
                        {currentStage ?? "Starting"}
                    </span>
                    <span class="truncate text-xs text-neutral-300">{statusMessage}</span>
                </div>
                <div class="shrink-0 text-right">
                    <span class="font-mono text-sm font-bold text-neutral-100">
                        {displayPercent}%
                    </span>
                    <p class="text-[10px] text-neutral-500">{current} / {total}</p>
                </div>
            </div>

            <div class="h-1.5 w-full overflow-hidden rounded-full bg-neutral-800">
                <div class="h-full bg-blue-500" style="width: {$smoothPercentage}%"></div>
            </div>

            <p class="text-[10px] italic text-neutral-500">Don't close the app while importing.</p>
        </div>
    {:else if assetLibrary.thumbProgress}
        <!-- Background thumbnail generation (Rebuild / large runs). -->
        <div
            class="fixed bottom-4 right-4 z-40 flex items-center gap-3 rounded-lg border
                   border-neutral-700 bg-neutral-900/95 px-4 py-2.5 text-neutral-200 shadow-xl"
        >
            <span class="h-2 w-2 shrink-0 animate-pulse rounded-full bg-blue-500"></span>
            <div class="flex flex-col gap-1">
                <span class="text-xs">
                    Generating thumbnails… {assetLibrary.thumbProgress.current}/{assetLibrary
                        .thumbProgress.total}
                </span>
                <div class="h-1 w-40 overflow-hidden rounded-full bg-neutral-800">
                    <div
                        class="h-full bg-blue-500 transition-all"
                        style="width: {(assetLibrary.thumbProgress.current /
                            assetLibrary.thumbProgress.total) *
                            100}%"
                    ></div>
                </div>
            </div>
        </div>
    {/if}

    <!-- Drag preview. Follows the cursor because a pointer drag moves no DOM of
         its own — unlike HTML5, nothing is dragged for us. `pointer-events-none`
         is required, not cosmetic: elementFromPoint skips it, so the preview
         never becomes its own drop target. -->
    {#if drag.active && drag.payload}
        {@const over = drag.target?.kind === "folder" ? drag.target : null}
        {@const smart = drag.target?.kind === "smart" ? drag.target : null}
        <div
            class="pointer-events-none fixed z-[100] flex items-center gap-2 rounded-md px-2.5
                   py-1.5 text-xs font-medium text-white shadow-lg
                   {drag.forbidden ? 'bg-red-700' : over ? 'bg-emerald-600' : 'bg-neutral-700'}"
            style="left: {drag.x + 14}px; top: {drag.y + 14}px"
        >
            {#if smart}
                <!-- Says what the MECHANISM is, not what the asset is.
                     "Doesn't match the rules" would be a diagnosis we can't
                     cheaply make — and often false, since a dragged asset may
                     already match and be in there. A wrong explanation is worse
                     than a plain refusal. -->
                <span>Smart folders collect matching assets automatically</span>
            {:else if drag.payload.kind === "assets"}
                <span>{drag.count} {drag.count === 1 ? "asset" : "assets"}</span>
                {#if over}
                    <!-- Naming the action AND the destination, because add and
                         move differ only by a held key and only one is reversible. -->
                    <span class="opacity-80">
                        {drag.move ? "move to" : "add to"} "{over.name}"
                    </span>
                {/if}
            {:else}
                <span>📁 {drag.payload.name}</span>
                {#if drag.forbidden}
                    <span class="opacity-80">can't go inside itself</span>
                {:else if over}
                    <!-- The zone IS the operation, so it has to be said out loud:
                         the same row means two different things depending on
                         where in it you release. -->
                    <span class="opacity-80">
                        {over.zone === "into" ? `into "${over.name}"` : "reorder"}
                    </span>
                {/if}
            {/if}
        </div>
    {/if}
</QueryClientProvider>
