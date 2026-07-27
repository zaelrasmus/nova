//! The rule language behind Smart Folders and Saved Filters.
//!
//! ## One engine, two products
//!
//! A rule set is a nested tree of `all` / `any` / `none` groups over leaf
//! conditions, compiled here into a single SQL predicate. The same tree is used
//! two ways, and the difference is entirely in the UI:
//!
//!   * a **Smart Folder** contributes its tree as the SCOPE predicate — the set
//!     of rows that exist at all in that place;
//!   * a **Saved Filter** contributes its tree as the LENS predicate — a
//!     narrowing applied on top of whatever scope you're already in.
//!
//! They compose: browsing a smart folder and then applying a saved filter
//! intersects the two, and clearing the filter leaves the folder intact. That
//! composition is the reason the tree is a predicate and not a materialised
//! list.
//!
//! ## Why a variant per field, not a generic (field, operator, value) triple
//!
//! The obvious modelling is three columns: a field name, an operator name, and a
//! stringly-typed value. It reads well in a spec and falls apart in practice —
//! a colour needs five numbers, a ratio needs three, `between` needs two, tags
//! need a list and a match mode. All of that ends up JSON-encoded inside the
//! "value" string, so the type system stops helping exactly where the bugs are.
//!
//! Here each dimension is its own variant carrying exactly what it needs, and
//! each operator carries its own arity (`NumOp::Between { min, max }` rather
//! than an operator plus two nullable columns). Illegal combinations —
//! `between` with one bound, `contains` on a colour — are unrepresentable rather
//! than validated.
//!
//! The cost is honest: a new field is a new variant here and a new editor case
//! in the frontend. That's more code than a generic triple, and far fewer silent
//! wrong answers.
//!
//! ## Text operators and the FTS index
//!
//! `contains` is the one operator with two possible compilations, and picking
//! wrong is the difference between instant and a full scan of 100k rows:
//!
//! | Operator                        | Compiles to |
//! |---------------------------------|-------------|
//! | `contains` (term ≥ 3 chars)     | column-scoped `search_index MATCH` |
//! | `contains` (term < 3 chars)     | `LIKE '%x%'` — trigram can't serve it |
//! | `excludes`                      | `NOT IN` the same MATCH, or `NOT LIKE` |
//! | `begins_with` / `ends_with`     | `LIKE 'x%'` / `LIKE '%x'` |
//! | `equals` / `is_null` / `is_not_null` | plain comparison |
//!
//! Every LIKE pattern is escaped (`%` and `_` are wildcards, and filenames are
//! full of underscores) and carries an explicit `ESCAPE`.

use serde::{Deserialize, Serialize};
use sqlx::{QueryBuilder, Sqlite};


use crate::assets::{AssetType, DateField, Shape, DIMENSIONED};
use crate::tags::TagFilter;

/// Deepest nesting a stored tree may have: the root group plus one level of
/// subgroups. Not a technical limit — a rule editor deeper than this is one
/// nobody can read, and every real rule set fits.
pub const MAX_DEPTH: usize = 2;

/// How a group combines its children.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GroupOp {
    /// Every child must match (AND).
    All,
    /// At least one child must match (OR).
    Any,
    /// No child may match (NOT (… OR …)).
    ///
    /// A third group operator rather than a separate "are true/false" toggle
    /// beside `all`/`any`: same expressiveness for every realistic rule, one
    /// fewer control to explain.
    None,
}

/// A node in the rule tree: either a group of nodes, or a single condition.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuleNode {
    Group {
        op: GroupOp,
        #[serde(default)]
        children: Vec<RuleNode>,
    },
    Condition(Condition),
}

/// Operators over a text column, each carrying its own operand.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum TextOp {
    Contains { value: String },
    Excludes { value: String },
    BeginsWith { value: String },
    EndsWith { value: String },
    Equals { value: String },
    IsNull,
    IsNotNull,
}

/// Operators over a numeric column.
#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum NumOp {
    Equals { value: f64 },
    GreaterThan { value: f64 },
    GreaterThanOrEqual { value: f64 },
    LessThan { value: f64 },
    LessThanOrEqual { value: f64 },
    Between { min: f64, max: f64 },
}

/// Operators over a date column.
///
/// Dates are RFC 3339 strings in the same format `stamp()` writes, so every
/// comparison stays lexicographic against an indexed column.
///
/// Absolute bounds are ABSOLUTE INSTANTS, not calendar days, because only the
/// client knows the user's timezone: it converts the picked LOCAL days into
/// instants, and an "until" is local midnight of the day AFTER the end date — so
/// an inclusive-looking "Jan 1 → Jan 31" still contains everything stamped
/// during the 31st. Interpreting days as UTC here instead would mean a user west
/// of Greenwich clicking "Today" gets nothing they imported that evening.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum DateOp {
    Before { date: String },
    After { date: String },
    /// A single calendar day: `[date, date + 1 day)`.
    On { date: String },
    /// Half-open `[from, until)`, matching how DateFilter has always worked.
    Between { from: String, until: String },
    /// Rolling window. Compiled as an expression evaluated by SQLite at QUERY
    /// time, never resolved to a literal when saved — a smart folder that froze
    /// its own "last 7 days" would defeat the entire point.
    WithinLast { days: i64 },
}

