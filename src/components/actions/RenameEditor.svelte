<script lang="ts">
    import { ArrowRight } from "@lucide/svelte";
    import { assetLibrary, type DateField } from "$lib/assets.svelte";
    import { RENAME_TOKENS, type Op, type RenameOrder, type RenameSample } from "$lib/actions";

    /**
     * The rename pattern box, its knobs, and its live preview.
     *
     * Shared by the step editor and the standalone Rename dialog, deliberately:
     * the preview's whole claim is that it shows what the run will produce, and
     * two copies of this control is how that stops being true.
     */
    interface Props {
        /** Always a `rename_with_pattern` op; the parent narrows before mounting. */
        op: Extract<Op, { type: "rename_with_pattern" }>;
        onchange: (next: Op) => void;
        /**
         * Assets to preview against. Empty falls back to library samples, so the
         * control is usable while an action is being written rather than only at
         * the moment it runs.
         */
        assetIds: string[];
        /** Reported up so a parent can block Apply on an unusable pattern. */
        onvalidity?: (error: string | null) => void;
    }

    const { op, onchange, assetIds, onvalidity }: Props = $props();

    const RENAME_ORDERS: { value: RenameOrder; label: string }[] = [
        { value: "filename", label: "name" },
        { value: "imported_date", label: "date imported" },
        { value: "creation_date", label: "date created" },
        { value: "modified_date", label: "date modified" },
        { value: "file_size", label: "file size" },
    ];

    const DATE_FIELDS: { value: DateField; label: string }[] = [
        { value: "imported_date", label: "imported" },
        { value: "creation_date", label: "created" },
        { value: "modified_date", label: "modified" },
    ];

    let samples = $state<RenameSample[]>([]);
    let patternError = $state<string | null>(null);

    /**
     * Renders through the SAME Rust path the run uses, so a preview cannot
     * disagree with the result. Debounced and token-guarded: a slow render from
     * three keystrokes ago must not overwrite the answer for what's on screen.
     */
    let previewToken = 0;
    $effect(() => {
        const snapshot = $state.snapshot(op) as Op;
        const ids = assetIds;
        const token = ++previewToken;
        const timer = setTimeout(async () => {
            try {
                const preview = await assetLibrary.previewRename({ op: snapshot }, ids, 3);
                if (token !== previewToken) return;
                samples = preview.rows;
                patternError = preview.error;
                onvalidity?.(preview.error);
            } catch {
                if (token !== previewToken) return;
                samples = [];
                patternError = null;
                onvalidity?.(null);
            }
        }, 200);
        return () => clearTimeout(timer);
    });

    const controlClass =
        "rounded border border-neutral-700 bg-neutral-950 px-1.5 py-1 text-xs text-neutral-200 " +
        "focus:border-neutral-500 focus:outline-none";
</script>

<div class="flex min-w-0 flex-1 flex-col gap-1.5">
    <input
        type="text"
        value={op.pattern}
        oninput={(e) => onchange({ ...op, pattern: e.currentTarget.value })}
        placeholder="Render_{'{date}'}_{'{index}'}"
        aria-label="Pattern"
        spellcheck="false"
        class="{controlClass} w-full font-mono placeholder:text-neutral-600
               {patternError ? 'border-red-800' : ''}"
    />

    <!-- Click to insert. Typing `{index}` correctly from memory is not a skill
         worth requiring, and a mistyped token is a hard error. -->
    <div class="flex flex-wrap gap-1">
        {#each RENAME_TOKENS as { token, hint } (token)}
            <button
                type="button"
                title={hint}
                onclick={() => onchange({ ...op, pattern: op.pattern + token })}
                class="rounded bg-neutral-800 px-1.5 py-0.5 font-mono text-[10px] text-neutral-400
                       transition-colors hover:bg-neutral-700 hover:text-neutral-200"
            >
                {token}
            </button>
        {/each}
    </div>

    <!-- The knobs {index} and {date} depend on. Visible rather than buried,
         because they're the difference between a rename that renumbers
         consistently and one that doesn't. -->
    <div class="flex flex-wrap items-center gap-1.5 text-[10px] text-neutral-500">
        <span>number by</span>
        <select
            value={op.index_order}
            onchange={(e) => onchange({ ...op, index_order: e.currentTarget.value as RenameOrder })}
            aria-label="Numbering order"
            class={controlClass}
        >
            {#each RENAME_ORDERS as o (o.value)}
                <option value={o.value}>{o.label}</option>
            {/each}
        </select>
        <select
            value={op.index_ascending ? "asc" : "desc"}
            onchange={(e) => onchange({ ...op, index_ascending: e.currentTarget.value === "asc" })}
            aria-label="Numbering direction"
            class={controlClass}
        >
            <option value="asc">ascending</option>
            <option value="desc">descending</option>
        </select>

        <span>from</span>
        <input
            type="number"
            min="0"
            value={op.index_start}
            oninput={(e) => onchange({ ...op, index_start: e.currentTarget.valueAsNumber || 0 })}
            aria-label="Starting number"
            class="{controlClass} w-14"
        />
        <span>padded to</span>
        <input
            type="number"
            min="0"
            max="9"
            value={op.index_pad}
            oninput={(e) =>
                onchange({
                    ...op,
                    index_pad: Math.max(0, Math.min(9, e.currentTarget.valueAsNumber || 0)),
                })}
            aria-label="Number padding"
            class="{controlClass} w-12"
        />

        <span>· date is</span>
        <select
            value={op.date_field}
            onchange={(e) => onchange({ ...op, date_field: e.currentTarget.value as DateField })}
            aria-label="Date field"
            class={controlClass}
        >
            {#each DATE_FIELDS as d (d.value)}
                <option value={d.value}>{d.label}</option>
            {/each}
        </select>
    </div>

    {#if patternError}
        <p class="text-[11px] text-red-400">{patternError}</p>
    {:else if samples.length > 0}
        <div class="flex flex-col gap-0.5 rounded bg-neutral-900/60 p-1.5">
            {#each samples as s (s.before)}
                <div class="flex items-center gap-1.5 font-mono text-[10px]">
                    <span class="min-w-0 flex-1 truncate text-neutral-600">{s.before}</span>
                    <ArrowRight class="h-2.5 w-2.5 shrink-0 text-neutral-700" />
                    <span class="min-w-0 flex-1 truncate text-neutral-300">{s.after}</span>
                </div>
            {/each}
            {#if assetIds.length === 0}
                <span class="mt-0.5 text-[10px] text-neutral-600">
                    Sampled from the library — select assets to preview those.
                </span>
            {/if}
        </div>
    {/if}
</div>
