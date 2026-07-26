import type { DateField, PinColor } from "./assets.svelte";
import { describeRules, type RuleNode } from "./rules";

/**
 * The Quick Action step language, mirrored from `src-tauri/src/actions.rs`.
 *
 * This is a FILE FORMAT: every saved action on disk is written in it. The exact
 * JSON shape is pinned by tests on the Rust side (`wire_tests`) — if these types
 * and those tests disagree, the tests are right.
 *
 * ## What an action is
 *
 * A smart folder is a PLACE and a saved filter is a LENS. An action is a VERB:
 * it changes assets rather than describing them, which is why it lives in the
 * grid toolbar next to the other controls that act on the current view, and
 * never in the sidebar.
 *
 * Two properties the UI must not undermine:
 *
 * * The selection is snapshotted at trigger time and sent once. Never re-read it
 *   while a run is in flight — an action that changes what matches the current
 *   scope makes assets vanish from the grid *as it runs*.
 * * Every step records its inverse as it applies, so a run is undoable as a
 *   unit. Undo is scoped to the RUN, not to a global stack: a bulk change is the
 *   only invisible edit in Nova, and a run-scoped Undo has no ambiguity about
 *   what it reverses.
 */

export type TextMode = "replace" | "append" | "prepend";

export type Op =
  | { type: "add_tags"; tag_ids: string[] }
  | { type: "remove_tags"; tag_ids: string[] }
  | { type: "clear_all_tags" }
  | { type: "add_to_folder"; folder_id: string }
  | { type: "remove_from_folder"; folder_id: string }
  /** Be in exactly these folders. An empty list files the assets nowhere. */
  | { type: "set_folders"; folder_ids: string[] }
  | { type: "set_note"; mode: TextMode; text: string }
  | { type: "set_source_url"; url: string }
  | {
      type: "rename_with_pattern";
      pattern: string;
      /**
       * What `{index}` counts in. Stored in the STEP, never inherited from the
       * view — an action has to number the same way every time it runs.
       */
      index_order: RenameOrder;
      index_ascending: boolean;
      index_start: number;
      index_pad: number;
      date_field: DateField;
    };

export type RenameOrder =
  | "filename"
  | "imported_date"
  | "creation_date"
  | "modified_date"
  | "file_size";

/** One before/after pair from the live preview. */
export interface RenameSample {
  before: string;
  after: string;
}

export interface RenamePreview {
  rows: RenameSample[];
  /** A half-typed pattern, shown inline rather than raised as a failure. */
  error: string | null;
}

/**
 * Tokens the pattern box understands.
 *
 * There is deliberately no `{ext}`: the extension comes from the file's real
 * bytes and is appended after rendering, so a pattern is structurally unable to
 * change it.
 */
export const RENAME_TOKENS: { token: string; hint: string }[] = [
  { token: "{name}", hint: "current name, without the extension" },
  { token: "{index}", hint: "position in the chosen order" },
  { token: "{date}", hint: "the chosen date, as YYYY-MM-DD" },
  { token: "{width}", hint: "pixel width" },
  { token: "{height}", hint: "pixel height" },
];

export type OpType = Op["type"];

/**
 * One step: an operation, and optionally a condition gating it.
 *
 * The condition is the same `RuleNode` tree the smart folder editor writes,
 * compiled by the same compiler — which is what turns a macro into a rules
 * engine without a second language.
 *
 * `op` is NESTED rather than flattened alongside `when`. Mirrors the Rust shape,
 * which is nested for a reason: `#[serde(flatten)]` over an internally-tagged
 * enum round-trips lossily, and this file is a format.
 */
export interface Step {
  op: Op;
  /** `null`/absent = applies to the whole selection. */
  when?: RuleNode | null;
}

export interface QuickAction {
  id: string;
  name: string;
  /** Lucide icon name. */
  icon: string | null;
  color: PinColor | null;
  /** 1..9, bound to Ctrl+Shift+&lt;n&gt;. */
  shortcut: number | null;
  position: number;
  steps: Step[];
}

/** What the editor sends. `id` and `position` are the store's business. */
export interface QuickActionDraft {
  name: string;
  icon?: string | null;
  color?: string | null;
  shortcut?: number | null;
  steps: Step[];
}

/** The dry run behind the confirmation. Read-only. */
export interface RunPreview {
  name: string;
  asset_count: number;
  step_count: number;
  will_be_undoable: boolean;
  /** Non-empty means the run is BLOCKED, not merely risky. */
  problems: string[];
  /**
   * Legal but probably unintended — e.g. a pattern that gives thousands of
   * assets the same name. Shown in the confirmation; never blocks.
   */
  warnings: string[];
}

export interface RunSummary {
  run_id: string;
  name: string;
  asset_count: number;
  is_undoable: boolean;
}

export interface UndoSummary {
  name: string;
  restored: number;
  /** Assets deleted since the run, which cannot be restored to. */
  skipped: number;
}

export interface ActionRun {
  id: string;
  name: string;
  ran_at: string;
  asset_count: number;
  is_undoable: boolean;
}

/**
 * Above this many assets, a run asks first.
 *
 * Not a safety net for undo — a threshold exists because at five assets the
 * result is visible on screen and at five thousand it isn't. Confirming a
 * three-asset run would train the user to dismiss the dialog unread, which is
 * exactly what makes it useless at five thousand.
 */
export const CONFIRM_THRESHOLD = 100;

