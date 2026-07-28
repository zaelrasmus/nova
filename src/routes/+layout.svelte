<!--
  Root layout. Nova is a single-route app, so this only loads global CSS and
  mounts the toast host — all real UI is in +page.svelte.
-->
<script lang="ts">
    import "./layout.css";
    import { Toaster } from "$components/ui/sonner/index.js";

    const { children } = $props();
</script>

<Toaster />

{@render children()}