/// Which text column a text condition reads. Each maps to both a real column
/// and an FTS5 column, which is what lets `contains` use the index.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TextField {
    Name,
    Notes,
    SourceUrl,
    /// Names of the folders the asset is filed in (direct membership only,
    /// matching how `search_index.folder_text` is built).
    FolderName,
}

impl TextField {
    /// The real column, for LIKE and NULL tests.
    fn column(self) -> &'static str {
        match self {
            TextField::Name => "a.filename",
            TextField::Notes => "a.notes",
            TextField::SourceUrl => "a.source_url",
            // No single column holds this; handled by `push_folder_name`.
            TextField::FolderName => "",
        }
    }

    /// The FTS5 column, for indexed `contains`.
    fn fts_column(self) -> &'static str {
        match self {
            TextField::Name => "name",
            TextField::Notes => "note",
            TextField::SourceUrl => "url",
            TextField::FolderName => "folder_text",
        }
    }

    /// Is the column nullable? Decides whether a negative predicate has to say
    /// so explicitly — `NULL NOT LIKE 'x'` is NULL, not true, so an asset with
    /// no notes would be silently dropped from "notes don't contain x".
    fn nullable(self) -> bool {
        !matches!(self, TextField::Name)
    }
}

/// Which numeric column a numeric condition reads.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NumField {
    Width,
    Height,
    FileSize,
}

impl NumField {
    fn column(self) -> &'static str {
        match self {
            NumField::Width => "a.width",
            NumField::Height => "a.height",
            NumField::FileSize => "a.file_size",
        }
    }
}

/// One leaf test.
///
/// Several variants carry an existing filter struct verbatim (`TagFilter`,
/// `Shape`, `ColorFilter`). That's deliberate reuse, not laziness: those types
/// already own working, tested SQL emitters, and the tree's job is to combine
/// predicates, not to reinvent the leaves.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Condition {
    Text {
        field: TextField,
        #[serde(flatten)]
        op: TextOp,
    },
    Number {
        field: NumField,
        #[serde(flatten)]
        op: NumOp,
    },
    Date {
        field: DateField,
        #[serde(flatten)]
        op: DateOp,
    },
    /// Tag membership, delegated to the existing `TagFilter` predicate so
    /// "untagged", match modes and exclusions behave identically everywhere.
    Tags(TagFilter),
    /// Media class. Empty list = no constraint; several types OR together.
    MediaType {
        #[serde(default)]
        negate: bool,
        types: Vec<AssetType>,
    },
    /// File extension, compared case-insensitively and without the dot.
    Extension {
        #[serde(default)]
        negate: bool,
        values: Vec<String>,
    },
    Shape {
        #[serde(default)]
        negate: bool,
        shape: Shape,
    },
    /// Dominant-colour proximity, delegated to the existing LAB predicate.
    Color(crate::assets::ColorFilter),
    /// Folder membership.
    ///
    /// `include_subfolders` is a MODIFIER, not an operator: as a sibling
    /// operator you could write `is_not_in + includes_subfolders`, which reads
    /// like it means something and doesn't.
    Folder {
        #[serde(default)]
        negate: bool,
        ids: Vec<String>,
        #[serde(default)]
        include_subfolders: bool,
    },
    /// In no folder at all.
    Uncategorized {
        #[serde(default)]
        negate: bool,
    },
}

impl RuleNode {
    /// An empty root, which constrains nothing.
    pub fn empty() -> Self {
        RuleNode::Group {
            op: GroupOp::All,
            children: Vec::new(),
        }
    }

    /// Does this tree actually constrain anything? An all-empty tree is treated
    /// as "no filter" rather than "match nothing", so a half-built rule in the
    /// editor shows the library instead of a blank grid.
    pub fn is_active(&self) -> bool {
        match self {
            RuleNode::Group { children, .. } => children.iter().any(RuleNode::is_active),
            RuleNode::Condition(c) => c.is_active(),
        }
    }

    /// Reject trees that are nested deeper than the editor can display. Stored
    /// documents are user data and a hand-edited library.db could carry
    /// anything; a runaway tree would compile to unbounded SQL.
    pub fn validate(&self) -> anyhow::Result<()> {
        fn walk(node: &RuleNode, depth: usize) -> anyhow::Result<()> {
            match node {
                RuleNode::Group { children, .. } => {
                    if depth >= MAX_DEPTH {
                        anyhow::bail!("Rule groups may not nest more than {MAX_DEPTH} deep");
                    }
                    for child in children {
                        walk(child, depth + 1)?;
                    }
                    Ok(())
                }
                RuleNode::Condition(_) => Ok(()),
            }
        }
        walk(self, 0)
    }

