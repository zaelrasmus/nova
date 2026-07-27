<script lang="ts">
    import { Minus, Filter } from "@lucide/svelte";
    import { assetLibrary, type Folder } from "$lib/assets.svelte";
    import { selection } from "$lib/selection.svelte";
    import { emptyRules, type RuleNode } from "$lib/rules";
    import RuleGroup from "../rules/RuleGroup.svelte";
    import RenameEditor from "./RenameEditor.svelte";
    import {
        emptyOp,
        STEP_GROUPS,
        stepLabel,
        type Op,
        type Step,
        type OpType,
        type TextMode,
    } from "$lib/actions";

    /**
     * One step: operation · operands · optional condition.
     *
     * The operand editor follows from the operation's TYPE, which is what the
     * union already encodes — each variant carries exactly the fields that make
     * sense for it, so this component never has to guard against a folder id on
     * a note.
     */
    interface Props {
        step: Step;
        onchange: (next: Step) => void;
        onremove: () => void;
    }

    const { step, onchange, onremove }: Props = $props();

    /** Replace the operation, keeping any condition the step carries. */
    const setOp = (op: Op) => onchange({ ...step, op });

    const TEXT_MODES: { value: TextMode; label: string }[] = [
        { value: "replace", label: "replace with" },
        { value: "append", label: "append" },
        { value: "prepend", label: "prepend" },
    ];

    /**
     * The folder tree flattened in display order, with depth for indentation.
     *
     * Same reasoning as the rule editor's folder picker: two folders called
     * "2024" under different parents are indistinguishable as a flat list, and
     * picking the wrong one produces an action that looks right and files assets
     * in the wrong place.
     */
    const folderRows = $derived.by(() => {
        const byParent = new Map<string | null, Folder[]>();
        for (const f of assetLibrary.folders) {
            const arr = byParent.get(f.parent_id) ?? [];
            arr.push(f);
            byParent.set(f.parent_id, arr);
        }
        const out: { folder: Folder; depth: number }[] = [];
        const walk = (parent: string | null, depth: number) => {
            for (const f of byParent.get(parent) ?? []) {
                out.push({ folder: f, depth });
                walk(f.id, depth + 1);
            }
        };
        walk(null, 0);
        return out;
    });

    /**
     * Switching type builds a fresh step rather than carrying operands across.
     *
     * Except within the tag pair and the folder pair, where the operand means the
     * same thing on both sides — add-then-remove of the same tags is a normal
     * edit, and re-picking the folder would be busywork.
     */
    function setType(next: OpType) {
        const tagPair = ["add_tags", "remove_tags"];
        const folderPair = ["add_to_folder", "remove_from_folder"];
        const current = step.op.type;
        if (
            (tagPair.includes(current) && tagPair.includes(next)) ||
            (folderPair.includes(current) && folderPair.includes(next))
        ) {
            setOp({ ...step.op, type: next } as Op);
        } else {
            // The condition survives a type change: "if wider than 3000" is a
            // statement about the assets, not about what you do to them.
            setOp(emptyOp(next));
        }
    }

    function toggleTag(tagId: string) {
        if (step.op.type !== "add_tags" && step.op.type !== "remove_tags") return;
        const tag_ids = step.op.tag_ids.includes(tagId)
            ? step.op.tag_ids.filter((x) => x !== tagId)
            : [...step.op.tag_ids, tagId];
        setOp({ ...step.op, tag_ids });
    }

    function toggleFolder(folderId: string) {
        if (step.op.type !== "set_folders") return;
        const folder_ids = step.op.folder_ids.includes(folderId)
            ? step.op.folder_ids.filter((x) => x !== folderId)
            : [...step.op.folder_ids, folderId];
        setOp({ ...step.op, folder_ids });
    }

    /**
     * Add or drop the step's condition.
     *
     * `emptyRules()` rather than a null tree, because an empty `all` group
     * constrains nothing and compiles to TRUE — so a freshly-added condition
     * leaves the step applying to everything until the first rule is written,
     * rather than silently gating it to nothing.
     */
    const toggleCondition = () =>
        onchange({ ...step, when: step.when ? null : emptyRules() });

    const controlClass =
        "rounded border border-neutral-700 bg-neutral-950 px-1.5 py-1 text-xs text-neutral-200 " +
        "focus:border-neutral-500 focus:outline-none";
