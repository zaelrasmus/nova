<!--
  Settings › About.

  Exists for one practical reason: a beta tester reporting a bug has to be able
  to say WHICH build they are on. There is no auto-updater yet and no log file on
  disk, so this version string is the only thing tying a report to a binary.

  `getVersion()` reads the version from tauri.conf.json, so it can never drift
  from what was actually built.
-->
<script lang="ts">
    import { getVersion } from "@tauri-apps/api/app";

    // Resolved at runtime, so a stale build can't show a stale number.
    let version = $state<string | null>(null);
    let versionError = $state(false);

    void getVersion()
        .then((v) => (version = v))
        .catch(() => (versionError = true));
</script>

<div class="flex flex-col gap-6">
    <div class="flex flex-col gap-1">
        <span class="text-sm font-medium text-neutral-100">Nova</span>
        <span class="text-xs tabular-nums text-neutral-500">
            {#if version}
                Version {version}
            {:else if versionError}
                Version unavailable
            {:else}
                Loading…
            {/if}
        </span>
    </div>

    <div class="h-px bg-neutral-800" aria-hidden="true"></div>

    <p class="max-w-prose text-xs leading-relaxed text-neutral-500">
        A media asset manager built for large libraries. This is a pre-release
        build — please include the version number above when reporting anything
        that looks wrong.
    </p>
</div>
