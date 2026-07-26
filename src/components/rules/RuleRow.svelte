<script lang="ts">
    import { Minus } from "@lucide/svelte";
    import {
        ASSET_TYPE_LABELS,
        assetLibrary,
        type AssetTypeFilter,
        type Folder,
    } from "$lib/assets.svelte";
    import type { Condition, NumField, TextField } from "$lib/rules";

    /**
     * One condition row: field · operator · value.
     *
     * The operator list and the value editor both follow from the FIELD, which
     * is what the tree's shape already encodes — each condition variant carries
     * exactly the operators and operands that make sense for it, so this
     * component never has to guard against `contains` on a number.
     *
     * Two fields the engine supports aren't offered here: colour and shape.
     * Both need a real picker (an eyedropper, a ratio grid), and a cramped copy
     * inside a rule row would be a worse version of the control the filter bar
     * already has. They stay compilable — a saved filter can carry them and
     * `fromRuleTree` round-trips them — and come here when they get a picker
     * worth using.
     */
    interface Props {
        condition: Condition;
        onchange: (next: Condition) => void;
        onremove: () => void;
    }

    const { condition, onchange, onremove }: Props = $props();

    type FieldKey =
        | `text:${TextField}`
        | `number:${NumField}`
        | `date:${"imported_date" | "creation_date" | "modified_date"}`
        | "media_type"
        | "extension"
        | "tags"
        | "folder"
        | "uncategorized";

    const FIELDS: { value: FieldKey; label: string }[] = [
        { value: "text:name", label: "Name" },
        { value: "text:notes", label: "Notes" },
        { value: "text:source_url", label: "Source URL" },
        // Two different questions, deliberately both offered: membership picks
        // folders by identity (survives a rename, can walk subfolders), while
        // "folder name" matches TEXT, which is what you want for a convention
        // like every folder called "raw" wherever it lives.
        { value: "folder", label: "In folder" },
        { value: "text:folder_name", label: "Folder name" },
        { value: "number:file_size", label: "File size" },
        { value: "number:width", label: "Width" },
        { value: "number:height", label: "Height" },
        { value: "date:imported_date", label: "Date imported" },
        { value: "date:creation_date", label: "Date created" },
        { value: "date:modified_date", label: "Date modified" },
        { value: "media_type", label: "Media type" },
        { value: "extension", label: "Extension" },
        { value: "tags", label: "Tags" },
        { value: "uncategorized", label: "Uncategorized" },
    ];

    const TAG_MODES = [
        { value: "all", label: "has all of" },
        { value: "any", label: "has any of" },
        { value: "equals", label: "has exactly" },
    ] as const;

    const TEXT_OPS = [
        { value: "contains", label: "contains" },
        { value: "excludes", label: "doesn't contain" },
        { value: "begins_with", label: "begins with" },
        { value: "ends_with", label: "ends with" },
        { value: "equals", label: "is" },
        { value: "is_null", label: "is empty" },
        { value: "is_not_null", label: "is set" },
    ] as const;

    const NUM_OPS = [
        { value: "greater_than_or_equal", label: "at least" },
        { value: "less_than_or_equal", label: "at most" },
        { value: "equals", label: "is" },
        { value: "between", label: "between" },
    ] as const;

    const DATE_OPS = [
        { value: "within_last", label: "within last (days)" },
        { value: "after", label: "after" },
        { value: "before", label: "before" },
        { value: "on", label: "on" },
    ] as const;

    /**
     * The folder tree flattened in display order, with depth for indentation.
     *
     * Hierarchy has to survive into this picker: two folders called "2024" under
     * different parents are indistinguishable as a flat list of names, and
     * picking the wrong one produces a smart folder that looks right and
     * collects the wrong assets.
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

    const fieldKey = $derived.by((): FieldKey => {
        switch (condition.type) {
            case "text":
            case "number":
            case "date":
                return `${condition.type}:${condition.field}` as FieldKey;
            default:
                return condition.type as FieldKey;
        }
    });

    /**
     * Switching field builds a fresh condition rather than carrying operands
     * across. "contains hero" has no meaning as a file size, and a half-migrated
     * row that still compiles is worse than an obviously blank one.
     */
    function setField(key: FieldKey) {
        const [type, field] = key.split(":");
        switch (type) {
            case "text":
                onchange({ type: "text", field: field as TextField, op: "contains", value: "" });
                break;
            case "number":
                onchange({
                    type: "number",
                    field: field as NumField,
                    op: "greater_than_or_equal",
                    value: 0,
                });
                break;
            case "date":
                onchange({
                    type: "date",
                    field: field as "imported_date",
                    op: "within_last",
                    days: 7,
                });
                break;
            case "media_type":
                onchange({ type: "media_type", types: [] });
                break;
            case "extension":
                onchange({ type: "extension", values: [] });
                break;
            case "tags":
                onchange({
                    type: "tags",
                    mode: "all",
                    include: [],
                    exclude: [],
                    untagged: false,
                });
                break;
            case "folder":
                onchange({ type: "folder", ids: [], include_subfolders: false });
                break;
            default:
                onchange({ type: "uncategorized" });
        }
    }

    function setTextOp(op: string) {
        const value = "value" in condition ? String(condition.value ?? "") : "";
        if (op === "is_null" || op === "is_not_null") {
            onchange({ type: "text", field: (condition as any).field, op } as Condition);
        } else {
            onchange({ type: "text", field: (condition as any).field, op, value } as Condition);
        }
    }

    function setNumOp(op: string) {
        const field = (condition as any).field;
        if (op === "between") {
            onchange({ type: "number", field, op: "between", min: 0, max: 0 } as Condition);
        } else {
            onchange({ type: "number", field, op, value: 0 } as Condition);
        }
    }

    function setDateOp(op: string) {
        const field = (condition as any).field;
        if (op === "within_last") {
            onchange({ type: "date", field, op: "within_last", days: 7 } as Condition);
        } else {
            // Today, as a local-midnight instant — the same convention the filter
            // bar uses, because only the client knows the user's timezone.
            const midnight = new Date();
            midnight.setHours(0, 0, 0, 0);
            onchange({ type: "date", field, op, date: midnight.toISOString() } as Condition);
        }
    }

    /** `<input type="date">` wants a bare day; the tree stores an instant. */
    const asDayValue = (iso: string) => {
        const d = new Date(iso);
        const pad = (n: number) => String(n).padStart(2, "0");
        return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
    };
    const fromDayValue = (day: string) => {
        const [y, m, d] = day.split("-").map(Number);
        return new Date(y, m - 1, d).toISOString();
    };

    const selectClass =
        "rounded border border-neutral-700 bg-neutral-950 px-1.5 py-1 text-xs text-neutral-200 " +
        "focus:border-neutral-500 focus:outline-none";
    const inputClass = `${selectClass} min-w-0 flex-1`;