    /// Emit this tree as one parenthesised SQL predicate.
    pub fn push_predicate<'a>(&'a self, qb: &mut QueryBuilder<'a, Sqlite>) {
        match self {
            RuleNode::Group { op, children } => {
                let active: Vec<&RuleNode> =
                    children.iter().filter(|c| c.is_active()).collect();

                // An empty group constrains nothing — including an empty ANY,
                // which as literal SQL would be `false` and would empty the
                // grid the moment a user added a group before filling it in.
                if active.is_empty() {
                    qb.push("1");
                    return;
                }

                if matches!(op, GroupOp::None) {
                    qb.push("NOT ");
                }
                qb.push("(");
                let joiner = match op {
                    GroupOp::All => " AND ",
                    // `none` is the negation of `any`, so it joins the same way.
                    GroupOp::Any | GroupOp::None => " OR ",
                };
                for (i, child) in active.iter().enumerate() {
                    if i > 0 {
                        qb.push(joiner);
                    }
                    child.push_predicate(qb);
                }
                qb.push(")");
            }
            RuleNode::Condition(c) => c.push_predicate(qb),
        }
    }
}

impl Condition {
    /// Is this condition worth compiling? Blank needles and empty lists are
    /// half-finished editor rows, not "match nothing".
    fn is_active(&self) -> bool {
        match self {
            Condition::Text { op, .. } => match op {
                TextOp::IsNull | TextOp::IsNotNull => true,
                TextOp::Contains { value }
                | TextOp::Excludes { value }
                | TextOp::BeginsWith { value }
                | TextOp::EndsWith { value }
                | TextOp::Equals { value } => !value.trim().is_empty(),
            },
            Condition::Number { .. } | Condition::Date { .. } => true,
            Condition::Tags(f) => f.is_active(),
            Condition::MediaType { types, .. } => !types.is_empty(),
            Condition::Extension { values, .. } => !values.is_empty(),
            Condition::Shape { .. } | Condition::Color(_) => true,
            Condition::Folder { ids, .. } => !ids.is_empty(),
            Condition::Uncategorized { .. } => true,
        }
    }

    fn push_predicate<'a>(&'a self, qb: &mut QueryBuilder<'a, Sqlite>) {
        match self {
            Condition::Text { field, op } => push_text(qb, *field, op),
            Condition::Number { field, op } => push_number(qb, *field, *op),
            Condition::Date { field, op } => push_date(qb, *field, op),

            Condition::Tags(filter) => {
                qb.push("(");
                filter.push_predicate(qb);
                qb.push(")");
            }

            Condition::MediaType { negate, types } => {
                qb.push("(a.asset_type ");
                if *negate {
                    qb.push("NOT ");
                }
                qb.push("IN (");
                let mut list = qb.separated(", ");
                for t in types {
                    list.push_bind(*t);
                }
                qb.push("))");
            }

            Condition::Extension { negate, values } => {
                // Stored without a dot and lowercased at import; normalise the
                // needle the same way so ".WEBP" and "webp" behave alike.
                qb.push("(LOWER(a.extension) ");
                if *negate {
                    qb.push("NOT ");
                }
                qb.push("IN (");
                let mut list = qb.separated(", ");
                for v in values {
                    list.push_bind(v.trim().trim_start_matches('.').to_lowercase());
                }
                qb.push("))");
            }

            Condition::Shape { negate, shape } => {
                // Dimensionless rows (audio, un-probed files) can't have a
                // shape, so they're excluded from BOTH sides — "not landscape"
                // shouldn't sweep in every MP3 in the library.
                //
                // The guard therefore sits OUTSIDE the negation. Wrapping the
                // whole conjunction instead — `NOT (dimensioned AND shape)` —
                // reads the same and means the opposite: for a 0x0 row that is
                // `NOT (false)`, which is TRUE, so every audio file matched
                // every negated shape. See `negated_shape_excludes_dimensionless`.
                qb.push("(").push(DIMENSIONED).push(" AND ");
                if *negate {
                    qb.push("NOT ");
                }
                qb.push("(");
                shape.push_predicate(qb);
                qb.push("))");
            }

            Condition::Color(filter) => {
                qb.push("(");
                filter.push_predicate(qb);
                qb.push(")");
            }

            Condition::Folder {
                negate,
                ids,
                include_subfolders,
            } => push_folder(qb, *negate, ids, *include_subfolders),

            Condition::Uncategorized { negate } => {
                qb.push("(a.id ");
                if *negate {
                    qb.push("IN");
                } else {
                    qb.push("NOT IN");
                }
                qb.push(" (SELECT asset_id FROM assets_folders))");
            }
        }
    }
}

