<script lang="ts">
    import { toast } from "svelte-sonner";
    import { assetLibrary, type SmartFolder } from "$lib/assets.svelte";
    import { emptyRules, type RuleNode } from "$lib/rules";
    import RuleGroup from "./RuleGroup.svelte";

    /**
     * Create or edit a smart folder.
     *
     * The live count is the only validation a rule set gets, and it's the reason
     * this dialog is worth having over a text field: it tells you the rule is
     * wrong — 0 items, or all 40,000 — before you commit to it. It runs the real
     * predicate through the real compiler, so it's debounced and superseded
     * rather than fired per keystroke.
     */
    interface Props {
        /** Absent = create. */
        existing?: SmartFolder;
        onclose: () => void;
    }

    const { existing, onclose }: Props = $props();

    // Seeded once, on purpose: this dialog edits ONE folder for its lifetime, so
    // it must not retarget under the user's cursor if the list reloads mid-edit.
    // The caller remounts it to edit something else.
    // svelte-ignore state_referenced_locally
    let name = $state(existing?.name ?? "");
    // svelte-ignore state_referenced_locally
    let rules = $state<RuleNode>(
        existing ? ($state.snapshot(existing.rules) as RuleNode) : emptyRules(),
    );
    let saving = $state(false);

    let count = $state<number | null>(null);
    let counting = $state(false);

    // Debounced, and guarded by a token: a slow count from three edits ago must
    // not overwrite the answer for what's on screen now.
    let countToken = 0;
    $effect(() => {
        const snapshot = $state.snapshot(rules) as RuleNode;
        const token = ++countToken;
        counting = true;
        const timer = setTimeout(async () => {
            try {
                const n = await assetLibrary.countMatching(snapshot);
                if (token === countToken) {
                    count = n;
                    counting = false;
                }
            } catch {
                if (token === countToken) {
                    count = null;
                    counting = false;
                }
            }
        }, 300);
        return () => clearTimeout(timer);
    });

    const canSave = $derived(name.trim().length > 0 && !saving);

    async function save() {
        if (!canSave) return;
        saving = true;
        try {
            const snapshot = $state.snapshot(rules) as RuleNode;
            if (existing) {
                await assetLibrary.updateSmartFolder(existing.id, {
                    name: name.trim(),
                    rules: snapshot,
                });
            } else {
                const created = await assetLibrary.createSmartFolder(name.trim(), snapshot);
                // Go there. Creating a place and staying somewhere else reads as
                // "did that work?" — the same reason an import reveals its result.
                await assetLibrary.setScope({ kind: "smart", id: created.id });
            }
            onclose();
        } catch (e) {
            toast.error(typeof e === "string" ? e : "Couldn't save the smart folder.");
            saving = false;
        }
    }
</script>

<svelte:window
    onkeydown={(e) => {
        if (e.key === "Escape") onclose();
    }}
/>

<div class="fixed inset-0 z-[95] grid place-items-center bg-black/60 p-6">
    <!-- Backdrop click closes; the panel stops the bubble so a click inside
         doesn't dismiss a half-written rule. -->
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
        aria-label={existing ? "Edit smart folder" : "New smart folder"}
        class="relative flex max-h-[80vh] w-[640px] max-w-full flex-col overflow-hidden rounded-xl
               border border-neutral-800 bg-neutral-950 shadow-2xl"
    >
        <div class="shrink-0 border-b border-neutral-800 px-5 py-4">
            <h2 class="text-sm font-semibold text-neutral-100">
                {existing ? "Edit smart folder" : "New smart folder"}
            </h2>
            <p class="mt-1 text-xs text-neutral-500">
                A smart folder collects every asset that matches its rules, and keeps collecting as
                your library changes.
            </p>
        </div>

        <div class="flex flex-1 flex-col gap-4 overflow-y-auto px-5 py-4 [scrollbar-width:thin]">
            <label class="flex flex-col gap-1.5">
                <span class="text-[11px] font-medium uppercase tracking-wide text-neutral-500">
                    Name
                </span>
                <!-- svelte-ignore a11y_autofocus -->
                <input
                    type="text"
                    bind:value={name}
                    autofocus
                    placeholder="Approved client renders"
                    class="rounded border border-neutral-800 bg-neutral-900 px-2.5 py-1.5 text-sm
                           text-neutral-100 placeholder:text-neutral-600 focus:border-neutral-600
                           focus:outline-none"
                />
            </label>

            <div class="flex flex-col gap-1.5">
                <span class="text-[11px] font-medium uppercase tracking-wide text-neutral-500">
                    Rules
                </span>
                <RuleGroup
                    node={rules as Extract<RuleNode, { kind: "group" }>}
                    depth={0}
                    onchange={(next) => (rules = next)}
                />
            </div>
        </div>

        <div class="flex shrink-0 items-center gap-3 border-t border-neutral-800 px-5 py-3">
            <span class="text-xs text-neutral-500">
                {#if counting}
                    Counting…
                {:else if count === null}
                    Couldn't count
                {:else}
                    Found {count.toLocaleString()}
                    {count === 1 ? "item" : "items"}
                {/if}
            </span>

            <div class="flex-1"></div>

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
