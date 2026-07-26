<script lang="ts">
    import { Plus } from "@lucide/svelte";
    import { toast } from "svelte-sonner";
    import { assetLibrary, PIN_COLORS, type PinColor } from "$lib/assets.svelte";
    import { describeSteps, emptyStep, isActive, type QuickAction, type Step } from "$lib/actions";
    import StepRow from "./StepRow.svelte";

    /**
     * Create or edit a quick action.
     *
     * The summary line at the bottom is this dialog's equivalent of the smart
     * folder editor's live count: it reads back the pipeline as a sentence, so a
     * step that references nothing is visible before the action is saved rather
     * than at 3 a.m. over 10,000 assets.
     */
    interface Props {
        /** Absent = create. */
        existing?: QuickAction;
        onclose: () => void;
    }

    const { existing, onclose }: Props = $props();

    // Seeded once, on purpose: this dialog edits ONE action for its lifetime and
    // must not retarget under the cursor if the list reloads mid-edit.
    // svelte-ignore state_referenced_locally
    let name = $state(existing?.name ?? "");
    // svelte-ignore state_referenced_locally
    let color = $state<PinColor | null>(existing?.color ?? null);
    // svelte-ignore state_referenced_locally
    let shortcut = $state<number | null>(existing?.shortcut ?? null);
    // svelte-ignore state_referenced_locally
    let steps = $state<Step[]>(
        existing ? ($state.snapshot(existing.steps) as Step[]) : [emptyStep("add_tags")],
    );
    let saving = $state(false);

    const names = {
        tag: (id: string) => assetLibrary.tagNames.get(id),
        folder: (id: string) => assetLibrary.folderNames.get(id),
        get folderNames() {
            return assetLibrary.folderNames;
        },
    };
    const summary = $derived(describeSteps(steps, names));
    const canSave = $derived(name.trim().length > 0 && steps.some(isActive) && !saving);

    async function save() {
        if (!canSave) return;
        saving = true;
        try {
            const draft = {
                name: name.trim(),
                color,
                shortcut,
                // Drop half-built rows rather than saving a step that does
                // nothing: it would read as a pipeline stage on every future
                // edit and quietly widen the summary.
                steps: ($state.snapshot(steps) as Step[]).filter(isActive),
            };
            if (existing) await assetLibrary.updateQuickAction(existing.id, draft);
            else await assetLibrary.createQuickAction(draft);
            onclose();
        } catch (e) {
            toast.error(typeof e === "string" ? e : "Couldn't save the action.");
            saving = false;
        }
    }

    const fieldClass =
        "rounded border border-neutral-800 bg-neutral-900 px-2.5 py-1.5 text-sm text-neutral-100 " +
        "placeholder:text-neutral-600 focus:border-neutral-600 focus:outline-none";
    const legendClass = "text-[11px] font-medium uppercase tracking-wide text-neutral-500";
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
        aria-label={existing ? "Edit action" : "New action"}
        class="relative flex max-h-[80vh] w-[640px] max-w-full flex-col overflow-hidden rounded-xl
               border border-neutral-800 bg-neutral-950 shadow-2xl"
    >
        <div class="shrink-0 border-b border-neutral-800 px-5 py-4">
            <h2 class="text-sm font-semibold text-neutral-100">
                {existing ? "Edit action" : "New action"}
            </h2>
            <p class="mt-1 text-xs text-neutral-500">
                An action applies its steps to whatever is selected, in one go. Every run can be
                undone as a whole.
            </p>
        </div>

        <div class="flex flex-1 flex-col gap-4 overflow-y-auto px-5 py-4 [scrollbar-width:thin]">
            <div class="flex gap-3">
                <label class="flex flex-1 flex-col gap-1.5">
                    <span class={legendClass}>Name</span>
                    <!-- svelte-ignore a11y_autofocus -->
                    <input type="text" bind:value={name} autofocus placeholder="Tag as reference" class={fieldClass} />
                </label>

                <label class="flex w-40 flex-col gap-1.5">
                    <span class={legendClass}>Shortcut</span>
                    <select
                        value={shortcut ?? ""}
                        onchange={(e) =>
                            (shortcut = e.currentTarget.value === "" ? null : +e.currentTarget.value)}
                        class={fieldClass}
                    >
                        <option value="">None</option>
                        {#each [1, 2, 3, 4, 5, 6, 7, 8, 9] as n (n)}
                            <option value={n}>Ctrl+Shift+{n}</option>
                        {/each}
                    </select>
                </label>
            </div>

            <div class="flex flex-col gap-1.5">
                <span class={legendClass}>Accent</span>
                <div class="flex items-center gap-1.5">
                    <button
                        type="button"
                        onclick={() => (color = null)}
                        title="No accent"
                        aria-label="No accent"
                        aria-pressed={color === null}
                        class="h-5 w-5 rounded-full border transition-colors
                               {color === null ? 'border-neutral-300' : 'border-neutral-700'}"
                    ></button>
                    {#each PIN_COLORS as token (token)}
                        <button
                            type="button"
                            onclick={() => (color = token)}
                            title={token}
                            aria-label={token}
                            aria-pressed={color === token}
                            class="h-5 w-5 rounded-full border-2 transition-transform
                                   {color === token
                                ? 'scale-110 border-neutral-100'
                                : 'border-transparent'}"
                            style="background-color: var(--pin-{token})"
                        ></button>
                    {/each}
                </div>
            </div>

            <div class="flex flex-col gap-2">
                <span class={legendClass}>Steps</span>

                {#each steps as step, i (i)}
                    <StepRow
                        {step}
                        onchange={(next) => (steps = steps.map((s, j) => (j === i ? next : s)))}
                        onremove={() => (steps = steps.filter((_, j) => j !== i))}
                    />
                {/each}

                <button
                    type="button"
                    onclick={() => (steps = [...steps, emptyStep("add_tags")])}
                    class="flex items-center gap-1.5 self-start rounded px-2 py-1 text-xs
                           text-neutral-500 transition-colors hover:bg-neutral-800
                           hover:text-neutral-300"
                >
                    <Plus class="h-3 w-3" /> Add step
                </button>
            </div>
        </div>

        <div class="flex shrink-0 items-center gap-3 border-t border-neutral-800 px-5 py-3">
            <!-- The pipeline read back as a sentence. Steps run top to bottom, so
                 this is also the order they apply in. -->
            <span class="min-w-0 flex-1 truncate text-xs text-neutral-500" title={summary}>
                {summary}
            </span>

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
                onclick={save}
                disabled={!canSave}
                class="rounded bg-blue-600 px-3 py-1.5 text-xs font-medium text-white
                       transition-colors hover:bg-blue-500 disabled:opacity-40"
            >
                {existing ? "Save" : "Create"}
            </button>
        </div>
    </div>
</div>