/// Escape LIKE's own wildcards so a literal `_` in a filename doesn't match any
/// character. Paired with an explicit `ESCAPE '\'` at every call site.
fn like_escape(needle: &str) -> String {
    needle
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn push_text<'a>(qb: &mut QueryBuilder<'a, Sqlite>, field: TextField, op: &'a TextOp) {
    // Folder names have no column of their own — they live across a join — so
    // every operator on them is an EXISTS over the membership table.
    if field == TextField::FolderName {
        push_folder_name(qb, op);
        return;
    }

    let col = field.column();
    match op {
        TextOp::IsNull => {
            qb.push("(").push(col).push(" IS NULL OR ").push(col).push(" = '')");
        }
        TextOp::IsNotNull => {
            qb.push("(").push(col).push(" IS NOT NULL AND ").push(col).push(" <> '')");
        }
        TextOp::Equals { value } => {
            qb.push("(").push(col).push(" = ").push_bind(value.as_str()).push(")");
        }
        TextOp::BeginsWith { value } => {
            qb.push("(")
                .push(col)
                .push(" LIKE ")
                .push_bind(format!("{}%", like_escape(value)))
                .push(" ESCAPE '\\')");
        }
        TextOp::EndsWith { value } => {
            qb.push("(")
                .push(col)
                .push(" LIKE ")
                .push_bind(format!("%{}", like_escape(value)))
                .push(" ESCAPE '\\')");
        }
        TextOp::Contains { value } => {
            qb.push("(");
            push_contains(qb, field, col, value, false);
            qb.push(")");
        }
        TextOp::Excludes { value } => {
            qb.push("(");
            push_contains(qb, field, col, value, true);
            // A NULL column is not "containing" the needle, so it belongs in the
            // result — but `NULL NOT LIKE 'x'` is NULL, which would drop it.
            if field.nullable() {
                qb.push(" OR ").push(col).push(" IS NULL");
            }
            qb.push(")");
        }
    }
}

/// The one operator with two compilations. See the table in the module docs.
fn push_contains<'a>(
    qb: &mut QueryBuilder<'a, Sqlite>,
    field: TextField,
    col: &'static str,
    needle: &'a str,
    negate: bool,
) {
    match crate::search::query::column_phrase(field.fts_column(), needle) {
        // Indexed path: the trigram index answers this without touching `assets`.
        Some(expr) => {
            qb.push("a.id ");
            if negate {
                qb.push("NOT ");
            }
            qb.push("IN (SELECT asset_id FROM search_index WHERE search_index MATCH ")
                .push_bind(expr)
                .push(")");
        }
        // Needle too short for trigram — a scan is the only option, and it's the
        // honest one. Silently matching nothing would be worse.
        None => {
            qb.push(col);
            if negate {
                qb.push(" NOT");
            }
            qb.push(" LIKE ")
                .push_bind(format!("%{}%", like_escape(needle)))
                .push(" ESCAPE '\\'");
        }
    }
}

/// Folder-name tests, as membership in any folder whose name matches.
fn push_folder_name<'a>(qb: &mut QueryBuilder<'a, Sqlite>, op: &'a TextOp) {
    // "Has no folder name" is the same question as "is uncategorised".
    if matches!(op, TextOp::IsNull) {
        qb.push("(a.id NOT IN (SELECT asset_id FROM assets_folders))");
        return;
    }
    if matches!(op, TextOp::IsNotNull) {
        qb.push("(a.id IN (SELECT asset_id FROM assets_folders))");
        return;
    }

    let negate = matches!(op, TextOp::Excludes { .. });
    qb.push("(a.id ");
    if negate {
        qb.push("NOT ");
    }
    qb.push(
        "IN (SELECT af2.asset_id FROM assets_folders af2 \
         JOIN folders f2 ON f2.id = af2.folder_id WHERE ",
    );

    match op {
        TextOp::Equals { value } => {
            qb.push("f2.name = ").push_bind(value.as_str());
        }
        TextOp::BeginsWith { value } => {
            qb.push("f2.name LIKE ")
                .push_bind(format!("{}%", like_escape(value)))
                .push(" ESCAPE '\\'");
        }
        TextOp::EndsWith { value } => {
            qb.push("f2.name LIKE ")
                .push_bind(format!("%{}", like_escape(value)))
                .push(" ESCAPE '\\'");
        }
        TextOp::Contains { value } | TextOp::Excludes { value } => {
            qb.push("f2.name LIKE ")
                .push_bind(format!("%{}%", like_escape(value)))
                .push(" ESCAPE '\\'");
        }
        TextOp::IsNull | TextOp::IsNotNull => unreachable!("handled above"),
    }
    qb.push("))");
}

fn push_number<'a>(qb: &mut QueryBuilder<'a, Sqlite>, field: NumField, op: NumOp) {
    let col = field.column();
    qb.push("(").push(col);
    match op {
        NumOp::Equals { value } => {
            qb.push(" = ").push_bind(value);
        }
        NumOp::GreaterThan { value } => {
            qb.push(" > ").push_bind(value);
        }
        NumOp::GreaterThanOrEqual { value } => {
            qb.push(" >= ").push_bind(value);
        }
        NumOp::LessThan { value } => {
            qb.push(" < ").push_bind(value);
        }
        NumOp::LessThanOrEqual { value } => {
            qb.push(" <= ").push_bind(value);
        }
        NumOp::Between { min, max } => {
            // Inclusive both ends, and tolerant of reversed bounds: a slider
            // dragged backwards is a UI accident, not a request for no rows.
            qb.push(" BETWEEN ")
                .push_bind(min.min(max))
                .push(" AND ")
                .push_bind(min.max(max));
        }
    }
    qb.push(")");
}