</script>

<div class="flex items-center gap-1.5">
    <select
        class="{selectClass} w-36 shrink-0"
        value={fieldKey}
        onchange={(e) => setField(e.currentTarget.value as FieldKey)}
        aria-label="Field"
    >
        {#each FIELDS as f (f.value)}
            <option value={f.value}>{f.label}</option>
        {/each}
    </select>

    {#if condition.type === "text"}
        <select
            class="{selectClass} w-32 shrink-0"
            value={condition.op}
            onchange={(e) => setTextOp(e.currentTarget.value)}
            aria-label="Operator"
        >
            {#each TEXT_OPS as o (o.value)}
                <option value={o.value}>{o.label}</option>
            {/each}
        </select>
        {#if condition.op !== "is_null" && condition.op !== "is_not_null"}
            <input
                class={inputClass}
                type="text"
                value={condition.value}
                oninput={(e) =>
                    onchange({ ...condition, value: e.currentTarget.value } as Condition)}
                placeholder="value"
                aria-label="Value"
            />
        {/if}
    {:else if condition.type === "number"}
        <select
            class="{selectClass} w-32 shrink-0"
            value={condition.op}
            onchange={(e) => setNumOp(e.currentTarget.value)}
            aria-label="Operator"
        >
            {#each NUM_OPS as o (o.value)}
                <option value={o.value}>{o.label}</option>
            {/each}
        </select>
        {#if condition.op === "between"}
            <input
                class={inputClass}
                type="number"
                value={condition.min}
                oninput={(e) =>
                    onchange({ ...condition, min: e.currentTarget.valueAsNumber || 0 } as Condition)}
                aria-label="Minimum"
            />
            <input
                class={inputClass}
                type="number"
                value={condition.max}
                oninput={(e) =>
                    onchange({ ...condition, max: e.currentTarget.valueAsNumber || 0 } as Condition)}
                aria-label="Maximum"
            />
        {:else}
            <input
                class={inputClass}
                type="number"
                value={condition.value}
                oninput={(e) =>
                    onchange({
                        ...condition,
                        value: e.currentTarget.valueAsNumber || 0,
                    } as Condition)}
                aria-label="Value"
            />
        {/if}
        {#if condition.field === "file_size"}
            <span class="shrink-0 text-[10px] text-neutral-500">bytes</span>
        {:else}
            <span class="shrink-0 text-[10px] text-neutral-500">px</span>
        {/if}
    {:else if condition.type === "date"}
        <select
            class="{selectClass} w-36 shrink-0"
            value={condition.op}
            onchange={(e) => setDateOp(e.currentTarget.value)}
            aria-label="Operator"
        >
            {#each DATE_OPS as o (o.value)}
                <option value={o.value}>{o.label}</option>
            {/each}
        </select>
        {#if condition.op === "within_last"}
            <input
                class={inputClass}
                type="number"
                min="1"
                value={condition.days}
                oninput={(e) =>
                    onchange({
                        ...condition,
                        days: Math.max(1, e.currentTarget.valueAsNumber || 1),
                    } as Condition)}
                aria-label="Days"
            />
        {:else if condition.op !== "between"}
            <input
                class={inputClass}
                type="date"
                value={asDayValue(condition.date)}
                onchange={(e) =>
                    onchange({
                        ...condition,
                        date: fromDayValue(e.currentTarget.value),
                    } as Condition)}
                aria-label="Date"
            />
        {/if}
    {:else if condition.type === "media_type"}
        <div class="flex flex-1 flex-wrap items-center gap-1">
            {#each ASSET_TYPE_LABELS as { value: t, label } (t)}
                {@const on = condition.types.includes(t)}
                <button
                    type="button"
                    aria-pressed={on}
                    onclick={() =>
                        onchange({
                            ...condition,
                            types: on
                                ? condition.types.filter((x: AssetTypeFilter) => x !== t)
                                : [...condition.types, t],
                        } as Condition)}
                    class="rounded px-2 py-0.5 text-xs capitalize transition-colors
                           {on
                        ? 'bg-blue-600 text-white'
                        : 'bg-neutral-800 text-neutral-400 hover:bg-neutral-700'}"
                >
                    {label}
                </button>
            {/each}
        </div>
    {:else if condition.type === "extension"}
        <input
            class={inputClass}
            type="text"
            value={condition.values.join(", ")}
            oninput={(e) =>
                onchange({
                    ...condition,
                    values: e.currentTarget.value
                        .split(",")
                        .map((v) => v.trim())
                        .filter(Boolean),
                } as Condition)}
            placeholder="png, webp"
            aria-label="Extensions"
        />
    {:else if condition.type === "tags"}
        <select
            class="{selectClass} w-28 shrink-0"
            value={condition.mode}
            onchange={(e) =>
                onchange({ ...condition, mode: e.currentTarget.value } as Condition)}
            aria-label="Tag match mode"
        >
            {#each TAG_MODES as m (m.value)}
                <option value={m.value}>{m.label}</option>
            {/each}
        </select>

        <!-- Chips rather than the filter bar's popover: a rule row is already a
             row of controls, and a dropdown inside one is a menu inside a menu.
             Scrolls once a library has more tags than fit.

             TRI-STATE, cycling neutral → include → exclude → neutral. The engine
             has always supported both lists; an editor that could only write
             `include` made "everything except drafts" — the most common tag rule
             there is — impossible to express here. -->
        <div class="flex max-h-16 min-w-0 flex-1 flex-wrap gap-1 overflow-y-auto [scrollbar-width:thin]">
            {#if assetLibrary.tags.length === 0}
                <span class="text-xs text-neutral-600">No tags in this library yet.</span>
            {/if}
            {#each assetLibrary.tags as tag (tag.id)}
                {@const included = condition.include.includes(tag.id)}
                {@const excluded = condition.exclude.includes(tag.id)}
                <button
                    type="button"
                    title={included
                        ? "Included — click to exclude"
                        : excluded
                          ? "Excluded — click to clear"
                          : "Click to include"}
                    aria-pressed={included || excluded}
                    onclick={() =>
                        onchange({
                            ...condition,
                            include: included
                                ? condition.include.filter((x: string) => x !== tag.id)
                                : excluded
                                  ? condition.include
                                  : [...condition.include, tag.id],
                            exclude: excluded
                                ? condition.exclude.filter((x: string) => x !== tag.id)
                                : included
                                  ? [...condition.exclude, tag.id]
                                  : condition.exclude,
                        } as Condition)}
                    class="rounded px-1.5 py-0.5 text-xs transition-colors
                           {included
                        ? 'bg-blue-600 text-white'
                        : excluded
                          ? 'bg-red-700 text-white line-through'
                          : 'bg-neutral-800 text-neutral-400 hover:bg-neutral-700'}"
                >
                    {tag.name}
                </button>
            {/each}
        </div>

        <!-- A three-state control has to say so. Without this legend the chips
             read as broken the first time a click lands on a state the user
             didn't expect — which is exactly how it was reported. -->
        <span class="shrink-0 whitespace-nowrap text-[10px] leading-tight text-neutral-600">
            click:<br /><span class="text-blue-400">include</span> ·
            <span class="text-red-400">exclude</span>
        </span>

        <label class="flex shrink-0 items-center gap-1 text-xs text-neutral-400">
            <input
                type="checkbox"
                checked={condition.untagged}
                onchange={(e) =>
                    onchange({ ...condition, untagged: e.currentTarget.checked } as Condition)}
                class="h-3 w-3 accent-blue-600"
            />
            untagged
        </label>
    {:else if condition.type === "folder"}
        <!-- `negate` as an operator, `include_subfolders` as a modifier — the
             same split the engine makes. Offering "is not in" + "with
             subfolders" as two sibling operators reads like it means something
             and doesn't. -->
        <select
            class="{selectClass} w-32 shrink-0"
            value={condition.negate ? "not_in" : "in"}
            onchange={(e) =>
                onchange({
                    ...condition,
                    negate: e.currentTarget.value === "not_in",
                } as Condition)}
            aria-label="Operator"
        >
            <option value="in">is in</option>
            <option value="not_in">is not in</option>
        </select>

        <div
            class="max-h-24 min-w-0 flex-1 overflow-y-auto rounded border border-neutral-800
                   [scrollbar-width:thin]"
        >
            {#if folderRows.length === 0}
                <p class="px-2 py-1 text-xs text-neutral-600">No folders in this library yet.</p>
            {/if}
            {#each folderRows as { folder, depth } (folder.id)}
                {@const on = condition.ids.includes(folder.id)}
                <button
                    type="button"
                    aria-pressed={on}
                    title={folder.name}
                    onclick={() =>
                        onchange({
                            ...condition,
                            ids: on
                                ? condition.ids.filter((x: string) => x !== folder.id)
                                : [...condition.ids, folder.id],
                        } as Condition)}
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

        <label
            class="flex shrink-0 items-center gap-1 text-xs text-neutral-400"
            title="Also match assets in folders nested under the ones picked"
        >
            <input
                type="checkbox"
                checked={condition.include_subfolders ?? false}
                onchange={(e) =>
                    onchange({
                        ...condition,
                        include_subfolders: e.currentTarget.checked,
                    } as Condition)}
                class="h-3 w-3 accent-blue-600"
            />
            + subfolders
        </label>
    {:else}
        <span class="flex-1 text-xs text-neutral-500">is in no folder</span>
    {/if}

    <button
        type="button"
        onclick={onremove}
        title="Remove condition"
        aria-label="Remove condition"
        class="grid h-6 w-6 shrink-0 place-items-center rounded text-neutral-500
               transition-colors hover:bg-neutral-800 hover:text-neutral-200"
    >
        <Minus class="h-3.5 w-3.5" />
    </button>
</div>
