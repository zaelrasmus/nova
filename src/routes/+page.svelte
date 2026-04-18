<script lang="ts">
    import { invoke } from "@tauri-apps/api/core";
    import { listen } from "@tauri-apps/api/event";
    import { open } from "@tauri-apps/plugin-dialog";
    import { cubicOut } from "svelte/easing";
    import { tweened } from "svelte/motion";
    import { QueryClient, QueryClientProvider } from "@tanstack/svelte-query";

    import { toast } from "svelte-sonner";
    import { Alert, AlertDescription, AlertTitle } from "$components/ui/alert";
    import { Button } from "$components/ui/button";
    import AssetGrid from "$components/AssetGrid.svelte";
    import { libraryManager } from "../routes/settings.svelte";

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

    async function handleImport() {
        const selectedSource = await open({
            directory: true,
            multiple: false,
            title: "Select folder to import",
        });
        if (!selectedSource) return;

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

        const importPromise = invoke<ImportResult>("import_assets", {
            sourcePath: selectedSource,
        }).then(async (result) => {
            await queryClient.invalidateQueries({ queryKey: ["assets"] });
            return result; // pass through so toast.promise still receives it
        });

        toast.promise<ImportResult>(importPromise, {
            loading: "Import in progress...",
            success: (result) =>
                `Imported ${result.assets.length} assets across ${result.folders.length} folders.`,
            error: (e) => (typeof e === "string" ? e : "Import failed. Please try again."),
        });
        // We can't await toast.promise directly and also run finally,
        // so we manage isImporting with the underlying invoke call.
        try {
            await invoke<ImportResult>("import_assets", { sourcePath: selectedSource });
        } catch {
            // Error is already handled by toast.promise above.
        } finally {
            isImporting = false;
            unlisten();
        }
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

    // Alert is shown as persistent inline state — not a toast —
    // because "no library connected" requires user action before anything else works.
    let noLibraryConnected = $derived(!libraryManager.state.activeLibrary);
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

        <Button onclick={createLibrary}>Create a new library</Button>

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

        <div class="mt-8">
            <AssetGrid />
        </div>
    </main>
</QueryClientProvider>