fn push_date<'a>(qb: &mut QueryBuilder<'a, Sqlite>, field: DateField, op: &'a DateOp) {
    let col = field.column();

    // An invalid bound matches nothing rather than being dropped. Silently
    // widening is the dangerous direction: you'd see MORE than you asked for
    // with no signal that the rule was ignored.
    let guard = |qb: &mut QueryBuilder<'a, Sqlite>, stamp: &'a str, cmp: &'static str| {
        if crate::assets::valid_stamp(stamp) {
            qb.push("(").push(col).push(cmp).push_bind(stamp).push(")");
        } else {
            qb.push("(0)");
        }
    };

    match op {
        DateOp::Before { date } => guard(qb, date, " < "),
        DateOp::After { date } => guard(qb, date, " >= "),
        DateOp::On { date } => {
            // One calendar day, half-open. `date` is the day's start.
            if crate::assets::valid_stamp(date) {
                qb.push("(")
                    .push(col)
                    .push(" >= ")
                    .push_bind(date.as_str())
                    .push(" AND ")
                    .push(col)
                    // `strftime` with the stored format, NOT `datetime(...)`.
                    // `datetime` emits "YYYY-MM-DD HH:MM:SS" — a different shape
                    // from every stamp in the database — so the comparison
                    // happened to work only because ASCII puts ' ' before 'T'.
                    // That is precisely the invisible ordering dependency the
                    // `stamp()` doc comment exists to warn about, and the
                    // `within_last` arm below already does it correctly.
                    .push(" < strftime('%Y-%m-%dT%H:%M:%fZ', ")
                    .push_bind(date.as_str())
                    .push(", '+1 day'))");
            } else {
                qb.push("(0)");
            }
        }
        DateOp::Between { from, until } => {
            qb.push("(");
            guard(qb, from, " >= ");
            qb.push(" AND ");
            guard(qb, until, " < ");
            qb.push(")");
        }
        DateOp::WithinLast { days } => {
            // Evaluated by SQLite on every run, never frozen at save time.
            // strftime with this exact format matches the `stamp()` writer, so
            // the comparison stays a plain string comparison and the date
            // indexes remain usable.
            let d = (*days).max(0);
            qb.push("(")
                .push(col)
                .push(" >= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ")
                .push_bind(format!("-{d} days"))
                .push("))");
        }
    }
}

