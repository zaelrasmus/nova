<script lang="ts">
    import { invoke } from "@tauri-apps/api/core";

    let name = $state("");
    let greetMsg = $state("");

    async function greet(event: Event) {
        event.preventDefault();
        // Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
        greetMsg = await invoke("greet", { name });
    }
    import { Button } from "$components/ui/button";
    import { Skeleton } from "$components/ui/skeleton";
</script>

<main class="container">
    <Button class="text-blue-500">Hello World</Button>
    <skeleton>aaa</skeleton>
</main>

<style>
</style>