</script>

<div class="flex flex-col gap-2 rounded border border-neutral-800 p-2">
  <div class="flex items-start gap-1.5">
    <select
        value={step.op.type}
        onchange={(e) => setType(e.currentTarget.value as OpType)}
        aria-label="Step"
        class="{controlClass} w-40 shrink-0"
    >
        {#each STEP_GROUPS as group (group.label)}
            <optgroup label={group.label}>
                {#each group.types as t (t)}
                    <option value={t}>{stepLabel(t)}</option>
                {/each}
            </optgroup>
        {/each}
    </select>

    {#if step.op.type === "add_tags" || step.op.type === "remove_tags"}
        <!-- Plain multi-select, not the rule editor's tri-state: the step type
             already says add or remove, so a chip only has to answer "is this
             tag involved". -->
        <div class="flex max-h-20 min-w-0 flex-1 flex-wrap gap-1 overflow-y-auto [scrollbar-width:thin]">
            {#if assetLibrary.tags.length === 0}
                <span class="text-xs text-neutral-600">No tags in this library yet.</span>
            {/if}
            {#each assetLibrary.tags as tag (tag.id)}
                {@const on = step.op.tag_ids.includes(tag.id)}
                <button
                    type="button"
                    aria-pressed={on}
                    onclick={() => toggleTag(tag.id)}
                    class="rounded px-1.5 py-0.5 text-xs transition-colors
                           {on
                        ? 'bg-blue-600 text-white'
                        : 'bg-neutral-800 text-neutral-400 hover:bg-neutral-700'}"
                >
                    {tag.name}
                </button>
            {/each}
        </div>
    {:else if step.op.type === "clear_all_tags"}
        <!-- Resolved at run time, never expanded into a tag list here: that's
             what keeps it correct for tags created after this was written. -->
        <span class="flex-1 py-1 text-xs text-neutral-500">
            every tag on the selected assets
        </span>
    {:else if step.op.type === "add_to_folder" || step.op.type === "remove_from_folder"}
        <select
            value={step.op.folder_id}
            onchange={(e) => setOp({ ...step.op, folder_id: e.currentTarget.value } as Op)}
            aria-label="Folder"
            class="{controlClass} min-w-0 flex-1"
        >
            <option value="">Pick a folder…</option>
            {#each folderRows as { folder, depth } (folder.id)}
                <!-- Non-breaking spaces: a <select> collapses ordinary runs of
                     whitespace, so this is the only way nesting survives here. -->
                <option value={folder.id}>
                    {"  ".repeat(depth)}{folder.name}
                </option>
            {/each}
        </select>
    {:else if step.op.type === "set_folders"}
        <div class="flex min-w-0 flex-1 flex-col gap-1">
            <div
                class="max-h-24 overflow-y-auto rounded border border-neutral-800
                       [scrollbar-width:thin]"
            >
                {#if folderRows.length === 0}
                    <p class="px-2 py-1 text-xs text-neutral-600">No folders in this library yet.</p>
                {/if}
                {#each folderRows as { folder, depth } (folder.id)}
                    {@const on = step.op.folder_ids.includes(folder.id)}
                    <button
                        type="button"
                        aria-pressed={on}
                        onclick={() => toggleFolder(folder.id)}
                        class="flex w-full items-center gap-2 py-0.5 pr-2 text-left text-xs
                               text-neutral-300 transition-colors hover:bg-neutral-800"
                        style="padding-left: {8 + depth * 12}px"
                    >
                        <span
                            aria-hidden="true"
                            class="grid h-3.5 w-3.5 shrink-0 place-items-center rounded-sm border
                                   text-[9px] leading-none text-white
                                   {on ? 'border-blue-500 bg-blue-600' : 'border-neutral-600'}"
                        >
                            {on ? "✓" : ""}
                        </span>
                        <span class="truncate">{folder.name}</span>
                    </button>
                {/each}
            </div>
            <!-- Says what the destructive part does, where it's decided. This
                 step REPLACES membership, and picking nothing is a real choice
                 rather than an unfinished row. -->
            <span class="text-[10px] leading-tight text-neutral-600">
                {step.op.folder_ids.length === 0
                    ? "Nothing picked — assets end up in no folder at all."
                    : "Assets end up in exactly these folders, leaving any others."}
            </span>
        </div>
    {:else if step.op.type === "set_note"}
        <div class="flex min-w-0 flex-1 flex-col gap-1">
            <select
                value={step.op.mode}
                onchange={(e) => setOp({ ...step.op, mode: e.currentTarget.value as TextMode } as Op)}
                aria-label="Note mode"
                class="{controlClass} self-start"
            >
                {#each TEXT_MODES as m (m.value)}
                    <option value={m.value}>{m.label}</option>
                {/each}
            </select>
            <textarea
                value={step.op.text}
                oninput={(e) => setOp({ ...step.op, text: e.currentTarget.value } as Op)}
                rows="2"
                placeholder={step.op.mode === "replace" ? "Leave empty to clear the note" : "Text to add"}
                aria-label="Note text"
                class="{controlClass} w-full resize-y placeholder:text-neutral-600"
            ></textarea>
            {#if step.op.mode === "replace"}
                <span class="text-[10px] leading-tight text-amber-600/80">
                    Replaces any note the assets already have.
                </span>
            {/if}
        </div>
    {:else if step.op.type === "set_source_url"}
        <input
            type="text"
            value={step.op.url}
            oninput={(e) => setOp({ ...step.op, url: e.currentTarget.value } as Op)}
            placeholder="https://…  (empty clears it)"
            aria-label="Source URL"
            class="{controlClass} min-w-0 flex-1 placeholder:text-neutral-600"
        />
    {:else if step.op.type === "rename_with_pattern"}
        <RenameEditor
            op={step.op}
            onchange={setOp}
            assetIds={selection.assetIds}
        />
    {/if}

    <div class="flex shrink-0 flex-col items-center gap-1">
        <button
            type="button"
            onclick={onremove}
            title="Remove step"
            aria-label="Remove step"
            class="grid h-6 w-6 place-items-center rounded text-neutral-500
                   transition-colors hover:bg-neutral-800 hover:text-neutral-200"
        >
            <Minus class="h-3.5 w-3.5" />
        </button>
        <button
            type="button"
            onclick={toggleCondition}
            title={step.when ? "Always apply this step" : "Only apply to matching assets"}
            aria-label="Condition"
            aria-pressed={!!step.when}
            class="grid h-6 w-6 place-items-center rounded transition-colors
                   {step.when
                ? 'bg-neutral-800 text-blue-400'
                : 'text-neutral-600 hover:bg-neutral-800 hover:text-neutral-300'}"
        >
            <Filter class="h-3.5 w-3.5" />
        </button>
    </div>
  </div>

  <!-- The condition, when there is one.
       Indented under the step and introduced by "only if", because that's the
       relationship: the step is what happens, this is what it happens to. Runs
       the SAME rule editor as smart folders — one language, one editor, so a
       rule learned in one place reads the same in the other. -->
  {#if step.when}
    <div class="ml-2 flex flex-col gap-1 border-l border-neutral-800 pl-2">
        <span class="text-[10px] font-medium uppercase tracking-wide text-neutral-500">
            only if
        </span>
        <RuleGroup
            node={step.when as Extract<RuleNode, { kind: "group" }>}
            depth={0}
            onchange={(next) => onchange({ ...step, when: next })}
        />
    </div>
  {/if}
</div>