fn push_folder<'a>(
    qb: &mut QueryBuilder<'a, Sqlite>,
    negate: bool,
    ids: &'a [String],
    include_subfolders: bool,
) {
    qb.push("(a.id ");
    if negate {
        qb.push("NOT ");
    }
    qb.push("IN (SELECT asset_id FROM assets_folders WHERE folder_id IN (");

    if include_subfolders {
        // Walk down from each seed. `UNION` (not ALL) also terminates a cycle if
        // one ever got written, which a self-referencing parent_id would create.
        qb.push("WITH RECURSIVE sub(id) AS (SELECT id FROM folders WHERE id IN (");
        let mut seeds = qb.separated(", ");
        for id in ids {
            seeds.push_bind(id.as_str());
        }
        qb.push(") UNION SELECT f.id FROM folders f JOIN sub ON f.parent_id = sub.id) \
                 SELECT id FROM sub");
    } else {
        let mut list = qb.separated(", ");
        for id in ids {
            list.push_bind(id.as_str());
        }
    }

    qb.push(")))");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stored JSON shape, pinned.
    ///
    /// This is a FILE FORMAT — the frontend mirrors it by hand and every saved
    /// filter on disk is written in it. Serde's internally-tagged enums plus
    /// `flatten` compile happily and can still fail to round-trip, so the shape
    /// gets asserted literally rather than assumed.
    #[test]
    fn condition_json_shape_is_stable() {
        let node = RuleNode::Condition(Condition::Text {
            field: TextField::Name,
            op: TextOp::Contains {
                value: "hero".into(),
            },
        });

        let json = serde_json::to_string(&node).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"condition","type":"text","field":"name","op":"contains","value":"hero"}"#
        );

        // And back, so a document written by this build is readable by it.
        let back: RuleNode = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            RuleNode::Condition(Condition::Text {
                field: TextField::Name,
                op: TextOp::Contains { .. }
            })
        ));
    }

    #[test]
    fn group_json_shape_is_stable() {
        let node = RuleNode::Group {
            op: GroupOp::Any,
            children: vec![RuleNode::Condition(Condition::Number {
                field: NumField::FileSize,
                op: NumOp::Between {
                    min: 1.0,
                    max: 2.0,
                },
            })],
        };
        let json = serde_json::to_string(&node).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"group","op":"any","children":[{"kind":"condition","type":"number","field":"file_size","op":"between","min":1.0,"max":2.0}]}"#
        );
        let _back: RuleNode = serde_json::from_str(&json).unwrap();
    }

    fn sql_of(node: &RuleNode) -> String {
        let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new("");
        node.push_predicate(&mut qb);
        qb.sql().to_string()
    }

    /// An empty group must not empty the grid. `any` is the dangerous one: as
    /// literal SQL an empty OR is `false`, so a user who adds a group before
    /// filling it in would watch their library vanish.
    #[test]
    fn empty_groups_constrain_nothing() {
        for op in [GroupOp::All, GroupOp::Any, GroupOp::None] {
            let node = RuleNode::Group {
                op,
                children: vec![],
            };
            assert_eq!(sql_of(&node), "1", "empty {op:?} group");
            assert!(!node.is_active());
        }
    }

    /// Half-finished rows are skipped, not compiled into predicates that match
    /// nothing — typing into an editor shouldn't blank the grid mid-word.
    #[test]
    fn blank_needles_are_inactive() {
        let node = RuleNode::Condition(Condition::Text {
            field: TextField::Notes,
            op: TextOp::Contains {
                value: "   ".into(),
            },
        });
        assert!(!node.is_active());
    }

    #[test]
    fn none_group_negates_an_or() {
        let node = RuleNode::Group {
            op: GroupOp::None,
            children: vec![
                RuleNode::Condition(Condition::Uncategorized { negate: false }),
                RuleNode::Condition(Condition::Uncategorized { negate: true }),
            ],
        };
        let sql = sql_of(&node);
        assert!(sql.starts_with("NOT ("), "got {sql}");
        assert!(sql.contains(" OR "), "got {sql}");
    }

    /// A long needle rides the trigram index; a short one can't and must fall
    /// back to LIKE rather than silently matching nothing.
    #[test]
    fn contains_routes_long_needles_to_fts_and_short_ones_to_like() {
        let long = RuleNode::Condition(Condition::Text {
            field: TextField::Name,
            op: TextOp::Contains {
                value: "hero".into(),
            },
        });
        let sql = sql_of(&long);
        assert!(sql.contains("search_index MATCH"), "got {sql}");

        let short = RuleNode::Condition(Condition::Text {
            field: TextField::Name,
            op: TextOp::Contains { value: "ab".into() },
        });
        let sql = sql_of(&short);
        assert!(sql.contains("LIKE"), "got {sql}");
        assert!(!sql.contains("search_index"), "got {sql}");
    }

    /// `NULL NOT LIKE 'x'` is NULL, not true. Without the explicit IS NULL arm,
    /// "notes don't contain x" would silently drop every asset with no notes.
    #[test]
    fn exclude_on_a_nullable_column_keeps_null_rows() {
        let node = RuleNode::Condition(Condition::Text {
            field: TextField::Notes,
            op: TextOp::Excludes { value: "ab".into() },
        });
        let sql = sql_of(&node);
        assert!(sql.contains("a.notes IS NULL"), "got {sql}");
    }

    /// LIKE's own wildcards must be escaped, or a filename with an underscore
    /// (which is most of them) matches any character in that position.
    #[test]
    fn like_wildcards_are_escaped() {
        assert_eq!(like_escape("a_b%c"), r"a\_b\%c");
        let node = RuleNode::Condition(Condition::Text {
            field: TextField::Name,
            op: TextOp::BeginsWith {
                value: "IMG_".into(),
            },
        });
        assert!(sql_of(&node).contains("ESCAPE"));
    }

    /// The rolling window must stay an expression SQLite evaluates per run. A
    /// smart folder that froze its own "last 7 days" at save time would be a
    /// static folder wearing a smart folder's name.
    #[test]
    fn within_last_stays_relative() {
        let node = RuleNode::Condition(Condition::Date {
            field: crate::assets::DateField::ImportedDate,
            op: DateOp::WithinLast { days: 7 },
        });
        let sql = sql_of(&node);
        assert!(sql.contains("'now'"), "got {sql}");
    }

    #[test]
    fn subfolder_membership_walks_the_tree() {
        let node = RuleNode::Condition(Condition::Folder {
            negate: false,
            ids: vec!["f1".into()],
            include_subfolders: true,
        });
        let sql = sql_of(&node);
        assert!(sql.contains("WITH RECURSIVE"), "got {sql}");
        // UNION, not UNION ALL: a cycle in parent_id would otherwise never end.
        assert!(sql.contains(" UNION SELECT"), "got {sql}");
    }

    /// A smart folder's tree and the filter bar's tree land in the SAME WHERE
    /// and simply AND together. That's what makes "be inside a smart folder,
    /// then narrow with a lens" work with no extra machinery — and what would
    /// break silently if either ever forgot its own parentheses.
    #[test]
    fn scope_and_lens_predicates_are_self_contained() {
        let scope = RuleNode::Group {
            op: GroupOp::Any,
            children: vec![
                RuleNode::Condition(Condition::Uncategorized { negate: false }),
                RuleNode::Condition(Condition::Uncategorized { negate: true }),
            ],
        };
        let sql = sql_of(&scope);
        // Parenthesised as a unit: without this, ANDing a lens onto an `any`
        // scope would bind to the last OR branch only, and the folder would
        // quietly show the wrong assets rather than fail.
        assert!(sql.starts_with('(') && sql.ends_with(')'), "got {sql}");
    }

    #[test]
    fn nesting_deeper_than_the_editor_is_rejected() {
        let leaf = RuleNode::Condition(Condition::Uncategorized { negate: false });
        let ok = RuleNode::Group {
            op: GroupOp::All,
            children: vec![RuleNode::Group {
                op: GroupOp::Any,
                children: vec![leaf.clone()],
            }],
        };
        assert!(ok.validate().is_ok());

        let too_deep = RuleNode::Group {
            op: GroupOp::All,
            children: vec![RuleNode::Group {
                op: GroupOp::Any,
                children: vec![RuleNode::Group {
                    op: GroupOp::All,
                    children: vec![leaf],
                }],
            }],
        };
        assert!(too_deep.validate().is_err());
    }
}

