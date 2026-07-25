<script lang="ts">
    import { onMount } from "svelte";
    import { invoke } from "@tauri-apps/api/core";
    import { listen } from "@tauri-apps/api/event";
    import { open } from "@tauri-apps/plugin-dialog";
    import { cubicOut } from "svelte/easing";
    import { tweened } from "svelte/motion";
    import { QueryClient, QueryClientProvider } from "@tanstack/svelte-query";
    import { assetLibrary, type ManifestScope } from "$lib/assets.svelte";
    import { dropzone } from "$lib/dropzone.svelte";
    import type { DropTarget } from "$lib/droptarget";
    import { drag, DRAG_SCROLL_ATTR } from "$lib/dragdrop.svelte";

    import { toast } from "svelte-sonner";
    import { Alert, AlertDescription, AlertTitle } from "$components/ui/alert";
    import { Button, buttonVariants } from "$components/ui/button";
    import AssetGrid from "$components/AssetGrid.svelte";
    import FolderTree from "$components/FolderTree.svelte";
    import SavedFilters from "$components/SavedFilters.svelte";
    import Inspector from "$components/Inspector.svelte";
    import TagManager from "$components/TagManager.svelte";
    import { libraryManager, settings } from "../routes/settings.svelte";

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

    // Commands

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
                success: (info) => `Library "${name}" created successfully.`,
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

    async function handleConnect() {
        const selected = await open({
            directory: true,
            multiple: false,
            title: "Select library folder",
        });
        if (!selected) return;

        try {
            await invoke<string>("connect_library", { libraryPath: selected });
            toast.success("Library connected successfully.");
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
                const base = `Imported ${result.assets.length} assets across ${result.folders.length} folders.`;
                // Always say so when files were skipped — otherwise re-importing
                // a folder looks like the import silently failed.
                return result.duplicates > 0
                    ? `${base} Skipped ${result.duplicates} already in the library.`
                    : base;
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
        if (isImporting) {
            // The pipeline holds a single DB handle and one progress channel; a
            // second concurrent import would interleave both.
            toast.error("An import is already running.");
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

        await runImport("import_assets", { sourcePath: selectedSource, importFolders }, {
            kind: "all",
        });
    }

    async function runInjection() {
        const assetName = window.prompt("Asset name to inject:");
        if (!assetName) return;

        try {
            await invoke<string>("inject_test_asset", { name: assetName });
            toast.success(`Test asset "${assetName}" injected.`);
        } catch (e) {
            handleCommandError(e);
        }
    }

    async function runFetch() {
        try {
            const assets = await invoke<any[]>("fetch_assets");
            console.table(assets);
            toast.info(`Found ${assets.length} assets. Check the console for details.`);
        } catch (e) {
            handleCommandError(e);
        }
    }

    // As background thumbnails finish, patch the just-completed rows into the
    // manifest in place (ThumbHash placeholder appears, then the real thumbnail
    // re-hydrates). No full reload → no grid flash.
    onMount(() => {
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

    // Alert is shown as persistent inline state — not a toast —
    // because "no library connected" requires user action before anything else works.
    let noLibraryConnected = $derived(!libraryManager.state.activeLibrary);

    // The Tag Manager is a full-screen view, opened over the library.
    let tagManagerOpen = $state(false);

    import * as Dialog from "$components/ui/dialog";
    import * as Tabs from "$components/ui/tabs";
    import { SETTINGS_SECTIONS, DEFAULT_SECTION_ID } from "./settings-sections";

    // import { Settings, Palette, FolderInput, LayoutGrid, Library, Info } from "lucide-svelte";

    // To add a new section: import its component and register it below.
    import AppearanceSection from "../components/settings/AppearanceSection.svelte";
    import ImportSection from "../components/settings/ImportSection.svelte";
    import DisplaySection from "../components/settings/DisplaySection.svelte";

    const sectionComponents: Record<string, any> = {
        appearance: AppearanceSection,
        import: ImportSection,
        display: DisplaySection,
    };

    // const iconComponents: Record<string, any> = {
    //     Palette,
    //     FolderInput,
    //     LayoutGrid,
    //     Library,
    //     Info,
    // };
</script>

<QueryClientProvider client={queryClient}>
    <main class="container">
        <!--
            Alert = persistent state warning, not a transient event.
            The user must address this before they can do anything meaningful.
            This stays visible until they connect or create a library.
        -->
        {#if noLibraryConnected}
            <Alert variant="destructive">
                <AlertTitle>No library connected</AlertTitle>
                <AlertDescription>
                    Create a new library or open an existing one to get started.
                </AlertDescription>
            </Alert>
        {/if}

        <Dialog.Root>
            <Dialog.Trigger type="button" class={buttonVariants({ variant: "ghost" })}>
                Open Dialog Settings
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
                                <!-- <svelte:component
                                    this={iconComponents[section.icon]}
                                    class="h-4 w-4 shrink-0"
                                /> -->
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
                                <div class="flex-1 overflow-y-auto px-6 py-5 text-neutral-200">
                                    <SectionComponent />
                                </div>
                            </Tabs.Content>
                        {/each}
                    </div>
                </Tabs.Root>
            </Dialog.Content>
        </Dialog.Root>

        <Button onclick={createLibrary}>Create a new library</Button>

        <label class="flex items-center gap-2 text-sm text-neutral-300">
                    <input type="checkbox" bind:checked={importFolders} disabled={isImporting} />
                    Import folder structure
                </label>

        <Button onclick={handleImport} disabled={isImporting}>
            {isImporting ? "🚀 Importing..." : "📥 Import Assets"}
        </Button>

        <Button onclick={handleConnect}>Connect to library</Button>
        <Button onclick={runInjection}>Inject test asset</Button>
        <Button onclick={runFetch}>Fetch assets</Button>

        <div class="library-panel p-6 bg-gray-900 text-white rounded-xl shadow-2xl w-96">
            <h2 class="text-xl font-bold mb-4">Libraries</h2>

            <div class="mb-6 p-3 bg-gray-800 rounded-lg border border-blue-500">
                <p class="text-xs text-blue-400 font-bold uppercase">Active</p>
                <p class="truncate text-sm">
                    {libraryManager.state.activeLibrary ?? "None selected"}
                </p>
            </div>

            <div class="space-y-2 mb-6">
                <p class="text-xs text-gray-500 font-bold uppercase">Recent</p>
                {#each libraryManager.state.history as path}
                    <div
                        class="group flex items-center justify-between bg-gray-800 p-2 rounded hover:bg-gray-700 transition-colors"
                    >
                        <button
                            onclick={() => libraryManager.switchLibrary(path)}
                            class="flex-1 text-left text-sm truncate mr-2"
                        >
                            {path.split("/").pop() || path}
                        </button>
                        <button
                            onclick={() => libraryManager.removeFromHistory(path)}
                            class="opacity-0 group-hover:opacity-100 text-red-400 hover:text-red-300 text-xs px-2"
                        >
                            Remove
                        </button>
                    </div>
                {/each}
            </div>

            <div class="flex flex-col gap-2">
                <button
                    onclick={createLibrary}
                    class="w-full py-2 bg-blue-600 hover:bg-blue-500 rounded font-bold transition-all"
                >
                    ✨ New Library
                </button>
                <button
                    onclick={addExistingLibrary}
                    class="w-full py-2 bg-gray-700 hover:bg-gray-600 rounded font-bold transition-all"
                >
                    📂 Open Existing
                </button>
            </div>
        </div>

        {#if isImporting}
            <div class="p-4 bg-gray-800 rounded-lg border border-gray-700 space-y-4">
                <div class="flex justify-between items-center">
                    <div class="flex flex-col">
                        <span class="text-xs font-bold uppercase tracking-wider text-blue-400">
                            {currentStage ?? "Starting"}
                        </span>
                        <span class="text-sm text-gray-200">{statusMessage}</span>
                    </div>
                    <div class="text-right">
                        <span class="text-lg font-mono font-bold">{displayPercent}%</span>
                        <p class="text-[10px] text-gray-400">{current} of {total} objects</p>
                    </div>
                </div>

                <div class="relative w-full h-3 bg-gray-900 rounded-full overflow-hidden">
                    <div
                        class="h-full bg-blue-500 shadow-[0_0_10px_rgba(59,130,246,0.5)] transition-none"
                        style="width: {$smoothPercentage}%"
                    ></div>
                    {#if currentStage === "Scanning"}
                        <div
                            class="absolute inset-0 bg-linear-to-r from-transparent via-white/10 to-transparent animate-shimmer"
                        ></div>
                    {/if}
                </div>

                <p class="text-[11px] text-gray-500 italic">
                    Don't close the app while importing.
                </p>
            </div>
        {/if}

        <!-- (h-[70vh] is pragmatic; long-term make <main> a h-screen flex flex-col and this flex-1 min-h-0. Any bounded height works.) -->
        <div class="mt-8 flex gap-4 h-[70vh]">
            <!-- Auto-scrolls while a drag hovers its edges: the folder you want
                 is often below the fold, and there's no way to scroll a sidebar
                 with a pointer already holding something. -->
            <div
                class="flex w-56 shrink-0 flex-col gap-3 overflow-y-auto"
                {...{ [DRAG_SCROLL_ATTR]: "" }}
            >
                <FolderTree />
                <SavedFilters />
                <button
                    type="button"
                    onclick={() => (tagManagerOpen = true)}
                    class="rounded-lg border border-neutral-800 bg-neutral-900/40 px-3 py-2 text-left
                           text-sm text-neutral-300 transition-colors hover:bg-neutral-800"
                >
                    🏷 Manage tags
                </button>
            </div>
            <div class="flex-1 min-w-0">
                <AssetGrid />
            </div>
            <!-- Right-hand column, the conventional DAM position. Rendered
                 unconditionally so it never remounts on selection change —
                 remounting would reset scroll and, once fields are editable,
                 drop in-flight writes. -->
            <div class="w-72 shrink-0">
                <Inspector />
            </div>
        </div>

        <!-- Drag preview. Follows the cursor because a pointer drag moves no
             DOM of its own — unlike HTML5, nothing is dragged for us.
             `pointer-events-none` is required, not cosmetic: elementFromPoint
             skips it, so the preview never becomes its own drop target. -->
        {#if drag.active && drag.payload}
            {@const over = drag.target?.kind === "folder" ? drag.target : null}
            <div
                class="pointer-events-none fixed z-[100] flex items-center gap-2 rounded-md px-2.5
                       py-1.5 text-xs font-medium text-white shadow-lg
                       {drag.forbidden ? 'bg-red-700' : over ? 'bg-emerald-600' : 'bg-neutral-700'}"
                style="left: {drag.x + 14}px; top: {drag.y + 14}px"
            >
                {#if drag.payload.kind === "assets"}
                    <span>{drag.count} {drag.count === 1 ? "asset" : "assets"}</span>
                    {#if over}
                        <!-- Naming the action AND the destination, because add
                             and move differ only by a held key and only one is
                             reversible. -->
                        <span class="opacity-80">
                            {drag.move ? "move to" : "add to"} "{over.name}"
                        </span>
                    {/if}
                {:else}
                    <span>📁 {drag.payload.name}</span>
                    {#if drag.forbidden}
                        <span class="opacity-80">can't go inside itself</span>
                    {:else if over}
                        <!-- The zone IS the operation, so it has to be said out
                             loud: the same row means two different things
                             depending on where in it you release. -->
                        <span class="opacity-80">
                            {over.zone === "into" ? `into "${over.name}"` : "reorder"}
                        </span>
                    {/if}
                {/if}
            </div>
        {/if}

        <!-- Background thumbnail generation (Rebuild / large runs). -->
        {#if assetLibrary.thumbProgress}
            <div
                class="fixed bottom-4 right-4 z-50 flex items-center gap-3 rounded-lg border border-neutral-700
                       bg-neutral-900/95 px-4 py-2.5 text-neutral-200 shadow-xl"
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
    </main>

    {#if tagManagerOpen}
        <TagManager onClose={() => (tagManagerOpen = false)} />
    {/if}
</QueryClientProvider>
