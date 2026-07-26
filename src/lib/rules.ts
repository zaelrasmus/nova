import type {
  AssetTypeFilter,
  ColorFilter,
  DateField,
  FilterSet,
  Shape,
  SizeRange,
  TagFilter,
} from "./assets.svelte";

/**
 * The rule language, mirrored from `src-tauri/src/rules.rs`.
 *
 * This is a FILE FORMAT: every saved filter on disk is written in it, and from
 * Phase 2 every smart folder too. The exact JSON shape is pinned by tests on the
 * Rust side (`condition_json_shape_is_stable`) — if these types and those tests
 * disagree, the tests are right.
 *
 * ## Why the filter bar still speaks in dimensions
 *
 * The tree is strictly more expressive than the filter bar's UI, which offers
 * one control per dimension and ANDs them. Rather than rewrite every control to
 * edit tree nodes, the bar keeps its flat `FilterSet` as a VIEW MODEL and this
 * module converts at the boundary: flat → tree on the way to SQL, tree → flat
 * when a saved filter is applied.
 *
 * That keeps one engine underneath (a smart folder and the filter bar compile
 * identically) without pretending the filter bar can express a nested `any`.
 * A tree it can't represent — nested groups, `none`, text conditions — round-
 * trips through `unrepresentable()` instead of being silently flattened into
 * something that means something else.
 */

export type GroupOp = "all" | "any" | "none";

export type TextField = "name" | "notes" | "source_url" | "folder_name";
export type NumField = "width" | "height" | "file_size";

export type TextOp =
  | { op: "contains"; value: string }
  | { op: "excludes"; value: string }
  | { op: "begins_with"; value: string }
  | { op: "ends_with"; value: string }
  | { op: "equals"; value: string }
  | { op: "is_null" }
  | { op: "is_not_null" };

export type NumOp =
  | { op: "equals"; value: number }
  | { op: "greater_than"; value: number }
  | { op: "greater_than_or_equal"; value: number }
  | { op: "less_than"; value: number }
  | { op: "less_than_or_equal"; value: number }
  | { op: "between"; min: number; max: number };

export type DateOp =
  | { op: "before"; date: string }
  | { op: "after"; date: string }
  | { op: "on"; date: string }
  | { op: "between"; from: string; until: string }
  /** Rolling window, evaluated at query time — never frozen when saved. */
  | { op: "within_last"; days: number };

export type Condition =
  | ({ type: "text"; field: TextField } & TextOp)
  | ({ type: "number"; field: NumField } & NumOp)
  | ({ type: "date"; field: DateField } & DateOp)
  | { type: "tags"; mode: TagFilter["mode"]; include: string[]; exclude: string[]; untagged: boolean }
  | { type: "media_type"; negate?: boolean; types: AssetTypeFilter[] }
  | { type: "extension"; negate?: boolean; values: string[] }
  | { type: "shape"; negate?: boolean; shape: Shape }
  | ({ type: "color" } & ColorFilter)
  | { type: "folder"; negate?: boolean; ids: string[]; include_subfolders?: boolean }
  | { type: "uncategorized"; negate?: boolean };

export type RuleNode =
  | { kind: "group"; op: GroupOp; children: RuleNode[] }
  | ({ kind: "condition" } & Condition);

/** Deepest nesting the editor allows. Mirrors MAX_DEPTH in rules.rs. */
export const MAX_DEPTH = 2;

export const emptyRules = (): RuleNode => ({ kind: "group", op: "all", children: [] });

const condition = (c: Condition): RuleNode => ({ kind: "condition", ...c }) as RuleNode;

/** Does this tree constrain anything, or is it a half-built editor state? */
export function isActive(node: RuleNode): boolean {
  if (node.kind === "group") return node.children.some(isActive);
  switch (node.type) {
    case "text":
      return node.op === "is_null" || node.op === "is_not_null"
        ? true
        : node.value.trim().length > 0;
    case "media_type":
      return node.types.length > 0;
    case "extension":
      return node.values.length > 0;
    case "folder":
      return node.ids.length > 0;
    case "tags":
      return node.untagged || node.include.length > 0 || node.exclude.length > 0;
    default:
      return true;
  }
}

