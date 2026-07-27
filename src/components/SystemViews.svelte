<script lang="ts">
    import { Layers, Inbox, Trash2 } from "@lucide/svelte";
    import { assetLibrary, type ManifestScope } from "$lib/assets.svelte";
    import { selection } from "$lib/selection.svelte";

    /**
     * The library's smart views — places that aren't folders.
     *
     * Split out of FolderTree so the sidebar can state the distinction it wants
     * the user to learn: these are SCOPES the library computes, the tree is
     * STRUCTURE the user built. They also need a glyph that isn't a folder, or
     * the rail ends up with two folder icons meaning different things.
     */
    interface Props {
        variant: "rail" | "expanded";
    }

    const { variant }: Props = $props();

    const scope = $derived(assetLibrary.scope);

    const VIEWS: { scope: ManifestScope; icon: typeof Layers; label: string }[] = [
        { scope: { kind: "all" }, icon: Layers, label: "All assets" },
        { scope: { kind: "uncategorized" }, icon: Inbox, label: "Uncategorized" },
        { scope: { kind: "trash" }, icon: Trash2, label: "Trash" },
    ];

    /**
     * The Trash carries a count; the other views don't.
     *
     * Deliberately asymmetric — "All: 40,000" is noise, but the Trash holding
     * things you meant to delete is exactly what you'd otherwise forget, and the
     * badge is the only prompt to go and empty it.
     */
    const badge = (kind: ManifestScope["kind"]) =>
        kind === "trash" && assetLibrary.trashCount > 0
            ? assetLibrary.trashCount.toLocaleString()
            : null;

    function go(next: ManifestScope) {
        selection.clear();
        void assetLibrary.setScope(next);
    }
</script>

<div class="flex flex-col gap-0.5 {variant === 'rail' ? 'items-center' : 'px-2'}">
    {#each VIEWS as view (view.scope.kind)}
        {@const Icon = view.icon}
        {@const active = scope.kind === view.scope.kind}
        {@const count = badge(view.scope.kind)}
        <button
            type="button"
            title={view.label}
            aria-label={view.label}
            aria-current={active ? "true" : undefined}
            onclick={() => go(view.scope)}
            class="relative flex items-center transition-colors
                   {variant === 'rail'
                ? 'h-9 w-9 justify-center rounded-md'
                : 'w-full gap-2 rounded px-2 py-1 text-left text-sm'}
                   {active
                ? 'bg-neutral-800 text-neutral-100'
                : 'text-neutral-400 hover:bg-neutral-800/60 hover:text-neutral-200'}"
        >
            <Icon class="h-4 w-4 shrink-0" strokeWidth={1.5} />
            {#if variant === "expanded"}
                <span class="truncate">{view.label}</span>
                {#if count}
                    <span class="ml-auto shrink-0 text-[10px] text-neutral-500">{count}</span>
                {/if}
            {:else if count}
                <!-- In the rail there's no room for a number, so it degrades to
                     a dot: "there's something in here" is the part that matters. -->
                <span
                    aria-hidden="true"
                    class="absolute right-1.5 top-1.5 h-1.5 w-1.5 rounded-full bg-neutral-500"
                ></span>
            {/if}
        </button>
    {/each}
</div>
