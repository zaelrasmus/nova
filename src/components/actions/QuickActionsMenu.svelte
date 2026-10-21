<script lang="ts">
    import { Zap, Plus, Pencil, Trash2, Undo2 } from "@lucide/svelte";
    import { toast } from "svelte-sonner";
    import { assetLibrary } from "$lib/assets.svelte";
    import { selection } from "$lib/selection.svelte";
    import { describeSteps, type QuickAction } from "$lib/actions";
    import { runAction, undoRun } from "./run";
    import QuickActionDialog from "./QuickActionDialog.svelte";
    import Popover from "../controls/Popover.svelte";

    /**
     * The ⚡ menu in the grid toolbar.
     *
     * Lives here rather than in the sidebar because an action is a VERB on the
     * current selection, and the toolbar is where the other selection-scoped
     * controls already are (search, filter, sort). The sidebar is navigation —
     * putting a verb in a list of places would blur the one distinction the app
     * has been careful about.
     *
     * This dropdown doubles as the manager. A separate settings screen for three
     * actions would be more chrome than the feature deserves, and editing where
     * you run is how the smart folder list already works.
     */
    let open = $state(false);
    let editing = $state<QuickAction | null | undefined>(undefined);

    const actions = $derived(assetLibrary.quickActions);
    const count = $derived(selection.assetCount);
    const names = {
        tag: (id: string) => assetLibrary.tagNames.get(id),
        folder: (id: string) => assetLibrary.folderNames.get(id),
        get folderNames() {
            return assetLibrary.folderNames;
        },
    };

    /**
     * The most recent run still worth offering an undo for.
     *
     * The toast that announced it is long gone by the time the user changes
     * their mind, so the offer has to live somewhere durable. `is_undoable` is
     * false for runs too large to record an inverse for — those are history
     * entries, not offers, so they aren't shown here at all.
     */
    const undoable = $derived(assetLibrary.actionRuns.find((r) => r.is_undoable));

    async function run(action: QuickAction) {
        open = false;
        await runAction(action);
    }

    async function remove(action: QuickAction) {
        const ok = window.confirm(
            `Delete "${action.name}"? Runs you've already made stay undoable.`,
        );
        if (!ok) return;
        try {
            await assetLibrary.deleteQuickAction(action.id);
        } catch (e) {
            toast.error(typeof e === "string" ? e : "Couldn't delete the action.");
        }
    }
</script>

<!-- A fixed-position Popover, not an absolute panel: the toolbar lives in an
     overflow-hidden header, which clipped the old panel to the header strip. -->
<Popover bind:open label="Quick actions" width="w-72">
    {#snippet trigger({ open: isOpen, toggle })}
        <button
            type="button"
            onclick={toggle}
            title="Quick actions"
            aria-label="Quick actions"
            aria-expanded={isOpen}
            class="grid h-7 w-7 shrink-0 place-items-center rounded-md transition-colors
                   {isOpen
                ? 'bg-wash-strong text-neutral-100'
                : 'text-neutral-500 hover:bg-wash hover:text-neutral-200'}"
        >
            <Zap class="h-4 w-4" />
        </button>
    {/snippet}
    {#snippet children()}
        <div role="menu" tabindex="-1">
            <p class="nv-menu-label">
                {count === 0
                    ? "Nothing selected"
                    : `${count.toLocaleString()} selected`}
            </p>

            {#each actions as action (action.id)}
                <div class="group flex items-center rounded-md hover:bg-wash-strong">
                    <button
                        type="button"
                        role="menuitem"
                        onclick={() => run(action)}
                        disabled={count === 0}
                        title={count === 0
                            ? "Select some assets first"
                            : describeSteps(action.steps, names)}
                        class="flex min-h-[30px] min-w-0 flex-1 items-center gap-2.5 rounded-md px-2 text-left
                               text-neutral-300 disabled:opacity-40"
                    >
                        <span
                            aria-hidden="true"
                            class="h-1.5 w-1.5 shrink-0 rounded-full"
                            style="background-color: var(--pin-{action.color ?? 'none'})"
                        ></span>
                        <span class="truncate">{action.name}</span>
                        {#if action.shortcut !== null}
                            <span class="ml-auto shrink-0 text-[10px] text-neutral-600">
                                Ctrl+Shift+{action.shortcut}
                            </span>
                        {/if}
                    </button>
                    <div
                        class="flex shrink-0 items-center opacity-0 transition-opacity
                               group-hover:opacity-100"
                    >
                        <button
                            type="button"
                            title="Edit"
                            aria-label="Edit"
                            onclick={() => {
                                editing = action;
                                open = false;
                            }}
                            class="grid h-6 w-6 place-items-center rounded text-neutral-500 hover:text-neutral-200"
                        >
                            <Pencil class="h-3 w-3" />
                        </button>
                        <button
                            type="button"
                            title="Delete"
                            aria-label="Delete"
                            onclick={() => remove(action)}
                            class="grid h-6 w-6 place-items-center rounded text-neutral-500 hover:text-red-400"
                        >
                            <Trash2 class="h-3 w-3" />
                        </button>
                    </div>
                </div>
            {:else}
                <p class="px-2 py-1.5 text-xs text-neutral-600">
                    No actions yet. An action applies several edits to a selection at once.
                </p>
            {/each}

            <div class="nv-menu-sep"></div>

            {#if undoable}
                <button
                    type="button"
                    role="menuitem"
                    onclick={() => {
                        open = false;
                        void undoRun(undoable.id);
                    }}
                    class="nv-menu-item"
                >
                    <Undo2 class="h-3.5 w-3.5 shrink-0" />
                    <span class="truncate">Undo "{undoable.name}"</span>
                </button>
            {/if}

            <button
                type="button"
                role="menuitem"
                onclick={() => {
                    editing = null;
                    open = false;
                }}
                class="nv-menu-item"
            >
                <Plus class="h-3.5 w-3.5 shrink-0" /> New action…
            </button>
        </div>
    {/snippet}
</Popover>

{#if editing !== undefined}
    <QuickActionDialog existing={editing ?? undefined} onclose={() => (editing = undefined)} />
{/if}