// ── Human-readable summaries ────────────────────────────────────────────────

const TEXT_FIELD_LABELS: Record<TextField, string> = {
  name: "Name",
  notes: "Notes",
  source_url: "Source URL",
  folder_name: "Folder",
};

const NUM_FIELD_LABELS: Record<NumField, string> = {
  width: "Width",
  height: "Height",
  file_size: "Size",
};

const OP_LABELS: Record<string, string> = {
  contains: "contains",
  excludes: "doesn't contain",
  begins_with: "starts with",
  ends_with: "ends with",
  equals: "is",
  is_null: "is empty",
  is_not_null: "is set",
  greater_than: ">",
  greater_than_or_equal: "≥",
  less_than: "<",
  less_than_or_equal: "≤",
  between: "between",
  before: "before",
  after: "after",
  on: "on",
  within_last: "within last",
};

/**
 * One short phrase per condition — the chips a rule set shows in a tooltip, a
 * sidebar flyout, or the editor's summary line.
 *
 * Deliberately lossy: it says which dimensions are involved, not the exact
 * values of every operand, because these read at a glance in cramped places.
 */
export function describeConditions(node: RuleNode | null): string[] {
  if (!node) return [];
  if (node.kind === "group") return node.children.flatMap(describeConditions);

  const op = (o: string) => OP_LABELS[o] ?? o;
  switch (node.type) {
    case "text":
      return [
        node.op === "is_null" || node.op === "is_not_null"
          ? `${TEXT_FIELD_LABELS[node.field]} ${op(node.op)}`
          : `${TEXT_FIELD_LABELS[node.field]} ${op(node.op)} "${node.value}"`,
      ];
    case "number":
      return [
        node.op === "between"
          ? `${NUM_FIELD_LABELS[node.field]} between ${node.min}–${node.max}`
          : `${NUM_FIELD_LABELS[node.field]} ${op(node.op)} ${node.value}`,
      ];
    case "date": {
      const field = node.field.replace("_date", "").replace("_", " ");
      if (node.op === "within_last") return [`${field} within last ${node.days}d`];
      if (node.op === "between") return [`${field} in range`];
      return [`${field} ${op(node.op)}`];
    }
    case "tags": {
      const bits: string[] = [];
      if (node.untagged) bits.push("untagged");
      if (node.include.length) bits.push(`${node.include.length} tag(s)`);
      if (node.exclude.length) bits.push(`excluding ${node.exclude.length}`);
      return bits.length ? [bits.join(", ")] : [];
    }
    case "media_type":
      return [`${node.negate ? "not " : ""}${node.types.join(", ")}`];
    case "extension":
      return [`${node.negate ? "not " : ""}${node.values.join(", ")}`];
    case "shape":
      return [`shape: ${node.shape.kind}`];
    case "color":
      return ["colour"];
    case "folder":
      return [`${node.negate ? "not in " : "in "}${node.ids.length} folder(s)`];
    case "uncategorized":
      return [node.negate ? "in a folder" : "uncategorized"];
  }
}

/** The same, as one line. Empty rule sets say so rather than reading blank. */
export function describeRules(node: RuleNode | null): string {
  const parts = describeConditions(node);
  return parts.length ? parts.join(" · ") : "no conditions";
}

// ── Filter bar ⇄ tree ───────────────────────────────────────────────────────

/**
 * The filter bar's dimensions as a flat `all` group.
 *
 * Order is stable and matches the bar's own layout, so a saved filter's stored
 * JSON doesn't churn just because the user toggled dimensions in a different
 * sequence.
 */
