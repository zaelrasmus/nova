<script lang="ts">
    import { toast } from "svelte-sonner";
    import { assetLibrary } from "$lib/assets.svelte";
    import { emptyOp, type Op } from "$lib/actions";
    import { undoRun } from "./run";
    import RenameEditor from "./RenameEditor.svelte";

    /**
     * Rename a selection from a pattern, without building an action first.
     *
     * This is the one bulk operation with no alternative — you cannot rename 500
     * files by pattern with multi-select, at any amount of clicking. Making it
     * reachable straight from the grid is the difference between a feature
     * people use and one they'd have to assemble a macro to reach.
     *
     * It still runs through the action pipeline, so it commits in one
     * transaction and records the same inverse an action would.
     */
    interface Props {
        /** Snapshotted by the caller at the moment the menu opened. */
        assetIds: string[];
        onclose: () => void;
    }

    const { assetIds, onclose }: Props = $props();

    let op = $state<Op>(emptyOp("rename_with_pattern"));
    let patternError = $state<string | null>(null);
    let running = $state(false);

    const canApply = $derived(
        !running &&
            patternError === null &&
            op.type === "rename_with_pattern" &&
            op.pattern.trim().length > 0,
    );

    async function apply() {
        if (!canApply) return;
        running = true;
        try {
            const summary = await assetLibrary.runSteps(
                "Rename",
                [{ op: $state.snapshot(op) as Op }],
                assetIds,
            );
            const runId = summary.run_id;
            const what = `Renamed ${summary.asset_count.toLocaleString()} ${
                summary.asset_count === 1 ? "asset" : "assets"
            }`;
            if (runId && summary.is_undoable) {
                toast.success(what, {
                    action: { label: "Undo", onClick: () => void undoRun(runId) },
                });
            } else {
                toast.success(what);
            }
            onclose();
        } catch (e) {
            toast.error(typeof e === "string" ? e : "Couldn't rename.");
            running = false;
        }
    }
</script>

<svelte:window
    onkeydown={(e) => {
        if (e.key === "Escape") onclose();
    }}
/>

<div class="fixed inset-0 z-[95] grid place-items-center bg-black/60 p-6">
    <button
        type="button"
        tabindex="-1"
        aria-label="Cancel"
        class="absolute inset-0 cursor-default"
        onclick={onclose}
    ></button>

    <div
        role="dialog"
        aria-modal="true"
        aria-label="Rename assets"
        class="relative flex w-[560px] max-w-full flex-col gap-4 rounded-xl border border-neutral-800
               bg-neutral-950 p-5 shadow-2xl"
    >
        <div>
            <h2 class="text-sm font-semibold text-neutral-100">
                Rename {assetIds.length.toLocaleString()}
                {assetIds.length === 1 ? "asset" : "assets"}
            </h2>
            <p class="mt-1 text-xs text-neutral-500">
                Only the name shown in Nova changes — the file on disk and its extension are
                untouched.
            </p>
        </div>

        {#if op.type === "rename_with_pattern"}
            <RenameEditor
                {op}
                onchange={(next) => (op = next)}
                {assetIds}
                onvalidity={(error) => (patternError = error)}
            />
        {/if}

        <div class="flex items-center justify-end gap-3">
            <button
                type="button"
                onclick={onclose}
                class="rounded border border-neutral-800 px-3 py-1.5 text-xs text-neutral-300
                       transition-colors hover:bg-neutral-800"
            >
                Cancel
            </button>
            <button
                type="button"
                onclick={apply}
                disabled={!canApply}
                class="rounded bg-blue-600 px-3 py-1.5 text-xs font-medium text-white
                       transition-colors hover:bg-blue-500 disabled:opacity-40"
            >
                {running ? "Renaming…" : "Rename"}
            </button>
        </div>
    </div>
</div>
