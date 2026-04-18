<script lang="ts">
    import { invoke } from "@tauri-apps/api/core";
    import { open } from "@tauri-apps/plugin-dialog";
    import { libraryManager } from "../routes/settings.svelte";

    interface LibraryInfo {
        db_path: string;
        root_path: string;
    }

    interface AssetsImportData {
        id: string;
        path: string;
        filename: string;
        width: number;
        height: number;
        parent_dir_path: string;
        source_path: string; // Add the new field
        imported_date: string;
        modified_date: string;
        creation_date: string;
    }

    interface FolderImportData {
        id: string;
        path: string;
        name: string;
        parent_id: string | null;
        order_by: string;
        is_ascending: number;
        /*  description: string | null; */
    }

    interface ImportResult {
        folders: FolderImportData[];
        assets: AssetsImportData[];
        path_links: { [key: string]: string };
    }

    type ImportStage = "Scanning" | "ProcessingMetadata" | "CopyingFiles" | "Finalizing";

    interface ImportProgress {
        stage: ImportStage;
        current: number;
        total: number;
        message: string;
    }

    interface ImportAssetsResult {
        assets: any[]; // Sustituir por interfaz de AssetMetadata
        folders: any[];
    }

    import { Button } from "$components/ui/button";
    import { Skeleton } from "$components/ui/skeleton";

    async function createLibrary() {
        // TODO: use a custom dialog
        const name = prompt("Library name:");
        if (!name) return;

        const location = await open({ directory: true, multiple: false });
        if (!location) return;

        console.log("path:", location);

        try {
            const dbPath = await invoke<LibraryInfo>("create_library", { location, name });

            console.log("dbPath:", dbPath);

            await libraryManager.switchLibrary(dbPath.root_path);
            alert("Library created and selected sucessfully");
        } catch (e) {
            console.error("Error creating library:", e);
        }
    }

    async function addExistingLibrary() {
        const selected = await open({ directory: true, multiple: false });
        if (selected) await libraryManager.switchLibrary(selected);
    }

    let isImporting = $state(false);

    async function handleImport() {
        // 1. Select folder where images are located
        const selectedSource = await open({
            directory: true,
            multiple: false,
            title: "Select folder where images are located",
        });

        if (!selectedSource) return;

        // Reset de estados
        current = 0;
        total = 0;
        currentStage = null;
        statusMessage = "Preparing...";
        smoothPercentage.set(0);
        isImporting = true;

        const unlisten = await listen<ImportProgress>("import-progress", (event) => {
            const payload = event.payload;

            current = payload.current;
            total = payload.total;
            statusMessage = payload.message;
            currentStage = payload.stage;

            // Calculamos el porcentaje localmente para asegurar suavidad.
            if (payload.total > 0) {
                const p = (payload.current / payload.total) * 100;
                smoothPercentage.set(p);
            } else {
                smoothPercentage.set(0);
            }
        });

        try {
            console.log("Init import...");
            const result = await invoke<ImportResult>("import_assets", {
                sourcePath: selectedSource,
            });

            await queryClient.invalidateQueries({ queryKey: ["assets"] });
            console.log("Importation successful:", result);
            alert(
                `impoted ${result.assets.length} assets y ${result.folders.length} folders.`,
            );
        } catch (error) {
            console.error("Error importing:", error);
            alert("Error crítico: " + error);
        } finally {
            isImporting = false;
            unlisten();
        }
    }

    async function handleConnect() {
        try {
            const selected = await open({
                directory: true,
                multiple: false,
                title: "Select library folder",
            });

            if (!selected) return;

            console.log("selected:", selected);

            const response = await invoke<string>("connect_library", {
                libraryPath: selected,
            });

            console.log(
                "[Se supone que ha sido conectado exitosamente a esta base de datos] response:",
                response,
            );
        } catch (e) {
            console.error(e);
        }
    }

    let testName = $state("Mi imagen de Prueba");
    let logOutput = $state("");

    async function runInjection() {
        // Usamos el prompt nativo del navegador
        const assetName = window.prompt("Introduce el nombre del asset para inyectar:");

        if (!assetName) return; // Si cancela o está vacío

        try {
            const response = await invoke<string>("inject_test_asset", { name: assetName });
            alert(response);
        } catch (error) {
            alert(`Error de inyección: ${error}`);
            console.log(error);
        }
    }

    async function runFetch() {
        try {
            const assets = await invoke<any[]>("fetch_assets");
            console.log("--- CONTENIDO DE LA LIBRERÍA ACTUAL ---");
            console.table(assets);
            alert(`Se encontraron ${assets.length} assets. Revisa la consola (F12).`);
        } catch (error) {
            alert(`Error al obtener assets: ${error}`);
            console.log(error);
        }
    }

    import { QueryClient, QueryClientProvider } from "@tanstack/svelte-query";
    import AssetGrid from "$components/AssetGrid.svelte";

    const queryClient = new QueryClient({
        defaultOptions: {
            queries: {
                refetchOnWindowFocus: false,
            },
        },
    });

    import { listen } from "@tauri-apps/api/event";
    import { cubicOut } from "svelte/easing";
    import { tweened } from "svelte/motion";

    let current = $state(0);
    let total = $state(0);
    let statusMessage = $state("Preparing...");
    let currentStage = $state<ImportStage | null>(null);

    // Barra de progreso suave
    const smoothPercentage = tweened(0, {
        duration: 400,
        easing: cubicOut,
    });

    // Derivamos el porcentaje para mostrarlo en texto
    let displayPercent = $derived(Math.round($smoothPercentage));
