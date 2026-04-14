<script lang="ts">
    import { invoke } from "@tauri-apps/api/core";
    import { open } from "@tauri-apps/plugin-dialog";

    let name = $state("");
    let greetMsg = $state("");

    async function greet(event: Event) {
        event.preventDefault();
        // Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
        greetMsg = await invoke("greet", { name });
    }
    import { Button } from "$components/ui/button";
    import { Skeleton } from "$components/ui/skeleton";

    async function createLibrary() {
        // TODO: use a custom dialog
        const name = prompt("Library name:");
        if (!name) return;

        const dir = await open({ directory: true, multiple: false });
        if (!dir) return;

        const path = `${dir}\\${name}.library`;

        console.log("path:", path);

        try {
            const dbPath = await invoke<string>("create_library", { libraryPath: path });

            // Todo: initialize database for the library here
        } catch (e) {
            console.error("Error creating library:", e);
        }
    }
</script>

<main class="container">
    <Button class="text-blue-500">Hello World</Button>
    <skeleton>saasssa</skeleton>

    <Button onclick={createLibrary}>Create a new library</Button>
</main>

<style>
</style>
