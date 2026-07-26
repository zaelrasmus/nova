import { toast } from "svelte-sonner";
import { assetLibrary } from "$lib/assets.svelte";
import { selection } from "$lib/selection.svelte";
import { CONFIRM_THRESHOLD, describeStep, type NameLookup, type QuickAction } from "$lib/actions";

/**
 * Running an action, shared by the ⚡ menu and the keyboard shortcuts.
 *
 * Deliberately not a component method: the same run has to be reachable from a
 * keydown handler that has no menu open, and duplicating the snapshot rules in
 * two places is exactly how one of them would end up re-reading the selection.
 */

const fail = (e: unknown, fallback: string) =>
    toast.error(typeof e === "string" ? e : fallback);

/**
 * Apply an action to the current selection.
 *
 * The ids are copied ONCE, here, before anything else can happen. A run takes
 * time; meanwhile the manifest streams and the grid re-renders, and a run that
 * changes what matches the current scope will make assets disappear from view
 * while it's still going. Reading `selection.ids` again at any later point would
 * apply the rest of the pipeline to a set the user never chose.
 */
export async function runAction(action: QuickAction): Promise<void> {
    const assetIds = selection.assetIds;
    if (assetIds.length === 0) {
        toast.error("Select some assets first.");
        return;
    }

    if (assetIds.length > CONFIRM_THRESHOLD && !(await confirmRun(action, assetIds))) return;

    try {
        const summary = await assetLibrary.runQuickAction(action.id, assetIds);
        const what = `${summary.name} · ${summary.asset_count.toLocaleString()} ${
            summary.asset_count === 1 ? "asset" : "assets"
        }`;
        if (summary.is_undoable) {
            toast.success(what, {
                action: { label: "Undo", onClick: () => void undoRun(summary.run_id) },
            });
        } else {
            // Say so at the moment it matters. Discovering there's no undo when
            // you reach for undo is the worst possible time to learn it.
            toast.success(`${what} — too large to undo`);
        }
    } catch (e) {
        fail(e, "The action didn't run.");
    }
}

/**
 * The dry run, above the confirmation threshold.
 *
 * A plain confirm carries everything a tag pipeline can say about itself. When
 * rename lands it will need a real dialog — a pattern is only judgeable against
 * sample output — and this is where that grows.
 */
async function confirmRun(action: QuickAction, assetIds: string[]): Promise<boolean> {
    let preview;
    try {
        preview = await assetLibrary.previewActionRun(action.id, assetIds);
    } catch (e) {
        fail(e, "Couldn't check the action.");
        return false;
    }

    // Problems BLOCK. A step pointing at a deleted tag would either do nothing
    // or do something unintended, and neither is worth a confirm button.
    if (preview.problems.length > 0) {
        toast.error(preview.problems[0]);
        return false;
    }

    const lines = [
        `Run "${preview.name}" on ${preview.asset_count.toLocaleString()} assets?`,
        "",
        ...action.steps.map((s, i) => `${i + 1}. ${describeStep(s, names())}`),
    ];
    // Warnings are legal-but-probably-unintended, so they belong in front of the
    // decision rather than in a toast after it.
    if (preview.warnings.length > 0) {
        lines.push("", ...preview.warnings.map((w) => `⚠ ${w}`));
    }
    if (!preview.will_be_undoable) {
        lines.push("", "This run is too large to record an undo for.");
    }
    return window.confirm(lines.join("\n"));
}

/** Resolvers read fresh, so a tag renamed since the action was saved reads right. */
const names = (): NameLookup => ({
    tag: (id) => assetLibrary.tagNames.get(id),
    folder: (id) => assetLibrary.folderNames.get(id),
    folderNames: assetLibrary.folderNames,
});

export async function undoRun(runId: string): Promise<void> {
    try {
        const summary = await assetLibrary.undoActionRun(runId);
        // Partial success is a real outcome, not a failure: assets deleted since
        // the run can't be restored to, and saying nothing would leave the user
        // believing the undo was complete.
        toast.success(
            summary.skipped > 0
                ? `Undid "${summary.name}" — ${summary.skipped.toLocaleString()} assets no longer exist`
                : `Undid "${summary.name}"`,
        );
    } catch (e) {
        fail(e, "Couldn't undo that run.");
    }
}