export function toRuleTree(f: FilterSet): RuleNode | null {
  const children: RuleNode[] = [];

  if (f.asset_types.length > 0) {
    children.push(condition({ type: "media_type", types: [...f.asset_types] }));
  }
  if (f.shape) {
    children.push(condition({ type: "shape", shape: f.shape }));
  }
  if (f.date && (f.date.from || f.date.until)) {
    // The bar can leave one end open; the tree has an operator for each case
    // rather than a range with nullable bounds.
    const { field, from, until } = f.date;
    if (from && until) {
      children.push(condition({ type: "date", field, op: "between", from, until }));
    } else if (from) {
      children.push(condition({ type: "date", field, op: "after", date: from }));
    } else if (until) {
      children.push(condition({ type: "date", field, op: "before", date: until }));
    }
  }
  if (f.size && (f.size.min !== null || f.size.max !== null)) {
    const { min, max } = f.size;
    if (min !== null && max !== null) {
      children.push(condition({ type: "number", field: "file_size", op: "between", min, max }));
    } else if (min !== null) {
      children.push(
        condition({ type: "number", field: "file_size", op: "greater_than_or_equal", value: min }),
      );
    } else if (max !== null) {
      children.push(
        condition({ type: "number", field: "file_size", op: "less_than_or_equal", value: max! }),
      );
    }
  }
  if (f.color) {
    children.push(condition({ type: "color", ...f.color }));
  }
  if (f.tags) {
    const { mode, include, exclude, untagged } = f.tags;
    if (untagged || include.length > 0 || exclude.length > 0) {
      children.push(condition({ type: "tags", mode, include, exclude, untagged }));
    }
  }

  return children.length > 0 ? { kind: "group", op: "all", children } : null;
}

/**
 * A tree back into the filter bar's dimensions.
 *
 * Returns `null` when the tree says something the bar cannot show — a nested
 * group, a `none`, a text or folder condition, two conditions on one dimension.
 * Callers must treat that as "open this in the rule editor", never as an empty
 * filter: silently dropping conditions would leave the user looking at more
 * assets than they asked for with no indication why.
 */
export function fromRuleTree(node: RuleNode | null): Partial<FilterSet> | null {
  if (!node) return {};
  if (node.kind !== "group" || node.op !== "all") return null;

  const out: Partial<FilterSet> = {};
  const claim = <K extends keyof FilterSet>(key: K, value: FilterSet[K]): boolean => {
    if (key in out) return false; // two conditions on one dimension
    out[key] = value;
    return true;
  };

  for (const child of node.children) {
    if (child.kind !== "condition") return null;

    switch (child.type) {
      case "media_type":
        if (child.negate || !claim("asset_types", [...child.types])) return null;
        break;
      case "shape":
        if (child.negate || !claim("shape", child.shape)) return null;
        break;
      case "date": {
        const field = child.field;
        let range: SizeRangeLike | null = null;
        if (child.op === "between") range = { from: child.from, until: child.until };
        else if (child.op === "after") range = { from: child.date, until: null };
        else if (child.op === "before") range = { from: null, until: child.date };
        else return null; // `on` / `within_last` have no control in the bar
        if (!claim("date", { field, ...range })) return null;
        break;
      }
      case "number": {
        if (child.field !== "file_size") return null;
        let size: SizeRange | null = null;
        if (child.op === "between") size = { min: child.min, max: child.max };
        else if (child.op === "greater_than_or_equal") size = { min: child.value, max: null };
        else if (child.op === "less_than_or_equal") size = { min: null, max: child.value };
        else return null;
        if (!claim("size", size)) return null;
        break;
      }
      case "color": {
        const { r, g, b, tolerance, min_coverage } = child;
        if (!claim("color", { r, g, b, tolerance, min_coverage })) return null;
        break;
      }
      case "tags": {
        const { mode, include, exclude, untagged } = child;
        if (!claim("tags", { mode, include, exclude, untagged })) return null;
        break;
      }
      default:
        // text / extension / folder / uncategorized — smart-folder territory.
        return null;
    }
  }

  return out;
}

/** Shape of a date range mid-conversion, before the field is attached. */
interface SizeRangeLike {
  from: string | null;
  until: string | null;
}
