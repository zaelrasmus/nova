<script lang="ts">
    import { Sparkles } from "@lucide/svelte";
    import { toast } from "svelte-sonner";
    import { assetLibrary } from "$lib/assets.svelte";
    import { undoRun } from "./actions/run";

    /**
     * Tags this folder seeds onto assets that arrive in it.
     *
     * Three behaviours the UI has to state, because none of them is guessable
     * from a list of chips:
     *   * it applies on ARRIVAL, so nothing already here changes;
     *   * an asset that leaves KEEPS the tags — by then they're the user's data;
     *   * subfolders don't inherit.
     */
    interface Props {
        folderId: string;
        legendClass: string;
    }

    const { folderId, legendClass }: Props = $props();

    let selected = $state<string[]>([]);
    let loading = $state(true);
    let applying = $state(false);

    // Reloaded when the selected folder changes; a stale answer would show one
    // folder's seeds against another's name.
    $effect(() => {
        const id = folderId;
        let cancelled = false;
        loading = true;
        assetLibrary
            .fetchFolderAutoTags(id)
            .then((tags) => {
                if (cancelled) return;
                selected = tags;
                loading = false;
            })
            .catch(() => {
                if (cancelled) return;
                selected = [];
                loading = false;
            });
        return () => {
            cancelled = true;
        };
    });

    /** Optimistic: the chip is the state, and a failed write puts it back. */
    async function toggle(tagId: string) {
        const before = selected;
        const next = selected.includes(tagId)
            ? selected.filter((x) => x !== tagId)
            : [...selected, tagId];
        selected = next;
        try {
            await assetLibrary.setFolderAutoTags(folderId, next);
        } catch (e) {
            selected = before;
            toast.error(typeof e === "string" ? e : "Couldn't save the auto-tags.");
        }
    }

    async function applyToExisting() {
        applying = true;
        try {
            const summary = await assetLibrary.applyFolderAutoTags(folderId);
            const runId = summary.run_id;
            if (summary.asset_count === 0) {
                toast.success("Nothing to tag here yet.");
            } else if (runId && summary.is_undoable) {
                toast.success(`Tagged ${summary.asset_count.toLocaleString()} assets`, {
                    action: { label: "Undo", onClick: () => void undoRun(runId) },
                });
            } else {
                toast.success(`Tagged ${summary.asset_count.toLocaleString()} assets`);
            }
        } catch (e) {
            toast.error(typeof e === "string" ? e : "Couldn't apply the auto-tags.");
        } finally {
            applying = false;
        }
    }
</script>

<div>
    <span class={legendClass}>Auto-tags</span>

    {#if assetLibrary.tags.length === 0}
        <p class="text-xs text-neutral-600">No tags in this library yet.</p>
    {:else if loading}
        <p class="text-xs text-neutral-600">Reading…</p>
    {:else}
        <div class="flex flex-wrap gap-1">
            {#each assetLibrary.tags as tag (tag.id)}
                {@const on = selected.includes(tag.id)}
                <button
                    type="button"
                    aria-pressed={on}
                    onclick={() => toggle(tag.id)}
                    class="rounded px-1.5 py-0.5 text-xs transition-colors
                           {on
                        ? 'bg-blue-600 text-white'
                        : 'bg-neutral-800 text-neutral-400 hover:bg-neutral-700'}"
                >
                    {tag.name}
                </button>
            {/each}
        </div>

        <p class="mt-1.5 text-[10px] leading-tight text-neutral-600">
            {#if selected.length === 0}
                Assets dropped here get no tags automatically.
            {:else}
                Applied when an asset arrives. Assets already here are unchanged, and moving one
                out keeps its tags. Subfolders don't inherit.
            {/if}
        </p>

        {#if selected.length > 0}
            <button
                type="button"
                onclick={applyToExisting}
                disabled={applying}
                class="mt-1.5 flex items-center gap-1.5 rounded border border-neutral-800 px-2 py-1
                       text-xs text-neutral-400 transition-colors hover:bg-neutral-800
                       hover:text-neutral-200 disabled:opacity-50"
            >
                <Sparkles class="h-3 w-3" />
                {applying ? "Applying…" : "Apply to assets already here"}
            </button>
        {/if}
    {/if}
</div>
