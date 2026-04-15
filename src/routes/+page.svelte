<script lang="ts">
    import { invoke } from "@tauri-apps/api/core";
    import { open } from "@tauri-apps/plugin-dialog";

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

            // Todo: initialize database for the library here
        } catch (e) {
            console.error("Error creating library:", e);
        }
    }

    async function handleImport() {
        // 1. Select folder where images are located
        const selectedSource = await open({
            directory: true,
            multiple: false,
            title: "Select folder where images are located",
        });

        if (!selectedSource) return;

        // 2. Select folder where the library is located
        // TODO: "libraryPath" should come from the global state
        const selectedLibrary = await open({
            directory: true,
            multiple: false,
            title: "Select folder where the library is located)",
        });

        if (!selectedLibrary) return;

        try {
            console.log("Init import...");
            const result = await invoke<ImportResult>("import_assets", {
                sourcePath: selectedSource,
                libraryPath: selectedLibrary,
            });

            console.log("Importation successful:", result);
            alert(
                `impoted ${result.assets.length} assets y ${result.folders.length} folders.`,
            );
        } catch (error) {
            console.error("Error importing:", error);
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
</script>

<main class="container">
    <Button class="text-blue-500">Hello World</Button>
    <skeleton>saasssa</skeleton>

    <Button onclick={createLibrary}>Create a new library</Button>

    <Button onclick={handleImport}>Add Assets</Button>

    <Button onclick={handleConnect}>Connect to library</Button>

    <Button onclick={runInjection}>Inject test Asset in current library</Button>
    <Button onclick={runFetch}>Run fetch for library</Button>
</main>

<style>
</style>