#[cfg(test)]
mod wire_tests {
    use super::*;

    /// The EXACT JSON `toRuleTree` in lib/rules.ts emits for the filter bar.
    ///
    /// The Rust round-trip tests prove this build can read what it writes; this
    /// one proves it can read what the FRONTEND writes, which is the contract
    /// that actually matters and the one a hand-mirrored type can break.
    #[test]
    fn frontend_filter_bar_json_parses() {
        let json = r#"{"kind":"group","op":"all","children":[
            {"kind":"condition","type":"media_type","types":["image"]},
            {"kind":"condition","type":"number","field":"file_size","op":"greater_than_or_equal","value":1000}
        ]}"#;
        let node: RuleNode = serde_json::from_str(json).expect("frontend JSON must parse");
        assert!(node.is_active());
        let sql = {
            let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new("");
            node.push_predicate(&mut qb);
            qb.sql().to_string()
        };
        assert!(sql.contains("a.asset_type"), "got {sql}");
        assert!(sql.contains("a.file_size"), "got {sql}");
    }


    #[test]
    fn frontend_tags_and_video_json_parse() {
        let tags = r#"{"kind":"condition","type":"tags","mode":"all","include":["t1"],"exclude":[],"untagged":false}"#;
        let node: RuleNode = serde_json::from_str(tags).expect("tags JSON must parse");
        assert!(node.is_active(), "tag condition must be active: {node:?}");

        let video = r#"{"kind":"condition","type":"media_type","types":["video"]}"#;
        let node: RuleNode = serde_json::from_str(video).expect("video JSON must parse");
        assert!(node.is_active(), "media_type must be active: {node:?}");
    }
    /// And the whole FilterSet envelope the frontend sends.
    #[test]
    fn frontend_filterset_envelope_parses() {
        let json = r#"{"rules":{"kind":"group","op":"all","children":[
            {"kind":"condition","type":"media_type","types":["video"]}]},"text":null}"#;
        let set: crate::assets::FilterSet =
            serde_json::from_str(json).expect("frontend FilterSet must parse");
        assert!(set.rules.is_some_and(|r| r.is_active()));
    }
}

/// Execution tests: the predicate is built AND run against a real SQLite, so a
/// wrong column name or a mis-bound enum shows up as wrong ROWS rather than as
/// SQL that merely looks plausible.
#[cfg(test)]
mod exec_tests {
    use super::*;
    use sqlx::SqlitePool;

