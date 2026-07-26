<script lang="ts">
    import { Plus, FolderPlus } from "@lucide/svelte";
    import { MAX_DEPTH, type Condition, type GroupOp, type RuleNode } from "$lib/rules";
    import RuleRow from "./RuleRow.svelte";
    import Self from "./RuleGroup.svelte";

    /**
     * A group of conditions, plus (up to `MAX_DEPTH`) nested groups.
     *
     * `all` / `any` / `none` as one triad, rather than `all`/`any` beside a
     * separate "are true/false" toggle: the same expressiveness for every
     * realistic rule with one fewer control to explain.
     */
    interface Props {
        node: Extract<RuleNode, { kind: "group" }>;
        depth: number;
        onchange: (next: RuleNode) => void;
        /** Absent on the root — a rule set always has an outermost group. */
        onremove?: () => void;
    }

    const { node, depth, onchange, onremove }: Props = $props();

    const OPS: { value: GroupOp; label: string }[] = [
        { value: "all", label: "all" },
        { value: "any", label: "any" },
        { value: "none", label: "none" },
    ];

    const canNest = $derived(depth + 1 < MAX_DEPTH);

    const replaceChild = (i: number, child: RuleNode) =>
        onchange({ ...node, children: node.children.map((c, j) => (j === i ? child : c)) });

    const removeChild = (i: number) =>
        onchange({ ...node, children: node.children.filter((_, j) => j !== i) });

    const addCondition = () =>
        onchange({
            ...node,
            children: [
                ...node.children,
                {
                    kind: "condition",
                    type: "text",
                    field: "name",
                    op: "contains",
                    value: "",
                } as RuleNode,
            ],
        });

    const addGroup = () =>
        onchange({
            ...node,
            children: [...node.children, { kind: "group", op: "all", children: [] }],
        });
</script>

<div
    class="flex flex-col gap-2 rounded-lg border border-neutral-800 p-2
           {depth > 0 ? 'bg-neutral-950/60' : ''}"
>
    <div class="flex items-center gap-2">
        <span class="text-xs text-neutral-400">Match</span>
        <select
            value={node.op}
            onchange={(e) => onchange({ ...node, op: e.currentTarget.value as GroupOp })}
            aria-label="Match mode"
            class="rounded border border-neutral-700 bg-neutral-950 px-1.5 py-1 text-xs
                   text-neutral-200 focus:border-neutral-500 focus:outline-none"
        >
            {#each OPS as o (o.value)}
                <option value={o.value}>{o.label}</option>
            {/each}
        </select>
        <span class="text-xs text-neutral-400">of the following</span>

        <div class="flex-1"></div>

        <button
            type="button"
            onclick={addCondition}
            title="Add condition"
            aria-label="Add condition"
            class="grid h-6 w-6 place-items-center rounded text-neutral-500 transition-colors
                   hover:bg-neutral-800 hover:text-neutral-200"
        >
            <Plus class="h-3.5 w-3.5" />
        </button>
        {#if canNest}
            <button
                type="button"
                onclick={addGroup}
                title="Add nested group"
                aria-label="Add nested group"
                class="grid h-6 w-6 place-items-center rounded text-neutral-500 transition-colors
                       hover:bg-neutral-800 hover:text-neutral-200"
            >
                <FolderPlus class="h-3.5 w-3.5" />
            </button>
        {/if}
        {#if onremove}
            <button
                type="button"
                onclick={onremove}
                title="Remove group"
                aria-label="Remove group"
                class="rounded px-1.5 text-xs text-neutral-500 transition-colors
                       hover:bg-neutral-800 hover:text-neutral-200"
            >
                ✕
            </button>
        {/if}
    </div>

    {#if node.children.length === 0}
        <!-- An empty group matches everything rather than nothing (see the
             compiler), so this says what it does instead of looking broken. -->
        <p class="px-1 text-xs text-neutral-600">No conditions — matches everything.</p>
    {/if}

    {#each node.children as child, i (i)}
        {#if child.kind === "group"}
            <Self
                node={child}
                depth={depth + 1}
                onchange={(next) => replaceChild(i, next)}
                onremove={() => removeChild(i)}
            />
        {:else}
            {@const { kind: _kind, ...condition } = child}
            <RuleRow
                condition={condition as Condition}
                onchange={(next) => replaceChild(i, { kind: "condition", ...next } as RuleNode)}
                onremove={() => removeChild(i)}
            />
        {/if}
    {/each}
</div>