</script>

<QueryClientProvider client={queryClient}>
    <main class="container">
        <Button class="text-blue-500">Hello World</Button>
        <skeleton>saasssa</skeleton>

        <Button onclick={createLibrary}>Create a new library</Button>

        <Button onclick={handleImport} disabled={isImporting}>
            {isImporting ? "🚀 Importing..." : "📥 Import Assets"}
        </Button>

        <Button onclick={handleConnect}>Connect to library</Button>

        <Button onclick={runInjection}>Inject test Asset in current library</Button>
        <Button onclick={runFetch}>Run fetch for library</Button>

        <div class="library-panel p-6 bg-gray-900 text-white rounded-xl shadow-2xl w-96">
            <h2 class="text-xl font-bold mb-4">Librerías</h2>

            <div class="mb-6 p-3 bg-gray-800 rounded-lg border border-blue-500">
                <p class="text-xs text-blue-400 font-bold uppercase">Activa</p>
                <p class="truncate text-sm">
                    {libraryManager.state.activeLibrary ?? "Ninguna seleccionada"}
                </p>
            </div>

            <div class="space-y-2 mb-6">
                <p class="text-xs text-gray-500 font-bold uppercase">Recientes</p>
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
                            Remover
                        </button>
                    </div>
                {/each}
            </div>

            <div class="flex flex-col gap-2">
                <button
                    onclick={createLibrary}
                    class="w-full py-2 bg-blue-600 hover:bg-blue-500 rounded font-bold transition-all"
                >
                    ✨ Crear Nueva Librería
                </button>
                <button
                    onclick={addExistingLibrary}
                    class="w-full py-2 bg-gray-700 hover:bg-gray-600 rounded font-bold transition-all"
                >
                    📂 Abrir Existente
                </button>
            </div>
        </div>
        {#if isImporting}
            <div class="p-4 bg-gray-800 rounded-lg border border-gray-700 space-y-4">
                <div class="flex justify-between items-center">
                    <div class="flex flex-col">
                        <span class="text-xs font-bold uppercase tracking-wider text-blue-400">
                            {currentStage ?? "Iniciando"}
                        </span>
                        <span class="text-sm text-gray-200">{statusMessage}</span>
                    </div>

                    <div class="text-right">
                        <span class="text-lg font-mono font-bold">{displayPercent}%</span>
                        <p class="text-[10px] text-gray-400">
                            {current} of {total} objects
                        </p>
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
                    Dont Close the App while importing
                </p>
            </div>
        {/if}
        <div class="mt-8">
            <AssetGrid />
        </div>
    </main>
</QueryClientProvider>

<style>
</style>