    async fn db() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        for stmt in [
            "CREATE TABLE assets (id TEXT PRIMARY KEY, asset_type TEXT, filename TEXT, \
             notes TEXT, source_url TEXT, file_size INTEGER, width INTEGER, height INTEGER, \
             extension TEXT, imported_date TEXT)",
            "CREATE TABLE assets_tags (asset_id TEXT, tag_id TEXT)",
            "CREATE TABLE assets_folders (folder_id TEXT, asset_id TEXT)",
            "INSERT INTO assets VALUES ('i1','image','a.png',NULL,NULL,10,100,100,'png','2026-01-01T00:00:00.000Z')",
            "INSERT INTO assets VALUES ('v1','video','b.mp4',NULL,NULL,20,100,100,'mp4','2026-01-01T00:00:00.000Z')",
            "INSERT INTO assets VALUES ('a1','audio','c.mp3',NULL,NULL,30,0,0,'mp3','2026-01-01T00:00:00.000Z')",
            "INSERT INTO assets_tags VALUES ('v1','t1')",
        ] {
            sqlx::query(stmt).execute(&pool).await.unwrap();
        }
        pool
    }

    async fn matching(pool: &SqlitePool, node: &RuleNode) -> Vec<String> {
        let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new("SELECT a.id FROM assets a WHERE ");
        node.push_predicate(&mut qb);
        qb.push(" ORDER BY a.id");
        qb.build_query_scalar().fetch_all(pool).await.unwrap()
    }

    #[tokio::test]
    async fn media_type_selects_that_type_only() {
        let pool = db().await;
        let node: RuleNode = serde_json::from_str(
            r#"{"kind":"condition","type":"media_type","types":["video"]}"#,
        )
        .unwrap();
        assert_eq!(matching(&pool, &node).await, vec!["v1"]);
    }

    #[tokio::test]
    async fn tag_include_selects_tagged_assets() {
        let pool = db().await;
        let node: RuleNode = serde_json::from_str(
            r#"{"kind":"condition","type":"tags","mode":"all","include":["t1"],"exclude":[],"untagged":false}"#,
        )
        .unwrap();
        assert_eq!(matching(&pool, &node).await, vec!["v1"]);
    }

    /// A shape test must never answer "yes" for something that has no shape.
    ///
    /// The bug this pins: with the guard written INSIDE the negation, `NOT
    /// (width > 0 AND height > 0 AND ...)` evaluates to `NOT (false)` for a 0x0
    /// audio row — so every MP3 in the library matched "not horizontal". The SQL
    /// read exactly like the intent and meant the opposite of it, which is why
    /// this is an execution test and not an assertion about the generated text.
    #[tokio::test]
    async fn negated_shape_excludes_dimensionless_assets() {
        let pool = db().await;
        let node: RuleNode = serde_json::from_str(
            r#"{"kind":"condition","type":"shape","negate":true,"shape":{"kind":"horizontal"}}"#,
        )
        .unwrap();
        // i1 and v1 are 100x100 — square, so genuinely "not horizontal".
        // a1 is audio at 0x0 and must be absent from BOTH sides of the test.
        assert_eq!(matching(&pool, &node).await, vec!["i1", "v1"]);
    }

    /// The positive side of the same guard, so a future change can't "fix" the
    /// test above by excluding everything.
    #[tokio::test]
    async fn shape_matches_only_dimensioned_assets() {
        let pool = db().await;
        let node: RuleNode = serde_json::from_str(
            r#"{"kind":"condition","type":"shape","negate":false,"shape":{"kind":"square"}}"#,
        )
        .unwrap();
        assert_eq!(matching(&pool, &node).await, vec!["i1", "v1"]);
    }

    /// The filter bar's whole payload: several dimensions ANDed as one group.
    #[tokio::test]
    async fn filter_bar_group_intersects_dimensions() {
        let pool = db().await;
        let node: RuleNode = serde_json::from_str(
            r#"{"kind":"group","op":"all","children":[
                {"kind":"condition","type":"media_type","types":["video","audio"]},
                {"kind":"condition","type":"number","field":"file_size","op":"greater_than_or_equal","value":25}
            ]}"#,
        )
        .unwrap();
        assert_eq!(matching(&pool, &node).await, vec!["a1"]);
    }
}

/// Storage round-trips.
///
/// A smart folder's tree goes through `serde_json::to_string` into `query_json`
/// and back out again — a step the filter bar never takes, since its tree is
/// built fresh per query. Anything that serializes lossily is invisible until a
/// saved rule quietly stops matching.
#[cfg(test)]
mod roundtrip_tests {
    use super::*;

    fn roundtrip(node: &RuleNode) -> RuleNode {
        let json = serde_json::to_string(node).expect("must serialize");
        serde_json::from_str(&json).unwrap_or_else(|e| panic!("must parse back: {e}\njson: {json}"))
    }

    #[test]
    fn tag_conditions_survive_storage() {
        let node: RuleNode = serde_json::from_str(
            r#"{"kind":"condition","type":"tags","mode":"all","include":["t1","t2"],"exclude":["t9"],"untagged":false}"#,
        )
        .unwrap();

        let back = roundtrip(&node);
        let RuleNode::Condition(Condition::Tags(f)) = back else {
            panic!("wrong variant after round-trip: {back:?}");
        };
        assert_eq!(f.include, vec!["t1", "t2"], "include must survive storage");
        assert_eq!(f.exclude, vec!["t9"], "exclude must survive storage");
    }

    #[test]
    fn colour_conditions_survive_storage() {
        let node = RuleNode::Condition(Condition::Color(crate::assets::ColorFilter {
            r: 1,
            g: 2,
            b: 3,
            tolerance: 12.0,
            min_coverage: 0.25,
        }));
        let back = roundtrip(&node);
        let RuleNode::Condition(Condition::Color(c)) = back else {
            panic!("wrong variant after round-trip: {back:?}");
        };
        assert_eq!((c.r, c.g, c.b), (1, 2, 3));
    }

    /// Every leaf shape at once, nested, as a stored smart folder would be.
    #[test]
    fn a_whole_tree_survives_storage() {
        let node: RuleNode = serde_json::from_str(
            r#"{"kind":"group","op":"all","children":[
                {"kind":"condition","type":"tags","mode":"any","include":["t1"],"exclude":[],"untagged":false},
                {"kind":"condition","type":"media_type","types":["video"]},
                {"kind":"group","op":"any","children":[
                    {"kind":"condition","type":"text","field":"name","op":"contains","value":"hero"}]}]}"#,
        )
        .unwrap();
        let back = roundtrip(&node);
        assert!(back.is_active());
        let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new("");
        back.push_predicate(&mut qb);
        let sql = qb.sql();
        assert!(sql.contains("assets_tags"), "tag predicate lost: {sql}");
        assert!(sql.contains("a.asset_type"), "media predicate lost: {sql}");
    }
}