/** A blank, unconditional step, for the editor's "add step" button. */
export const emptyStep = (type: OpType): Step => ({ op: emptyOp(type) });

export function emptyOp(type: OpType): Op {
  switch (type) {
    case "add_tags":
    case "remove_tags":
      return { type, tag_ids: [] };
    case "clear_all_tags":
      return { type };
    case "add_to_folder":
    case "remove_from_folder":
      return { type, folder_id: "" };
    case "set_folders":
      return { type, folder_ids: [] };
    case "set_note":
      return { type, mode: "replace", text: "" };
    case "set_source_url":
      return { type, url: "" };
    case "rename_with_pattern":
      return {
        type,
        pattern: "",
        index_order: "imported_date",
        index_ascending: true,
        index_start: 1,
        index_pad: 3,
        date_field: "imported_date",
      };
  }
}

/**
 * Does this step do anything, or is it a half-built editor row?
 *
 * Mirrors `Step::is_active` in Rust, including its asymmetry: only the tag steps
 * and a blank append can be empty. `set_folders` with no targets files assets
 * nowhere and `set_note` with no text clears the note — both are real
 * instructions, so treating "empty" as "unfinished" would make them
 * unexpressible.
 */
export function isActive(step: Step): boolean {
  const op = step.op;
  switch (op.type) {
    case "add_tags":
    case "remove_tags":
      return op.tag_ids.length > 0;
    case "add_to_folder":
    case "remove_from_folder":
      return op.folder_id !== "";
    case "set_note":
      return op.mode === "replace" || op.text.trim().length > 0;
    case "rename_with_pattern":
      return op.pattern.trim().length > 0;
    default:
      return true;
  }
}

const STEP_LABELS: Record<OpType, string> = {
  add_tags: "Add tags",
  remove_tags: "Remove tags",
  clear_all_tags: "Clear all tags",
  add_to_folder: "Add to folder",
  remove_from_folder: "Remove from folder",
  set_folders: "Set folders",
  set_note: "Set note",
  set_source_url: "Set source URL",
  rename_with_pattern: "Rename",
};

/**
 * Grouped for the editor's type picker, in the order they appear there. Tags,
 * then folders, then text — the same grouping the steps have in Rust.
 */
export const STEP_GROUPS: { label: string; types: OpType[] }[] = [
  { label: "Tags", types: ["add_tags", "remove_tags", "clear_all_tags"] },
  { label: "Folders", types: ["add_to_folder", "remove_from_folder", "set_folders"] },
  { label: "Text", types: ["set_note", "set_source_url"] },
  { label: "Naming", types: ["rename_with_pattern"] },
];

export const stepLabel = (type: OpType): string => STEP_LABELS[type];

/**
 * Resolvers for the ids a step carries.
 *
 * Passed IN rather than read from the library store: this module is the file
 * format, and a saved action can reference a tag or folder that no longer
 * exists. An unresolved id degrades to "a deleted tag" instead of rendering
 * blank — which is also precisely the state the run validator blocks on.
 */
export interface NameLookup {
  tag?: (id: string) => string | undefined;
  folder?: (id: string) => string | undefined;
  /** For rendering a step's condition, which `describeRules` resolves itself. */
  folderNames?: ReadonlyMap<string, string>;
}

const list = (names: string[], noun: string): string =>
  names.length === 0 ? "nothing" : names.length <= 3 ? names.join(", ") : `${names.length} ${noun}`;

/** One short phrase per step, for the menu tooltip and the confirmation. */
export function describeStep(step: Step, names: NameLookup = {}): string {
  const base = describeOp(step.op, names);
  // The condition reads as a suffix, matching the order it applies in: the step
  // is what happens, the rule is what it happens to.
  return step.when ? `${base} (${describeRules(step.when, names.folderNames)})` : base;
}

function describeOp(op: Op, names: NameLookup = {}): string {
  const tag = (id: string) => names.tag?.(id) ?? "a deleted tag";
  const folder = (id: string) => names.folder?.(id) ?? "a deleted folder";
  const label = STEP_LABELS[op.type];

  switch (op.type) {
    case "add_tags":
    case "remove_tags":
      return `${label}: ${list(op.tag_ids.map(tag), "tags")}`;
    case "clear_all_tags":
      return label;
    case "add_to_folder":
    case "remove_from_folder":
      return `${label}: ${folder(op.folder_id)}`;
    case "set_folders":
      // The empty case has a name of its own — "Set folders: nothing" reads as a
      // broken step, and this is the operation that makes an asset uncategorised.
      return op.folder_ids.length === 0
        ? "Remove from all folders"
        : `${label}: ${list(op.folder_ids.map(folder), "folders")}`;
    case "set_note": {
      if (op.mode === "replace" && op.text.trim() === "") return "Clear note";
      const verb = op.mode === "replace" ? "Set" : op.mode === "append" ? "Append to" : "Prepend to";
      return `${verb} note`;
    }
    case "set_source_url":
      return op.url.trim() === "" ? "Clear source URL" : label;
    case "rename_with_pattern":
      return `${label}: ${op.pattern}`;
  }
}

/** The same, as one line. Empty pipelines say so rather than reading blank. */
export function describeSteps(steps: Step[], names: NameLookup = {}): string {
  const parts = steps.filter(isActive).map((s) => describeStep(s, names));
  return parts.length ? parts.join(" · ") : "no steps";
}
