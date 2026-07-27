//! Quick Actions: user-defined macros over a selection.
//!
//! An action is a named, ordered pipeline of mutation steps applied to a set of
//! assets snapshotted at trigger time. Three properties define it:
//!
//! * **It is a verb.** A smart folder is a place and a saved filter is a lens;
//!   an action *changes* assets rather than describing them. That's why it lives
//!   in the grid toolbar and never in the sidebar.
//! * **It is atomic.** Every step of a run commits together or none does. The
//!   whole pipeline shares ONE transaction, which is what the `_in` half of each
//!   mutation primitive exists for (see the contract in `assets.rs`).
//! * **It is reversible.** Each step computes its own inverse *while it applies*,
//!   because that is the only moment the information exists. Undo is therefore
//!   not a later feature bolted across the step types — a step that cannot state
//!   its inverse is not finished.
//!
//! ## Why the inverse is a delta, not a snapshot
//!
//! The inverse of "add tag t to 10,000 assets" is "remove t from the assets that
//! did not already have it" — an id list, not a copy of 10,000 rows. The same
//! query yields both deltas: for an ADD the delta is the complement of the
//! existing set, for a REMOVE it is the intersection. Cost scales with what
//! actually changed, which is what lets the log live on disk with a budget
//! instead of in memory with a prayer.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, QueryBuilder, Sqlite, SqlitePool};
use std::collections::HashSet;
use tracing::{instrument, warn};

/// SQLite caps bound parameters at 32766; the same chunk size the rest of the
/// codebase uses for selection-sized id lists.
const IDS_PER_QUERY: usize = 8000;

/// Bumped when the step language changes shape.
///
/// An action written by a NEWER Nova is skipped rather than parsed leniently: a
/// macro that quietly does less than its name says is worse than one that
/// refuses to appear. An OLDER one is upgraded — see `decode_steps`.
///
///   * v1 — a bare array of operations.
///   * v2 — an array of `{op, when}`, so a step can be conditional.
pub const ACTION_VERSION: i64 = 2;

/// How many runs of history to keep. Undo here is a "that was wrong" affordance
/// measured in seconds, not version control, so the log is deliberately shallow.
const MAX_RUNS: i64 = 20;

/// Ceiling on one run's recorded inverse.
///
/// Generous for a tag or folder pipeline at six figures; the binding case is
/// `SetNote`, whose inverse carries the OLD text of every asset. Past the budget
/// the run is marked not-undoable and says so, which beats both silently
/// truncating the inverse (undo would half-work) and refusing to run at all.
pub const UNDO_BUDGET_BYTES: usize = 4 * 1024 * 1024;

// ── The step language ────────────────────────────────────────────────────────

/// One step: an operation, and optionally a condition gating it.
///
/// The condition is what turns a macro into a rules engine. "Add #hero **if**
/// wider than 3000, file into Archive **if** older than 90 days" is one pass
/// over one selection, and it reuses `rules.rs` wholesale — the same tree the
/// smart folder editor writes, compiled by the same compiler.
///
/// Nested rather than flattened onto `Op`. `#[serde(flatten)]` over an
/// internally-tagged enum compiles happily and round-trips lossily, which is the
/// exact trap this file's tests exist to catch; one extra level of JSON is a
/// cheap price for a shape that can't surprise us.
// No `PartialEq`: `RuleNode` carries a colour filter of floats and doesn't
// derive it either. Round-trip tests compare the JSON, which is the thing that
// actually has to stay stable.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Step {
    pub op: Op,
    /// `None` = applies to the whole selection.
    #[serde(default)]
    pub when: Option<crate::rules::RuleNode>,
}

impl Step {
    /// Which of `asset_ids` this step actually applies to.
    ///
    /// Evaluated when the step RUNS, against whatever earlier steps left behind.
    /// That's what "then" means in a pipeline — `add #done` followed by
    /// `if #done, file into Done` will move everything, and should.
    async fn targets(
        &self,
        conn: &mut sqlx::SqliteConnection,
        asset_ids: &[String],
    ) -> Result<Vec<String>> {
        let Some(rules) = &self.when else {
            return Ok(asset_ids.to_vec());
        };
        matching_assets(conn, rules, asset_ids).await
    }

    async fn apply(
        &self,
        conn: &mut sqlx::SqliteConnection,
        asset_ids: &[String],
    ) -> Result<Vec<Inverse>> {
        let targets = self.targets(&mut *conn, asset_ids).await?;
        if targets.is_empty() {
            // Nothing matched, so nothing changed and there is nothing to undo.
            return Ok(Vec::new());
        }
        self.op.apply(conn, &targets).await
    }
}

/// The subset of `asset_ids` matching a rule tree.
///
/// Every condition in `rules.rs` is self-contained against `assets a` — the
/// cross-table ones are `EXISTS`/`IN` subqueries, not joins — so the selection
/// filter needs no more `FROM` than this. Order is preserved from `asset_ids`
/// so a gated rename numbers the same way an ungated one would.
async fn matching_assets(
    conn: &mut sqlx::SqliteConnection,
    rules: &crate::rules::RuleNode,
    asset_ids: &[String],
) -> Result<Vec<String>> {
    let mut matched = HashSet::new();
    for chunk in asset_ids.chunks(IDS_PER_QUERY) {
        let mut qb = QueryBuilder::<Sqlite>::new("SELECT a.id FROM assets a WHERE a.id IN (");
        let mut sep = qb.separated(", ");
        for id in chunk {
            sep.push_bind(id);
        }
        qb.push(") AND (");
        rules.push_predicate(&mut qb);
        qb.push(")");
        let rows: Vec<String> = qb
            .build_query_scalar()
            .fetch_all(&mut *conn)
            .await
            .context("Failed to evaluate a step's condition")?;
        matched.extend(rows);
    }
    Ok(asset_ids
        .iter()
        .filter(|id| matched.contains(*id))
        .cloned()
        .collect())
}

/// One operation in a pipeline.
///
/// This is a FILE FORMAT — every saved action on disk is written in it — so the
/// JSON shape is pinned by tests in `wire_tests` rather than assumed. Same
/// discipline as `rules.rs`, for the same reason: serde's tagged enums compile
/// happily and can still round-trip lossily.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Op {
    /// Apply tags. Additive — an asset that already carries one keeps it, and
    /// contributes nothing to the inverse.
    AddTags { tag_ids: Vec<String> },
    /// Strip tags. Never touches the tag rows themselves.
    RemoveTags { tag_ids: Vec<String> },
    /// Strip every tag. Deliberately its own step rather than "remove all the
    /// tags that exist": this stays correct for tags created after the action
    /// was written, which a materialised list would not.
    ClearAllTags,

    /// Join a folder, keeping every other membership.
    AddToFolder { folder_id: String },
    /// Leave one folder, keeping every other membership.
    RemoveFromFolder { folder_id: String },
    /// Be in exactly these folders and no others.
    ///
    /// This is what "move" would have meant if `assets_folders` were one-to-many.
    /// It isn't — an asset lives in several folders at once — so a step called
    /// "move" would have had to invent an origin the user never specified. An
    /// empty list is meaningful and allowed: it files the assets nowhere, which
    /// is exactly what Uncategorized shows.
    SetFolders { folder_ids: Vec<String> },

    /// Write the note. `mode` decides whether existing text survives — blind
    /// replacement across a selection is the most destructive thing here, so it
    /// is a choice the action has to state rather than a default it inherits.
    SetNote { mode: TextMode, text: String },
    /// Write the source URL. No modes: a URL is not a thing you append to.
    SetSourceUrl { url: String },

    /// Move to the Trash.
    ///
    /// Safe to offer as a step precisely because it's reversible — the asset
    /// keeps its file, its folders and its tags, and Restore is one UPDATE.
    /// Permanent deletion is deliberately NOT a step: it has no inverse, so it
    /// must never sit inside a pipeline that claims to be undoable.
    MoveToTrash,
    /// Bring assets back from the Trash. Not offered in the step picker — it
    /// exists so the Trash view's Restore runs through the same pipeline (and
    /// gets the same undo) as everything else.
    RestoreFromTrash,

    /// Rename from a pattern.
    ///
    /// In Nova a filename is METADATA — files on disk are `assets/{uuid}.{ext}` —
    /// so this touches no bytes and belongs in the same transaction as the rest.
    ///
    /// Everything `{index}` and `{date}` depend on is stored HERE rather than
    /// inherited from the view. An action has to do the same thing twice; if the
    /// numbering followed whatever sort happened to be on screen, running it
    /// again after changing the sort would renumber the same assets differently.
    RenameWithPattern {
        pattern: String,
        /// What `{index}` counts in.
        #[serde(default)]
        index_order: RenameOrder,
        #[serde(default = "yes")]
        index_ascending: bool,
        #[serde(default = "one")]
        index_start: i64,
        /// Zero-padding width, so 1..1000 sorts as text the way it reads.
        #[serde(default = "three")]
        index_pad: u8,
        /// Which date `{date}` reads.
        #[serde(default)]
        date_field: crate::assets::DateField,
    },
}

fn yes() -> bool {
    true
}
fn one() -> i64 {
    1
}
fn three() -> u8 {
    3
}

/// Orders a rename can number in.
///
/// Its own enum rather than the view's `OrderBy`: this sorts a SELECTION with no
/// folder or scope behind it, so the entries that need one (`manual`, "date
/// added to this folder") have no meaning here and shouldn't be offerable.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RenameOrder {
    Filename,
    #[default]
    ImportedDate,
    CreationDate,
    ModifiedDate,
    FileSize,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TextMode {
    Replace,
    Append,
    Prepend,
}

/// Which text column an inverse restores. Applying differs per field (a note has
/// modes, a URL doesn't); REVERTING is the same operation either way — write
/// these exact values back — so the inverse is one variant carrying the column.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TextTarget {
    Notes,
    SourceUrl,
    /// Only ever written by a rename. `filename` is NOT NULL, so a captured
    /// value is always `Some` — the `Option` here is the shape the other two
    /// columns need, not a state this one can reach.
    Filename,
}

impl TextTarget {
    fn column(self) -> &'static str {
        match self {
            TextTarget::Notes => "notes",
            TextTarget::SourceUrl => "source_url",
            TextTarget::Filename => "filename",
        }
    }
}

// ── The rename pattern language ──────────────────────────────────────────────

/// One piece of a parsed pattern.
///
/// Note what is NOT here: the extension. It is derived from the file's real
/// bytes and appended after rendering, so a pattern is structurally unable to
/// change it — which is the difference between renaming an asset and lying about
/// what it is.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Literal(String),
    /// The current name without its extension, so a pattern can wrap rather
    /// than replace.
    Name,
    Index,
    Date,
    Width,
    Height,
}

/// Characters a filename may not contain on Windows.
///
/// Enforced even though `assets.filename` is metadata, because outbound drag
/// hardlinks each asset under this name inside `.drag-staging` — a name with a
/// colon in it would import fine and then fail to drag out. Rejected in the
/// PATTERN, at edit time, rather than sanitised at render time: silently
/// rewriting what the user typed across 10,000 assets is worse than not
/// accepting it.
const ILLEGAL_IN_FILENAME: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

/// Parse a pattern into tokens, or explain what's wrong with it.
///
/// Errors are written for the person typing: they name the offending token or
/// character rather than the position, because the pattern box is one line and
/// the mistake is nearly always visible in it.
fn parse_pattern(pattern: &str) -> Result<Vec<Token>> {
    if pattern.trim().is_empty() {
        bail!("The pattern is empty");
    }

    let mut tokens = Vec::new();
    let mut literal = String::new();
    let mut chars = pattern.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            // `{{` and `}}` escape, so a literal brace stays expressible.
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                literal.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                literal.push('}');
            }
            '{' => {
                if !literal.is_empty() {
                    tokens.push(Token::Literal(std::mem::take(&mut literal)));
                }
                let mut word = String::new();
                let mut closed = false;
                for c in chars.by_ref() {
                    if c == '}' {
                        closed = true;
                        break;
                    }
                    word.push(c);
                }
                if !closed {
                    bail!("Unclosed {{ — every token needs a closing brace");
                }
                tokens.push(match word.trim().to_ascii_lowercase().as_str() {
                    "name" => Token::Name,
                    "index" => Token::Index,
                    "date" => Token::Date,
                    "width" => Token::Width,
                    "height" => Token::Height,
                    other => bail!(
                        "Unknown token {{{other}}} — use name, index, date, width or height"
                    ),
                });
            }
            '}' => bail!("Stray }} — write }}}} for a literal brace"),
            c if c.is_control() => bail!("The pattern contains a control character"),
            c if ILLEGAL_IN_FILENAME.contains(&c) => {
                bail!("A filename can't contain {c}")
            }
            c => literal.push(c),
        }
    }
    if !literal.is_empty() {
        tokens.push(Token::Literal(literal));
    }
    Ok(tokens)
}

/// "photo.jpg" + "jpg" -> "photo". Mirrors `filenameStem` on the frontend.
fn stem_of(filename: &str, extension: &str) -> String {
    if extension.is_empty() {
        return filename.to_string();
    }
    let suffix = format!(".{extension}");
    if filename.to_lowercase().ends_with(&suffix.to_lowercase()) {
        filename[..filename.len() - suffix.len()].to_string()
    } else {
        filename.to_string()
    }
}

/// Everything a pattern can read about one asset.
#[derive(FromRow, Debug, Clone)]
struct RenameRow {
    id: String,
    filename: String,
    extension: String,
    width: i64,
    height: i64,
    file_size: i64,
    imported_date: String,
    creation_date: String,
    modified_date: String,
}

impl RenameRow {
    fn date(&self, field: crate::assets::DateField) -> &str {
        let full = match field {
            crate::assets::DateField::ImportedDate => &self.imported_date,
            crate::assets::DateField::CreationDate => &self.creation_date,
            crate::assets::DateField::ModifiedDate => &self.modified_date,
        };
        // Stamps are RFC 3339; the first ten characters are the calendar day,
        // which is the only part that belongs in a filename.
        full.get(..10).unwrap_or(full)
    }

    /// The SQL column matching `sort_key`, so a chunk can be ordered in the
    /// database. The two MUST agree — see `rename_rows`.
    fn order_column(order: RenameOrder) -> &'static str {
        match order {
            RenameOrder::Filename => "filename COLLATE NOCASE",
            RenameOrder::ImportedDate => "imported_date",
            RenameOrder::CreationDate => "creation_date",
            RenameOrder::ModifiedDate => "modified_date",
            RenameOrder::FileSize => "file_size",
        }
    }

    /// The sort key, as a string so one comparison serves every order. Numbers
    /// are zero-padded so they compare by magnitude rather than lexically.
    fn sort_key(&self, order: RenameOrder) -> String {
        match order {
            RenameOrder::Filename => self.filename.to_lowercase(),
            RenameOrder::ImportedDate => self.imported_date.clone(),
            RenameOrder::CreationDate => self.creation_date.clone(),
            RenameOrder::ModifiedDate => self.modified_date.clone(),
            RenameOrder::FileSize => format!("{:020}", self.file_size),
        }
    }
}

/// Render one asset's new stem. Never returns the extension.
fn render(
    tokens: &[Token],
    row: &RenameRow,
    index: i64,
    pad: u8,
    date_field: crate::assets::DateField,
) -> String {
    let mut out = String::new();
    for token in tokens {
        match token {
            Token::Literal(s) => out.push_str(s),
            Token::Name => out.push_str(&stem_of(&row.filename, &row.extension)),
            Token::Index => out.push_str(&format!("{:0width$}", index, width = pad as usize)),
            Token::Date => out.push_str(row.date(date_field)),
            Token::Width => out.push_str(&row.width.to_string()),
            Token::Height => out.push_str(&row.height.to_string()),
        }
    }
    out
}

/// The selection's rows, in the order `{index}` counts.
///
/// The final sort happens in Rust because the ids are fetched in chunks, and a
/// per-chunk `ORDER BY` alone would number each chunk from the start of its own
/// range — a bug that only appears past 8,000 assets.
///
/// `limit` is for the PREVIEW, which only ever shows a handful of rows. Each
/// chunk is ordered and limited in SQL first, then the same Rust sort merges
/// them: the global first *n* is necessarily among the per-chunk first *n*s, so
/// this is exact, and it turns "read 10,000 rows on every keystroke" into
/// "read 3 per chunk". The run passes `None` and is unchanged.
async fn rename_rows(
    conn: &mut sqlx::SqliteConnection,
    asset_ids: &[String],
    order: RenameOrder,
    ascending: bool,
    limit: Option<usize>,
) -> Result<Vec<RenameRow>> {
    const COLS: &str = "SELECT id, filename, extension, width, height, file_size, \
                        imported_date, creation_date, modified_date FROM assets WHERE id IN (";
    let mut out: Vec<RenameRow> = Vec::with_capacity(limit.unwrap_or(asset_ids.len()));
    for chunk in asset_ids.chunks(IDS_PER_QUERY) {
        let mut qb = QueryBuilder::<Sqlite>::new(COLS);
        let mut sep = qb.separated(", ");
        for id in chunk {
            sep.push_bind(id);
        }
        qb.push(")");
        if let Some(n) = limit {
            // Must match `sort_key`, or the rows kept here wouldn't be the ones
            // the merge below would have chosen.
            qb.push(" ORDER BY ")
                .push(RenameRow::order_column(order))
                .push(if ascending { " ASC" } else { " DESC" })
                .push(", id ASC LIMIT ")
                .push_bind(n as i64);
        }
        out.extend(
            qb.build_query_as::<RenameRow>()
                .fetch_all(&mut *conn)
                .await
                .context("Failed to read the assets to rename")?,
        );
    }

    // `id` breaks ties, so two assets with the same timestamp always number in
    // the same order — a rerun must reproduce its own numbering exactly.
    out.sort_by(|a, b| {
        let ord = a.sort_key(order).cmp(&b.sort_key(order));
        let ord = if ascending { ord } else { ord.reverse() };
        ord.then_with(|| a.id.cmp(&b.id))
    });
    if let Some(n) = limit {
        out.truncate(n);
    }
    Ok(out)
}

/// The new filename for every asset, in order. Shared by apply and both previews
/// so what you see is what runs.
fn render_all(
    tokens: &[Token],
    rows: &[RenameRow],
    start: i64,
    pad: u8,
    date_field: crate::assets::DateField,
) -> Result<Vec<(String, String)>> {
    rows.iter()
        .enumerate()
        .map(|(i, row)| {
            let stem = render(tokens, row, start + i as i64, pad, date_field);
            let stem = crate::assets::clean_name(&stem)
                .context("This pattern produces an empty name for some assets")?;
            let filename = if row.extension.is_empty() {
                stem
            } else {
                format!("{stem}.{}", row.extension)
            };
            Ok((row.id.clone(), filename))
        })
        .collect()
}

/// One row of `assets_folders`, captured whole.
///
/// `position` and `added_at` ride along because membership is not a boolean: a
/// folder sorted manually has an arrangement the user made by hand, and one
/// sorted by "date added" reads that column. An inverse that re-added assets
/// without them would restore the fact of membership and destroy its meaning.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, FromRow)]
pub struct FolderMember {
    pub folder_id: String,
    pub asset_id: String,
    pub position: f64,
    pub added_at: Option<String>,
}

/// The recorded reverse of applied work.
///
/// Kept separate from `Step` even though the variants mirror each other, because
/// they answer different questions: a step says *what the user asked for* over a
/// selection, an inverse says *what actually changed* and for exactly which
/// assets. `AddTags{[t]}` over 10,000 assets where 9,000 already had `t`
/// produces an inverse naming 1,000.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Inverse {
    AddTag {
        tag_id: String,
        asset_ids: Vec<String>,
    },
    RemoveTag {
        tag_id: String,
        asset_ids: Vec<String>,
    },
    /// Undo of a JOIN: the assets that gained membership had no row before, so
    /// reverting is a plain delete with nothing to restore.
    RemoveFolderMembers {
        folder_id: String,
        asset_ids: Vec<String>,
    },
    /// Undo of a LEAVE: put the captured rows back exactly as they were.
    AddFolderMembers { members: Vec<FolderMember> },
    /// Undo of a REPLACE. Destructive by necessity — the step overwrote the whole
    /// membership set, so reverting has to clear whatever is there now before
    /// restoring, or memberships the step removed would come back *alongside*
    /// the ones it added.
    ReplaceFolderMembers {
        asset_ids: Vec<String>,
        members: Vec<FolderMember>,
    },
    /// Undo of a trash or restore, naming only the assets whose state actually
    /// changed — so undoing a trash can't drag back something that was already
    /// in the Trash before the run.
    SetTrashed {
        asset_ids: Vec<String>,
        trashed: bool,
    },
    /// Undo of a text write. Values are per asset and `None` means the column was
    /// empty — which must be restored as NULL, not as the string "None".
    RestoreText {
        field: TextTarget,
        values: Vec<(String, Option<String>)>,
    },
}

impl Op {
    /// Apply this step inside a caller-owned transaction, returning its inverse.
    ///
    /// Computing the inverse HERE, before the write, is the whole design: after
    /// the `INSERT OR IGNORE` there is no way left to tell which assets actually
    /// gained the tag and which already had it.
    async fn apply(
        &self,
        conn: &mut sqlx::SqliteConnection,
        asset_ids: &[String],
    ) -> Result<Vec<Inverse>> {
        let mut inverses = Vec::new();
        match self {
            Op::AddTags { tag_ids } => {
                for tag_id in tag_ids {
                    let already = assets_with_tag(&mut *conn, tag_id, asset_ids).await?;
                    let gained: Vec<String> = asset_ids
                        .iter()
                        .filter(|id| !already.contains(*id))
                        .cloned()
                        .collect();
                    crate::tags::assign_tag_in(&mut *conn, tag_id, asset_ids).await?;
                    if !gained.is_empty() {
                        inverses.push(Inverse::RemoveTag {
                            tag_id: tag_id.clone(),
                            asset_ids: gained,
                        });
                    }
                }
            }
            Op::RemoveTags { tag_ids } => {
                for tag_id in tag_ids {
                    let had = assets_with_tag(&mut *conn, tag_id, asset_ids).await?;
                    crate::tags::unassign_tag_in(&mut *conn, tag_id, asset_ids).await?;
                    if !had.is_empty() {
                        inverses.push(Inverse::AddTag {
                            tag_id: tag_id.clone(),
                            asset_ids: had.into_iter().collect(),
                        });
                    }
                }
            }

            Op::ClearAllTags => {
                // Grouped BY TAG, which lets the inverse reuse `AddTag` instead
                // of inventing a per-asset restore format: "give t1 back to
                // these 40 assets" says the same thing as forty one-asset rows,
                // in a fraction of the bytes.
                for (tag_id, ids) in tags_of_assets(&mut *conn, asset_ids).await? {
                    crate::tags::unassign_tag_in(&mut *conn, &tag_id, &ids).await?;
                    inverses.push(Inverse::AddTag {
                        tag_id,
                        asset_ids: ids,
                    });
                }
            }

            Op::AddToFolder { folder_id } => {
                let already = folder_members(&mut *conn, std::slice::from_ref(folder_id), asset_ids).await?;
                let present: HashSet<&String> = already.iter().map(|m| &m.asset_id).collect();
                let gained: Vec<String> = asset_ids
                    .iter()
                    .filter(|id| !present.contains(*id))
                    .cloned()
                    .collect();

                crate::assets::add_assets_to_folder_in(&mut *conn, folder_id, asset_ids).await?;
                if !gained.is_empty() {
                    inverses.push(Inverse::RemoveFolderMembers {
                        folder_id: folder_id.clone(),
                        asset_ids: gained,
                    });
                }
                // Seeded on ARRIVAL, for everything in the operation rather than
                // only what gained membership: "I dragged these here, they
                // should pick up the folder's tags" holds whether or not one of
                // them happened to already be filed here.
                inverses.extend(seed_auto_tags(&mut *conn, folder_id, asset_ids).await?);
            }

            Op::RemoveFromFolder { folder_id } => {
                let lost = folder_members(&mut *conn, std::slice::from_ref(folder_id), asset_ids).await?;
                crate::assets::remove_assets_from_folder_in(&mut *conn, folder_id, asset_ids)
                    .await?;
                if !lost.is_empty() {
                    inverses.push(Inverse::AddFolderMembers { members: lost });
                }
            }

            Op::SetFolders { folder_ids } => {
                // Capture EVERY membership first: the step is about to drop all
                // of them, and once dropped there is no record of where these
                // assets used to live.
                let before = all_folder_members(&mut *conn, asset_ids).await?;
                clear_folder_members(&mut *conn, asset_ids).await?;
                for folder_id in folder_ids {
                    crate::assets::add_assets_to_folder_in(&mut *conn, folder_id, asset_ids)
                        .await?;
                    inverses.extend(seed_auto_tags(&mut *conn, folder_id, asset_ids).await?);
                }
                inverses.push(Inverse::ReplaceFolderMembers {
                    asset_ids: asset_ids.to_vec(),
                    members: before,
                });
            }

            Op::SetNote { mode, text } => {
                if let Some(inverse) =
                    write_text(&mut *conn, TextTarget::Notes, *mode, text, asset_ids).await?
                {
                    inverses.push(inverse);
                }
            }

            Op::MoveToTrash => {
                if let Some(inverse) = set_trashed(&mut *conn, asset_ids, true).await? {
                    inverses.push(inverse);
                }
            }
            Op::RestoreFromTrash => {
                if let Some(inverse) = set_trashed(&mut *conn, asset_ids, false).await? {
                    inverses.push(inverse);
                }
            }

            Op::SetSourceUrl { url } => {
                if let Some(inverse) = write_text(
                    &mut *conn,
                    TextTarget::SourceUrl,
                    TextMode::Replace,
                    url,
                    asset_ids,
                )
                .await?
                {
                    inverses.push(inverse);
                }
            }

            Op::RenameWithPattern {
                pattern,
                index_order,
                index_ascending,
                index_start,
                index_pad,
                date_field,
            } => {
                let tokens = parse_pattern(pattern)?;
                let rows =
                    rename_rows(&mut *conn, asset_ids, *index_order, *index_ascending, None).await?;
                let renamed =
                    render_all(&tokens, &rows, *index_start, *index_pad, *date_field)?;

                // Capture from the rows already in hand rather than re-reading:
                // they were fetched inside this transaction, so they are the
                // values the rename is about to overwrite.
                let before: Vec<(String, Option<String>)> = rows
                    .iter()
                    .map(|r| (r.id.clone(), Some(r.filename.clone())))
                    .collect();
                let after: Vec<(String, Option<String>)> = renamed
                    .into_iter()
                    .map(|(id, name)| (id, Some(name)))
                    .collect();

                write_text_values(&mut *conn, TextTarget::Filename, &after).await?;
                if !before.is_empty() {
                    inverses.push(Inverse::RestoreText {
                        field: TextTarget::Filename,
                        values: before,
                    });
                }
            }
        }
        Ok(inverses)
    }

    /// Tag ids this step depends on, for the pre-run existence check.
    fn tag_refs(&self) -> &[String] {
        match self {
            Op::AddTags { tag_ids } | Op::RemoveTags { tag_ids } => tag_ids,
            _ => &[],
        }
    }

    /// Folder ids this step depends on. Checked for the same reason as tags: a
    /// step pointing at a deleted folder would silently do nothing.
    fn folder_refs(&self) -> &[String] {
        match self {
            Op::AddToFolder { folder_id } | Op::RemoveFromFolder { folder_id } => {
                std::slice::from_ref(folder_id)
            }
            Op::SetFolders { folder_ids } => folder_ids,
            _ => &[],
        }
    }

    /// Does this step actually do anything, or is it a half-built editor row?
    ///
    /// Only the tag steps can be blank. `SetFolders{[]}` files assets nowhere and
    /// `SetNote{Replace,""}` clears the note — both are real instructions, so
    /// treating "empty" as "unfinished" would make them unexpressible.
    fn is_active(&self) -> bool {
        match self {
            Op::AddTags { tag_ids } | Op::RemoveTags { tag_ids } => !tag_ids.is_empty(),
            Op::SetNote { mode, text } => {
                // Appending nothing is the one genuinely empty case.
                *mode == TextMode::Replace || !text.trim().is_empty()
            }
            Op::RenameWithPattern { pattern, .. } => !pattern.trim().is_empty(),
            _ => true,
        }
    }

    /// Roughly how many undo bytes this step costs PER ASSET.
    ///
    /// Every figure errs high on purpose: warning about a run that turns out to
    /// be undoable is a smaller failure than promising undo and not delivering.
    /// A uuid costs 36 characters plus JSON quoting and separators.
    fn undo_bytes_per_asset(&self) -> usize {
        match self {
            Op::AddTags { tag_ids } | Op::RemoveTags { tag_ids } => 48 * tag_ids.len(),
            // An asset carries a handful of tags; three is a generous typical.
            Op::ClearAllTags => 48 * 3,
            // A member record is two ids plus a position and a timestamp.
            Op::AddToFolder { .. } | Op::RemoveFromFolder { .. } => 128,
            // Same, but for every folder an asset was in rather than one.
            Op::SetFolders { .. } => 128 * 3,
            // Free text, and the OLD text at that — the one figure here that is
            // genuinely unbounded, so it gets the most headroom.
            Op::SetNote { .. } => 512,
            Op::SetSourceUrl { .. } => 160,
            // The old filename, which is bounded in a way a note isn't.
            Op::RenameWithPattern { .. } => 160,
            // An id list and one boolean for the whole step.
            Op::MoveToTrash | Op::RestoreFromTrash => 48,
        }
    }
}

impl Inverse {
    async fn apply(&self, conn: &mut sqlx::SqliteConnection) -> Result<()> {
        match self {
            Inverse::AddTag { tag_id, asset_ids } => {
                crate::tags::assign_tag_in(conn, tag_id, asset_ids).await
            }
            Inverse::RemoveTag { tag_id, asset_ids } => {
                crate::tags::unassign_tag_in(conn, tag_id, asset_ids).await
            }
            Inverse::RemoveFolderMembers {
                folder_id,
                asset_ids,
            } => crate::assets::remove_assets_from_folder_in(conn, folder_id, asset_ids).await,
            Inverse::AddFolderMembers { members } => insert_folder_members(conn, members).await,
            Inverse::ReplaceFolderMembers { asset_ids, members } => {
                clear_folder_members(&mut *conn, asset_ids).await?;
                insert_folder_members(conn, members).await
            }
            Inverse::SetTrashed { asset_ids, trashed } => {
                set_trashed(conn, asset_ids, *trashed).await.map(|_| ())
            }
            Inverse::RestoreText { field, values } => {
                write_text_values(conn, *field, values).await
            }
        }
    }

    /// Every asset this inverse touches, so undo knows what to reindex and which
    /// entries to drop for assets that no longer exist.
    fn asset_ids(&self) -> Vec<String> {
        match self {
            Inverse::AddTag { asset_ids, .. }
            | Inverse::RemoveTag { asset_ids, .. }
            | Inverse::SetTrashed { asset_ids, .. }
            | Inverse::RemoveFolderMembers { asset_ids, .. } => asset_ids.clone(),
            // The scope is what was cleared, which is a superset of what the
            // captured members mention — an asset that was in no folder still
            // has to be excluded from the restore.
            Inverse::ReplaceFolderMembers { asset_ids, .. } => asset_ids.clone(),
            Inverse::AddFolderMembers { members } => {
                members.iter().map(|m| m.asset_id.clone()).collect()
            }
            Inverse::RestoreText { values, .. } => {
                values.iter().map(|(id, _)| id.clone()).collect()
            }
        }
    }

    /// Undo runs against a library that has moved on. Restoring a tag to an
    /// asset that no longer exists would trip the foreign key and abort the
    /// whole undo, so a missing asset is dropped from the inverse instead —
    /// the caller reports the shortfall rather than failing.
    fn retaining(&self, alive: &HashSet<String>) -> Self {
        let keep = |ids: &[String]| -> Vec<String> {
            ids.iter().filter(|id| alive.contains(*id)).cloned().collect()
        };
        let keep_members = |ms: &[FolderMember]| -> Vec<FolderMember> {
            ms.iter()
                .filter(|m| alive.contains(&m.asset_id))
                .cloned()
                .collect()
        };
        match self {
            Inverse::AddTag { tag_id, asset_ids } => Inverse::AddTag {
                tag_id: tag_id.clone(),
                asset_ids: keep(asset_ids),
            },
            Inverse::RemoveTag { tag_id, asset_ids } => Inverse::RemoveTag {
                tag_id: tag_id.clone(),
                asset_ids: keep(asset_ids),
            },
            Inverse::RemoveFolderMembers {
                folder_id,
                asset_ids,
            } => Inverse::RemoveFolderMembers {
                folder_id: folder_id.clone(),
                asset_ids: keep(asset_ids),
            },
            Inverse::AddFolderMembers { members } => Inverse::AddFolderMembers {
                members: keep_members(members),
            },
            Inverse::ReplaceFolderMembers { asset_ids, members } => {
                Inverse::ReplaceFolderMembers {
                    asset_ids: keep(asset_ids),
                    members: keep_members(members),
                }
            }
            Inverse::SetTrashed { asset_ids, trashed } => Inverse::SetTrashed {
                asset_ids: keep(asset_ids),
                trashed: *trashed,
            },
            Inverse::RestoreText { field, values } => Inverse::RestoreText {
                field: *field,
                values: values
                    .iter()
                    .filter(|(id, _)| alive.contains(id))
                    .cloned()
                    .collect(),
            },
        }
    }
}

/// Which of `asset_ids` already carry `tag_id`.
///
/// One query serves both directions: an ADD wants the complement of this set,
/// a REMOVE wants the set itself.
async fn assets_with_tag(
    conn: &mut sqlx::SqliteConnection,
    tag_id: &str,
    asset_ids: &[String],
) -> Result<HashSet<String>> {
    let mut out = HashSet::new();
    for chunk in asset_ids.chunks(IDS_PER_QUERY) {
        let mut qb =
            QueryBuilder::<Sqlite>::new("SELECT asset_id FROM assets_tags WHERE tag_id = ");
        qb.push_bind(tag_id).push(" AND asset_id IN (");
        let mut sep = qb.separated(", ");
        for id in chunk {
            sep.push_bind(id);
        }
        qb.push(")");
        let rows: Vec<String> = qb
            .build_query_scalar()
            .fetch_all(&mut *conn)
            .await
            .context("Failed to read existing tag assignments")?;
        out.extend(rows);
    }
    Ok(out)
}

/// Every tag on these assets, grouped by tag.
///
/// Grouping here rather than at the call site is what lets `ClearAllTags` reuse
/// the `AddTag` inverse: the natural read is one row per (asset, tag) pair, and
/// pivoting it into per-tag id lists collapses a 30,000-row inverse into a
/// handful of entries.
async fn tags_of_assets(
    conn: &mut sqlx::SqliteConnection,
    asset_ids: &[String],
) -> Result<Vec<(String, Vec<String>)>> {
    let mut by_tag: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for chunk in asset_ids.chunks(IDS_PER_QUERY) {
        let mut qb =
            QueryBuilder::<Sqlite>::new("SELECT tag_id, asset_id FROM assets_tags WHERE asset_id IN (");
        let mut sep = qb.separated(", ");
        for id in chunk {
            sep.push_bind(id);
        }
        qb.push(")");
        let rows: Vec<(String, String)> = qb
            .build_query_as()
            .fetch_all(&mut *conn)
            .await
            .context("Failed to read the tags on these assets")?;
        for (tag_id, asset_id) in rows {
            by_tag.entry(tag_id).or_default().push(asset_id);
        }
    }
    Ok(by_tag.into_iter().collect())
}

/// Membership rows for these assets, restricted to `folder_ids` when non-empty.
async fn folder_members(
    conn: &mut sqlx::SqliteConnection,
    folder_ids: &[String],
    asset_ids: &[String],
) -> Result<Vec<FolderMember>> {
    let mut out = Vec::new();
    for chunk in asset_ids.chunks(IDS_PER_QUERY) {
        let mut qb = QueryBuilder::<Sqlite>::new(
            "SELECT folder_id, asset_id, position, added_at FROM assets_folders WHERE asset_id IN (",
        );
        let mut sep = qb.separated(", ");
        for id in chunk {
            sep.push_bind(id);
        }
        qb.push(")");
        if !folder_ids.is_empty() {
            qb.push(" AND folder_id IN (");
            let mut sep = qb.separated(", ");
            for id in folder_ids {
                sep.push_bind(id);
            }
            qb.push(")");
        }
        let rows: Vec<FolderMember> = qb
            .build_query_as()
            .fetch_all(&mut *conn)
            .await
            .context("Failed to read folder membership")?;
        out.extend(rows);
    }
    Ok(out)
}

/// Every membership these assets have, across all folders.
async fn all_folder_members(
    conn: &mut sqlx::SqliteConnection,
    asset_ids: &[String],
) -> Result<Vec<FolderMember>> {
    folder_members(conn, &[], asset_ids).await
}

/// Drop every folder membership for these assets.
async fn clear_folder_members(
    conn: &mut sqlx::SqliteConnection,
    asset_ids: &[String],
) -> Result<()> {
    for chunk in asset_ids.chunks(IDS_PER_QUERY) {
        let mut qb = QueryBuilder::<Sqlite>::new("DELETE FROM assets_folders WHERE asset_id IN (");
        let mut sep = qb.separated(", ");
        for id in chunk {
            sep.push_bind(id);
        }
        qb.push(")");
        qb.build()
            .execute(&mut *conn)
            .await
            .context("Failed to clear folder membership")?;
    }
    Ok(())
}

/// Put captured membership rows back, `position` and `added_at` included.
///
/// `INSERT OR IGNORE` rather than `REPLACE`: if something already put the asset
/// back in that folder, its current position is more recent than the captured
/// one and overwriting it would undo an edit this run never made.
async fn insert_folder_members(
    conn: &mut sqlx::SqliteConnection,
    members: &[FolderMember],
) -> Result<()> {
    // Four binds per row, so a smaller chunk than the id lists use.
    for chunk in members.chunks(IDS_PER_QUERY / 4) {
        let mut qb = QueryBuilder::<Sqlite>::new(
            "INSERT OR IGNORE INTO assets_folders (folder_id, asset_id, position, added_at) ",
        );
        qb.push_values(chunk, |mut b, m| {
            b.push_bind(&m.folder_id)
                .push_bind(&m.asset_id)
                .push_bind(m.position)
                .push_bind(&m.added_at);
        });
        qb.build()
            .execute(&mut *conn)
            .await
            .context("Failed to restore folder membership")?;
    }
    Ok(())
}

/// Write a text column across a selection, returning the inverse.
///
/// `None` when there is nothing to do, so an "append nothing" step doesn't
/// record an empty undo entry that would read as a change in the history.
async fn write_text(
    conn: &mut sqlx::SqliteConnection,
    field: TextTarget,
    mode: TextMode,
    text: &str,
    asset_ids: &[String],
) -> Result<Option<Inverse>> {
    if mode != TextMode::Replace && text.trim().is_empty() {
        return Ok(None);
    }

    // Capture BEFORE the write. This is the expensive inverse in the language —
    // it's the old text itself, per asset, and nothing smaller reconstructs it.
    let values = capture_text(&mut *conn, field, asset_ids).await?;
    let col = field.column();
    // Blank normalises to NULL, matching `blank_to_null` everywhere else, so an
    // emptied note is indistinguishable from one that was never written.
    let value: Option<&str> = if text.is_empty() { None } else { Some(text) };

    for chunk in asset_ids.chunks(IDS_PER_QUERY) {
        let mut qb = QueryBuilder::<Sqlite>::new("UPDATE assets SET ");
        qb.push(col).push(" = ");
        match mode {
            TextMode::Replace => {
                qb.push_bind(value);
            }
            // A separator only where there is something to separate — appending
            // to an empty note must not leave it starting with a blank line.
            TextMode::Append => {
                qb.push("CASE WHEN ").push(col).push(" IS NULL OR ").push(col);
                qb.push(" = '' THEN ").push_bind(text);
                qb.push(" ELSE ").push(col).push(" || ").push_bind(format!("\n{text}"));
                qb.push(" END");
            }
            TextMode::Prepend => {
                qb.push("CASE WHEN ").push(col).push(" IS NULL OR ").push(col);
                qb.push(" = '' THEN ").push_bind(text);
                qb.push(" ELSE ").push_bind(format!("{text}\n")).push(" || ").push(col);
                qb.push(" END");
            }
        }
        qb.push(" WHERE id IN (");
        let mut sep = qb.separated(", ");
        for id in chunk {
            sep.push_bind(id);
        }
        qb.push(")");
        qb.build()
            .execute(&mut *conn)
            .await
            .with_context(|| format!("Failed to write {col}"))?;
    }

    Ok(Some(Inverse::RestoreText { field, values }))
}

async fn capture_text(
    conn: &mut sqlx::SqliteConnection,
    field: TextTarget,
    asset_ids: &[String],
) -> Result<Vec<(String, Option<String>)>> {
    let mut out = Vec::with_capacity(asset_ids.len());
    for chunk in asset_ids.chunks(IDS_PER_QUERY) {
        let mut qb = QueryBuilder::<Sqlite>::new("SELECT id, ");
        qb.push(field.column()).push(" FROM assets WHERE id IN (");
        let mut sep = qb.separated(", ");
        for id in chunk {
            sep.push_bind(id);
        }
        qb.push(")");
        let rows: Vec<(String, Option<String>)> = qb
            .build_query_as()
            .fetch_all(&mut *conn)
            .await
            .context("Failed to read the current text")?;
        out.extend(rows);
    }
    Ok(out)
}

/// Write per-asset text back.
///
/// One statement per asset, deliberately. Every value differs, so the only
/// set-based alternative is a `CASE id WHEN … END` with thousands of branches —
/// which risks SQLite's expression limits to save milliseconds inside a
/// transaction that is already committed as a unit.
async fn write_text_values(
    conn: &mut sqlx::SqliteConnection,
    field: TextTarget,
    values: &[(String, Option<String>)],
) -> Result<()> {
    let sql = format!("UPDATE assets SET {} = ? WHERE id = ?", field.column());
    for (asset_id, value) in values {
        sqlx::query(&sql)
            .bind(value)
            .bind(asset_id)
            .execute(&mut *conn)
            .await
            .context("Failed to restore text")?;
    }
    Ok(())
}

/// Apply a folder's auto-tags to assets that just arrived in it, returning the
/// inverse of what actually changed.
///
/// Called from the steps that CREATE membership, and deliberately not from
/// `insert_folder_members` — that path restores membership during an undo, and
/// seeding there would re-add the very tags the undo is removing. The two paths
/// being separate functions is what makes this safe by construction rather than
/// by remembering.
///
/// The delta is computed exactly as `AddTags` computes it, so an asset that
/// already carried the tag contributes nothing and undo will not take it away.
async fn seed_auto_tags(
    conn: &mut sqlx::SqliteConnection,
    folder_id: &str,
    asset_ids: &[String],
) -> Result<Vec<Inverse>> {
    let mut inverses = Vec::new();
    for tag_id in crate::assets::auto_tags_of(&mut *conn, folder_id).await? {
        let already = assets_with_tag(&mut *conn, &tag_id, asset_ids).await?;
        let gained: Vec<String> = asset_ids
            .iter()
            .filter(|id| !already.contains(*id))
            .cloned()
            .collect();
        crate::tags::assign_tag_in(&mut *conn, &tag_id, asset_ids).await?;
        if !gained.is_empty() {
            inverses.push(Inverse::RemoveTag {
                tag_id,
                asset_ids: gained,
            });
        }
    }
    Ok(inverses)
}

/// Move assets into or out of the Trash, returning the inverse.
///
/// Only assets whose state actually CHANGES are written and recorded, computed
/// the same way every other delta here is. Undoing a trash therefore can't drag
/// back something that was already in the Trash when the run started.
async fn set_trashed(
    conn: &mut sqlx::SqliteConnection,
    asset_ids: &[String],
    trashed: bool,
) -> Result<Option<Inverse>> {
    // The assets currently on the other side of the line — the ones this will
    // move. Read before the write, because afterwards there is no way to tell.
    let mut changing: Vec<String> = Vec::new();
    for chunk in asset_ids.chunks(IDS_PER_QUERY) {
        let mut qb = QueryBuilder::<Sqlite>::new("SELECT id FROM assets WHERE deleted_at IS ");
        qb.push(if trashed { "NULL" } else { "NOT NULL" });
        qb.push(" AND id IN (");
        let mut sep = qb.separated(", ");
        for id in chunk {
            sep.push_bind(id);
        }
        qb.push(")");
        changing.extend(
            qb.build_query_scalar::<String>()
                .fetch_all(&mut *conn)
                .await
                .context("Failed to read which assets would move")?,
        );
    }
    if changing.is_empty() {
        return Ok(None);
    }

    let stamp = trashed.then(crate::assets::now_stamp);
    for chunk in changing.chunks(IDS_PER_QUERY) {
        let mut qb = QueryBuilder::<Sqlite>::new("UPDATE assets SET deleted_at = ");
        qb.push_bind(stamp.clone());
        qb.push(" WHERE id IN (");
        let mut sep = qb.separated(", ");
        for id in chunk {
            sep.push_bind(id);
        }
        qb.push(")");
        qb.build()
            .execute(&mut *conn)
            .await
            .context("Failed to move assets to or from the Trash")?;
    }

    Ok(Some(Inverse::SetTrashed {
        asset_ids: changing,
        // The inverse of trashing is restoring, and vice versa.
        trashed: !trashed,
    }))
}

/// Which of `asset_ids` still exist. Used only by undo.
async fn alive_assets(
    conn: &mut sqlx::SqliteConnection,
    asset_ids: &[String],
) -> Result<HashSet<String>> {
    let mut out = HashSet::new();
    for chunk in asset_ids.chunks(IDS_PER_QUERY) {
        let mut qb = QueryBuilder::<Sqlite>::new("SELECT id FROM assets WHERE id IN (");
        let mut sep = qb.separated(", ");
        for id in chunk {
            sep.push_bind(id);
        }
        qb.push(")");
        let rows: Vec<String> = qb
            .build_query_scalar()
            .fetch_all(&mut *conn)
            .await
            .context("Failed to check which assets still exist")?;
        out.extend(rows);
    }
    Ok(out)
}

// ── Stored actions ───────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct QuickAction {
    pub id: String,
    pub name: String,
    /// Lucide icon name, so the menu shows intent before the label is read.
    pub icon: Option<String>,
    /// Palette token (see `assets::PIN_COLORS`), never a hex value.
    pub color: Option<String>,
    /// 1..=9, bound to Ctrl+Shift+&lt;n&gt;. `None` = no shortcut.
    pub shortcut: Option<i64>,
    pub position: f64,
    pub steps: Vec<Step>,
}

/// What the caller sends when creating or editing. Separate from `QuickAction`
/// because `id` and `position` are the store's business, not the editor's.
#[derive(Deserialize, Debug, Clone)]
pub struct QuickActionDraft {
    pub name: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub shortcut: Option<i64>,
    pub steps: Vec<Step>,
}

#[derive(FromRow)]
struct QuickActionRow {
    id: String,
    name: String,
    icon: Option<String>,
    color: Option<String>,
    shortcut: Option<i64>,
    position: f64,
    version: i64,
    steps_json: String,
}

const SELECT_COLS: &str =
    "SELECT id, name, icon, color, shortcut, position, version, steps_json FROM quick_actions";

impl QuickActionRow {
    /// `None` for a row this build cannot honour. One unreadable action must not
    /// take the whole menu down with it.
    fn decode(self) -> Option<QuickAction> {
        if self.version > ACTION_VERSION {
            warn!(id = %self.id, version = self.version, "Quick action is from a newer version; skipping");
            return None;
        }
        match decode_steps(&self.steps_json, self.version) {
            Ok(steps) => Some(QuickAction {
                id: self.id,
                name: self.name,
                icon: self.icon,
                color: self.color,
                shortcut: self.shortcut,
                position: self.position,
                steps,
            }),
            Err(e) => {
                warn!(id = %self.id, error = %e, "Quick action has unreadable steps_json; skipping");
                None
            }
        }
    }
}

#[instrument(skip_all)]
pub async fn fetch_quick_actions(pool: &SqlitePool) -> Result<Vec<QuickAction>> {
    let rows = sqlx::query_as::<_, QuickActionRow>(&format!(
        "{SELECT_COLS} ORDER BY position, name"
    ))
    .fetch_all(pool)
    .await
    .context("Failed to fetch quick actions")?;

    Ok(rows.into_iter().filter_map(QuickActionRow::decode).collect())
}

async fn fetch_one(pool: &SqlitePool, id: &str) -> Result<QuickAction> {
    let row = sqlx::query_as::<_, QuickActionRow>(&format!("{SELECT_COLS} WHERE id = ?"))
        .bind(id)
        .fetch_optional(pool)
        .await
        .context("Failed to read quick action")?
        .ok_or_else(|| anyhow::anyhow!("Quick action not found"))?;

    row.decode()
        .ok_or_else(|| anyhow::anyhow!("This action was saved by a newer version of Nova"))
}

/// Reject a draft the store cannot honour. Runs before any write so a bad edit
/// never lands half-applied.
async fn validate(pool: &SqlitePool, draft: &QuickActionDraft, editing: Option<&str>) -> Result<String> {
    let name = crate::assets::clean_name(&draft.name).context("An action needs a name")?;

    if draft.steps.is_empty() || !draft.steps.iter().any(|s| s.op.is_active()) {
        bail!("Add at least one step that does something");
    }
    // The same depth cap the smart folder editor enforces, checked here too:
    // an action is another way to author a rule tree, and a document that the
    // compiler would reject must not reach the database.
    for step in &draft.steps {
        if let Some(rules) = &step.when {
            rules.validate()?;
        }
    }
    if let Some(color) = &draft.color {
        if !crate::assets::PIN_COLORS.contains(&color.as_str()) {
            bail!("Unknown colour");
        }
    }
    if let Some(n) = draft.shortcut {
        if !(1..=9).contains(&n) {
            bail!("Shortcuts run from Ctrl+Shift+1 to Ctrl+Shift+9");
        }
        // Report the conflict by NAME. The unique index would reject this write
        // anyway, but "already used by Archive old" is something the user can
        // act on and "UNIQUE constraint failed" is not.
        let holder: Option<String> = sqlx::query_scalar(
            "SELECT name FROM quick_actions WHERE shortcut = ? AND id IS NOT ?",
        )
        .bind(n)
        .bind(editing)
        .fetch_optional(pool)
        .await
        .context("Failed to check shortcut availability")?;
        if let Some(other) = holder {
            bail!("Ctrl+Shift+{n} is already used by \"{other}\"");
        }
    }
    Ok(name)
}

fn encode_steps(steps: &[Step]) -> Result<String> {
    serde_json::to_string(steps).context("Failed to encode action steps")
}

/// Read a stored document at whatever version wrote it.
///
/// A v1 action is a bare array of operations, which is exactly a v2 array with
/// no conditions — so the upgrade is total and lossless, and every action
/// written before this phase keeps working untouched. Upgrading on READ rather
/// than migrating the table means a half-finished migration can't exist.
fn decode_steps(json: &str, version: i64) -> Result<Vec<Step>> {
    if version <= 1 {
        let ops: Vec<Op> = serde_json::from_str(json)?;
        return Ok(ops
            .into_iter()
            .map(|op| Step { op, when: None })
            .collect());
    }
    Ok(serde_json::from_str(json)?)
}

#[instrument(skip_all, fields(name = %draft.name, steps = draft.steps.len()))]
pub async fn create_quick_action(
    pool: &SqlitePool,
    draft: QuickActionDraft,
) -> Result<QuickAction> {
    let name = validate(pool, &draft, None).await?;
    let id = uuid::Uuid::new_v4().to_string();
    let position: f64 = sqlx::query_scalar::<_, Option<f64>>(
        "SELECT MAX(position) FROM quick_actions",
    )
    .fetch_one(pool)
    .await
    .context("Failed to compute action position")?
    .map(|m| m + 1.0)
    .unwrap_or(0.0);

    sqlx::query(
        "INSERT INTO quick_actions (id, name, icon, color, shortcut, position, steps_json, version)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&name)
    .bind(&draft.icon)
    .bind(&draft.color)
    .bind(draft.shortcut)
    .bind(position)
    .bind(encode_steps(&draft.steps)?)
    .bind(ACTION_VERSION)
    .execute(pool)
    .await
    .context("Failed to create quick action")?;

    Ok(QuickAction {
        id,
        name,
        icon: draft.icon,
        color: draft.color,
        shortcut: draft.shortcut,
        position,
        steps: draft.steps,
    })
}

#[instrument(skip_all, fields(id, steps = draft.steps.len()))]
pub async fn update_quick_action(
    pool: &SqlitePool,
    id: &str,
    draft: QuickActionDraft,
) -> Result<()> {
    let name = validate(pool, &draft, Some(id)).await?;
    let res = sqlx::query(
        "UPDATE quick_actions SET name = ?, icon = ?, color = ?, shortcut = ?, steps_json = ?,
                version = ? WHERE id = ?",
    )
    .bind(&name)
    .bind(&draft.icon)
    .bind(&draft.color)
    .bind(draft.shortcut)
    .bind(encode_steps(&draft.steps)?)
    .bind(ACTION_VERSION)
    .bind(id)
    .execute(pool)
    .await
    .context("Failed to update quick action")?;

    if res.rows_affected() == 0 {
        bail!("Quick action not found");
    }
    Ok(())
}

#[instrument(skip(pool))]
pub async fn delete_quick_action(pool: &SqlitePool, id: &str) -> Result<()> {
    sqlx::query("DELETE FROM quick_actions WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to delete quick action")?;
    Ok(())
}

// ── Running ──────────────────────────────────────────────────────────────────

/// What the confirmation dialog needs before a large run.
///
/// A dry run rather than a bare "are you sure": at 10,000 assets a good preview
/// prevents more damage than a good undo, because it stops the mistake instead
/// of describing how to reverse it.
#[derive(Serialize, Debug)]
pub struct RunPreview {
    pub name: String,
    pub asset_count: usize,
    pub step_count: usize,
    /// Estimated from the selection size, so the dialog can warn BEFORE running
    /// rather than leaving the user to discover it at undo time.
    pub will_be_undoable: bool,
    /// Steps that cannot run — a deleted tag, a bad pattern. Non-empty means the
    /// run is BLOCKED.
    pub problems: Vec<String>,
    /// Things the user probably didn't mean but is allowed to do. Kept separate
    /// from `problems` because the difference is whether the run proceeds: a
    /// pattern that gives 9,998 assets the same name is legal (Nova has no
    /// unique constraint on filenames, and two files really can share a name)
    /// and is nonetheless almost never the intent.
    pub warnings: Vec<String>,
}

#[derive(Serialize, Debug)]
pub struct RunSummary {
    /// `None` when the run left no history entry — a small direct manipulation
    /// that isn't worth one. Distinct from `is_undoable: false`, which means a
    /// run WAS recorded but its inverse was too large to keep.
    pub run_id: Option<String>,
    pub name: String,
    pub asset_count: usize,
    pub is_undoable: bool,
}

/// Rough size of the inverse this run would record.
///
/// A uuid costs 36 characters plus JSON quoting and separators; 48 is that with
/// headroom. Deliberately an over-estimate — warning about a run that turns out
/// to be undoable is a smaller failure than promising undo and not delivering.
fn estimated_undo_bytes(steps: &[Step], asset_count: usize) -> usize {
    steps
        .iter()
        .map(|s| s.op.undo_bytes_per_asset())
        .sum::<usize>()
        .saturating_mul(asset_count)
}

/// Ids referenced by the steps that no longer exist in `table`.
///
/// One helper for tags and folders because the check is identical and the
/// consequence is too: a step pointing at a deleted row does not fail, it
/// silently does nothing, which is the failure mode worth blocking on.
async fn missing_refs(pool: &SqlitePool, table: &str, refs: HashSet<&String>) -> Result<Vec<String>> {
    if refs.is_empty() {
        return Ok(Vec::new());
    }
    // `table` is a literal chosen by the two call sites below, never user input.
    let mut qb = QueryBuilder::<Sqlite>::new(format!("SELECT id FROM {table} WHERE id IN ("));
    let mut sep = qb.separated(", ");
    for id in &refs {
        sep.push_bind(*id);
    }
    qb.push(")");
    let found: HashSet<String> = qb
        .build_query_scalar()
        .fetch_all(pool)
        .await
        .with_context(|| format!("Failed to check the {table} this action uses"))?
        .into_iter()
        .collect();

    Ok(refs
        .into_iter()
        .filter(|id| !found.contains(*id))
        .cloned()
        .collect())
}

/// Everything a step depends on that is gone, as sentences for the UI.
async fn broken_refs(pool: &SqlitePool, steps: &[Step]) -> Result<Vec<String>> {
    let mut problems = Vec::new();
    let tags = missing_refs(pool, "tags", steps.iter().flat_map(|s| s.op.tag_refs()).collect()).await?;
    if !tags.is_empty() {
        problems.push(format!(
            "{} tag(s) this action uses no longer exist",
            tags.len()
        ));
    }
    let folders = missing_refs(
        pool,
        "folders",
        steps.iter().flat_map(|s| s.op.folder_refs()).collect(),
    )
    .await?;
    if !folders.is_empty() {
        problems.push(format!(
            "{} folder(s) this action uses no longer exist",
            folders.len()
        ));
    }
    Ok(problems)
}

/// Deduplicate while preserving order. Repeated ids inside one `IN (...)` are
/// harmless, but they'd inflate the inverse and the reported count.
fn unique(ids: &[String]) -> Vec<String> {
    let mut seen = HashSet::with_capacity(ids.len());
    ids.iter()
        .filter(|id| seen.insert((*id).clone()))
        .cloned()
        .collect()
}

#[instrument(skip(pool, asset_ids), fields(count = asset_ids.len()))]
pub async fn preview_run(
    pool: &SqlitePool,
    action_id: &str,
    asset_ids: &[String],
) -> Result<RunPreview> {
    let action = fetch_one(pool, action_id).await?;
    let ids = unique(asset_ids);

    let mut problems = Vec::new();
    if ids.is_empty() {
        problems.push("Nothing is selected".to_string());
    }
    problems.extend(broken_refs(pool, &action.steps).await?);

    let mut warnings = Vec::new();
    let mut conn = pool
        .acquire()
        .await
        .context("Failed to open a connection for the preview")?;
    for (i, step) in action.steps.iter().enumerate() {
        // A condition matching nothing isn't an error — the step just doesn't
        // apply — but it's nearly always a mistake worth seeing before the run,
        // because the rest of the pipeline still goes ahead without it.
        if step.when.is_some() && !ids.is_empty() {
            let targets = step.targets(&mut conn, &ids).await?;
            if targets.is_empty() {
                warnings.push(format!("Step {} matches none of the selection", i + 1));
            }
        }

        if let Op::RenameWithPattern {
            pattern,
            index_order,
            index_ascending,
            index_start,
            index_pad,
            date_field,
        } = &step.op
        {
            // A bad pattern BLOCKS; colliding output only warns.
            let tokens = match parse_pattern(pattern) {
                Ok(t) => t,
                Err(e) => {
                    problems.push(e.to_string());
                    continue;
                }
            };
            if ids.is_empty() {
                continue;
            }
            let rows = rename_rows(&mut conn, &ids, *index_order, *index_ascending, None).await?;
            match render_all(&tokens, &rows, *index_start, *index_pad, *date_field) {
                Ok(renamed) => {
                    let unique: HashSet<&String> = renamed.iter().map(|(_, n)| n).collect();
                    let collisions = renamed.len() - unique.len();
                    if collisions > 0 {
                        warnings.push(format!(
                            "{collisions} assets would end up sharing a name — add {{index}} to \
                             tell them apart"
                        ));
                    }
                }
                Err(e) => problems.push(e.to_string()),
            }
        }
    }

    Ok(RunPreview {
        name: action.name,
        asset_count: ids.len(),
        step_count: action.steps.len(),
        will_be_undoable: estimated_undo_bytes(&action.steps, ids.len()) <= UNDO_BUDGET_BYTES,
        problems,
        warnings,
    })
}

/// One before/after pair for the editor's live preview.
#[derive(Serialize, Debug)]
pub struct RenameSample {
    pub before: String,
    pub after: String,
}

/// What the pattern box shows while you type.
///
/// `error` rather than a failed command: a half-typed pattern is the normal
/// state of this control, and raising it as an error would fire a toast on every
/// keystroke. The editor renders it inline instead.
#[derive(Serialize, Debug)]
pub struct RenamePreview {
    pub rows: Vec<RenameSample>,
    pub error: Option<String>,
}

/// Render a rename step against real assets.
///
/// Falls back to a few of the library's own assets when nothing is selected, so
/// the pattern is judgeable while an action is being written rather than only at
/// the moment it runs. Uses the SAME render path as `apply`, so a preview can't
/// disagree with the result.
#[instrument(skip(pool, asset_ids), fields(count = asset_ids.len()))]
pub async fn preview_rename(
    pool: &SqlitePool,
    step: &Step,
    asset_ids: &[String],
    limit: usize,
) -> Result<RenamePreview> {
    let Op::RenameWithPattern {
        pattern,
        index_order,
        index_ascending,
        index_start,
        index_pad,
        date_field,
    } = &step.op
    else {
        bail!("Not a rename step");
    };

    let tokens = match parse_pattern(pattern) {
        Ok(t) => t,
        Err(e) => {
            return Ok(RenamePreview {
                rows: Vec::new(),
                error: Some(e.to_string()),
            })
        }
    };

    let mut conn = pool
        .acquire()
        .await
        .context("Failed to open a connection for the preview")?;

    let ids = unique(asset_ids);
    let rows = if ids.is_empty() {
        sample_rows(&mut conn, limit).await?
    } else {
        rename_rows(&mut conn, &ids, *index_order, *index_ascending, Some(limit)).await?
    };

    match render_all(&tokens, &rows, *index_start, *index_pad, *date_field) {
        Ok(renamed) => Ok(RenamePreview {
            rows: rows
                .iter()
                .zip(renamed)
                .take(limit)
                .map(|(row, (_, after))| RenameSample {
                    before: row.filename.clone(),
                    after,
                })
                .collect(),
            error: None,
        }),
        Err(e) => Ok(RenamePreview {
            rows: Vec::new(),
            error: Some(e.to_string()),
        }),
    }
}

/// A few arbitrary assets, for previewing with nothing selected.
async fn sample_rows(conn: &mut sqlx::SqliteConnection, limit: usize) -> Result<Vec<RenameRow>> {
    sqlx::query_as::<_, RenameRow>(
        "SELECT id, filename, extension, width, height, file_size, \
                imported_date, creation_date, modified_date \
         FROM assets ORDER BY imported_date DESC, id LIMIT ?",
    )
    .bind(limit as i64)
    .fetch_all(&mut *conn)
    .await
    .context("Failed to read sample assets")
}

/// Below this, a direct manipulation isn't worth a history entry.
///
/// Undo exists for the change you CAN'T see. Moving one asset into a folder is
/// visible and reversible by dragging it back; moving four hundred is neither.
/// Recording every single-asset edit would also flood the history so the one
/// bulk mistake you actually want back is no longer the most recent run.
///
/// Quick actions are exempt — a macro is opaque at any size.
const UNDO_MIN_ASSETS: usize = 2;

/// Where a run came from. Decides whether it earns a place in the history.
#[derive(Debug)]
pub enum RunSource<'a> {
    /// A saved quick action. Always recorded.
    Action { id: &'a str, name: &'a str },
    /// Direct manipulation — a drag, a tag toggle. Recorded only above
    /// `UNDO_MIN_ASSETS`.
    Direct { name: &'a str },
}

impl RunSource<'_> {
    fn name(&self) -> &str {
        match self {
            RunSource::Action { name, .. } | RunSource::Direct { name } => name,
        }
    }
    fn action_id(&self) -> Option<&str> {
        match self {
            RunSource::Action { id, .. } => Some(id),
            RunSource::Direct { .. } => None,
        }
    }
    fn records_history(&self, asset_count: usize) -> bool {
        match self {
            RunSource::Action { .. } => true,
            RunSource::Direct { .. } => asset_count >= UNDO_MIN_ASSETS,
        }
    }
}

/// Apply an action to a snapshotted selection, atomically.
///
/// `asset_ids` is the selection as it was when the user triggered this, sent
/// once and never re-derived: the manifest streams and the watcher fires while a
/// run is in flight, and an action that changes what matches the current scope
/// will make assets vanish from the grid *as it runs*.
#[instrument(skip(pool, asset_ids), fields(count = asset_ids.len()))]
pub async fn run_action(
    pool: &SqlitePool,
    action_id: &str,
    asset_ids: &[String],
) -> Result<RunSummary> {
    let action = fetch_one(pool, action_id).await?;
    if let Some(problem) = broken_refs(pool, &action.steps).await?.into_iter().next() {
        bail!("{problem}. Edit the action and try again.");
    }
    run_steps(
        pool,
        RunSource::Action {
            id: &action.id,
            name: &action.name,
        },
        &action.steps,
        asset_ids,
    )
    .await
}

/// Apply a pipeline that isn't a saved action.
///
/// The same machinery, reached from a different door. Direct manipulation —
/// dragging a selection into a folder, toggling a tag across it — is expressible
/// as steps, so routing it through here is what gives bulk edits an inverse
/// without writing undo twice.
#[instrument(skip(pool, source, steps, asset_ids), fields(steps = steps.len(), count = asset_ids.len()))]
pub async fn run_steps(
    pool: &SqlitePool,
    source: RunSource<'_>,
    steps: &[Step],
    asset_ids: &[String],
) -> Result<RunSummary> {
    let ids = unique(asset_ids);
    if ids.is_empty() {
        bail!("Select some assets first");
    }
    // Belt to `begin_session`'s braces: a run must never be written before the
    // session it belongs to has started, or it would be invisible to undo.
    begin_session();

    let mut tx = pool
        .begin()
        .await
        .context("Failed to begin the action transaction")?;

    // Every step in ONE transaction: a failure at step 3 of 4 must leave the
    // library exactly as it was, not two-thirds changed.
    let mut payloads: Vec<String> = Vec::with_capacity(steps.len());
    for step in steps {
        let inverse = step.apply(&mut tx, &ids).await?;
        payloads.push(serde_json::to_string(&inverse).context("Failed to encode the undo record")?);
    }

    let total: usize = payloads.iter().map(String::len).sum();
    let is_undoable = total <= UNDO_BUDGET_BYTES;
    let keep = source.records_history(ids.len());

    let run_id = uuid::Uuid::new_v4().to_string();
    if keep {
        sqlx::query(
            "INSERT INTO action_runs (id, action_id, name, ran_at, asset_count, is_undoable)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&run_id)
        .bind(source.action_id())
        .bind(source.name())
        .bind(crate::assets::now_stamp())
        .bind(ids.len() as i64)
        .bind(is_undoable)
        .execute(&mut *tx)
        .await
        .context("Failed to record the run")?;
    }
    let is_undoable = is_undoable && keep;

    if is_undoable {
        for (seq, payload) in payloads.iter().enumerate() {
            sqlx::query("INSERT INTO action_undo (run_id, seq, payload_json) VALUES (?, ?, ?)")
                .bind(&run_id)
                .bind(seq as i64)
                .bind(payload)
                .execute(&mut *tx)
                .await
                .context("Failed to record the undo step")?;
        }
    } else if keep {
        warn!(bytes = total, "Run exceeded the undo budget; recorded as not undoable");
    }

    // Prune inside the same transaction, so history can never be longer than the
    // cap even if the app dies immediately after a run.
    sqlx::query(
        "DELETE FROM action_runs WHERE id NOT IN
             (SELECT id FROM action_runs ORDER BY ran_at DESC, id DESC LIMIT ?)",
    )
    .bind(MAX_RUNS)
    .execute(&mut *tx)
    .await
    .context("Failed to prune the run history")?;

    tx.commit()
        .await
        .context("Failed to commit the action transaction")?;

    // ONE reindex for the whole pipeline, after commit and non-fatal — five
    // steps must not mean five full-text rebuilds, and a failed reindex must not
    // roll back work the user can see.
    if let Err(e) = crate::search::reindex_assets(pool, &ids).await {
        warn!(error = %e, "Reindex after quick action failed (non-fatal)");
    }

    Ok(RunSummary {
        run_id: keep.then_some(run_id),
        name: source.name().to_string(),
        asset_count: ids.len(),
        is_undoable,
    })
}

/// How an undo turned out. Partial success is a real outcome, not a failure:
/// assets deleted since the run cannot be restored to, and refusing to undo the
/// other 4,180 because of them would help nobody.
#[derive(Serialize, Debug)]
pub struct UndoSummary {
    pub name: String,
    pub restored: usize,
    pub skipped: usize,
}

/// When this app session began.
///
/// `action_runs` is a table, so it outlives the process — without this bound,
/// launching Nova and pressing Ctrl+Z would silently reverse whatever you did
/// last week, with nothing on screen to say what changed. Undo is for the thing
/// you just did and can still picture; past that it's a trap, and the history is
/// still browsable as history.
///
/// Initialised on first touch, which is either the first run or the first undo.
/// If an undo comes first, nothing qualifies — correct, because nothing has
/// happened this session yet.
static SESSION_START: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(crate::assets::now_stamp);

/// Pin the session start at app launch.
///
/// Called from `lib.rs` so "this session" means the process, not "whenever
/// something first asked". Without it the first touch could land AFTER a run had
/// already been written, which would put that run outside its own session.
pub fn begin_session() {
    std::sync::LazyLock::force(&SESSION_START);
}

/// The most recent run of THIS SESSION that can still be undone. Backs Ctrl+Z.
pub async fn latest_undoable_run(pool: &SqlitePool) -> Result<Option<String>> {
    sqlx::query_scalar(
        "SELECT id FROM action_runs WHERE is_undoable = 1 AND ran_at >= ? \
         ORDER BY ran_at DESC, id DESC LIMIT 1",
    )
    .bind(&*SESSION_START)
    .fetch_optional(pool)
    .await
    .context("Failed to read the run history")
}

#[instrument(skip(pool))]
pub async fn undo_run(pool: &SqlitePool, run_id: &str) -> Result<UndoSummary> {
    let (name, is_undoable): (String, bool) =
        sqlx::query_as("SELECT name, is_undoable FROM action_runs WHERE id = ?")
            .bind(run_id)
            .fetch_optional(pool)
            .await
            .context("Failed to read the run")?
            .ok_or_else(|| anyhow::anyhow!("That run is no longer in the history"))?;

    if !is_undoable {
        bail!("\"{name}\" was too large to record an undo for");
    }

    // DESCENDING: the inverse of (A then B) is (B⁻¹ then A⁻¹).
    let payloads: Vec<String> =
        sqlx::query_scalar("SELECT payload_json FROM action_undo WHERE run_id = ? ORDER BY seq DESC")
            .bind(run_id)
            .fetch_all(pool)
            .await
            .context("Failed to read the undo record")?;

    let mut inverses: Vec<Inverse> = Vec::new();
    for payload in &payloads {
        inverses.extend(
            serde_json::from_str::<Vec<Inverse>>(payload)
                .context("The undo record for this run is unreadable")?,
        );
    }

    let touched: Vec<String> = unique(
        &inverses
            .iter()
            .flat_map(Inverse::asset_ids)
            .collect::<Vec<_>>(),
    );

    let mut tx = pool
        .begin()
        .await
        .context("Failed to begin the undo transaction")?;

    let alive = alive_assets(&mut tx, &touched).await?;
    for inverse in &inverses {
        inverse.retaining(&alive).apply(&mut tx).await?;
    }

    // The run is consumed. Leaving it would let a second undo double-apply, and
    // there is no redo by design — re-running the action is the redo.
    sqlx::query("DELETE FROM action_runs WHERE id = ?")
        .bind(run_id)
        .execute(&mut *tx)
        .await
        .context("Failed to clear the undone run")?;

    tx.commit()
        .await
        .context("Failed to commit the undo transaction")?;

    let restored: Vec<String> = touched.iter().filter(|id| alive.contains(*id)).cloned().collect();
    if let Err(e) = crate::search::reindex_assets(pool, &restored).await {
        warn!(error = %e, "Reindex after undo failed (non-fatal)");
    }

    Ok(UndoSummary {
        name,
        skipped: touched.len() - restored.len(),
        restored: restored.len(),
    })
}

/// Runs from THIS SESSION, newest first.
///
/// Session-bounded for the same reason Ctrl+Z is: the ⚡ menu shows these as an
/// *offer* to undo, and offering to reverse something from a previous session is
/// the same trap in a different control. A reload inside one session still keeps
/// its offers, which is what this is for.
#[derive(Serialize, Debug, FromRow)]
pub struct ActionRun {
    pub id: String,
    pub name: String,
    pub ran_at: String,
    pub asset_count: i64,
    pub is_undoable: bool,
}

#[instrument(skip_all)]
pub async fn fetch_recent_runs(pool: &SqlitePool) -> Result<Vec<ActionRun>> {
    sqlx::query_as::<_, ActionRun>(
        "SELECT id, name, ran_at, asset_count, is_undoable FROM action_runs
         WHERE ran_at >= ? ORDER BY ran_at DESC, id DESC",
    )
    .bind(&*SESSION_START)
    .fetch_all(pool)
    .await
    .context("Failed to fetch the run history")
}

// ── Tests ────────────────────────────────────────────────────────────────────

/// The wire format, pinned BEFORE anything consumes it.
///
/// `Step` crosses three boundaries — the IPC payload, the `steps_json` column,
/// and the hand-written TypeScript mirror — and every seam bug in this project
/// so far has lived in exactly that kind of gap. These assert the literal JSON
/// rather than round-tripping through serde, because a round-trip test passes
/// happily while both sides agree on the wrong spelling.
#[cfg(test)]
mod wire_tests {
    use super::*;

    #[test]
    fn step_json_shape_is_stable() {
        let step = Op::AddTags {
            tag_ids: vec!["t1".into()],
        };
        let json = serde_json::to_value(&step).unwrap();
        assert_eq!(json["type"], "add_tags");
        assert_eq!(json["tag_ids"][0], "t1");

        let step = Op::RemoveTags {
            tag_ids: vec!["t2".into()],
        };
        let json = serde_json::to_value(&step).unwrap();
        assert_eq!(json["type"], "remove_tags");
    }

    /// Every variant's discriminator, in one place. The TS mirror is a hand
    /// transcription of exactly this list.
    #[test]
    fn every_step_type_has_the_spelling_typescript_expects() {
        let cases: Vec<(Op, &str)> = vec![
            (Op::AddTags { tag_ids: vec![] }, "add_tags"),
            (Op::RemoveTags { tag_ids: vec![] }, "remove_tags"),
            (Op::ClearAllTags, "clear_all_tags"),
            (
                Op::AddToFolder {
                    folder_id: "f".into(),
                },
                "add_to_folder",
            ),
            (
                Op::RemoveFromFolder {
                    folder_id: "f".into(),
                },
                "remove_from_folder",
            ),
            (
                Op::SetFolders {
                    folder_ids: vec![],
                },
                "set_folders",
            ),
            (
                Op::SetNote {
                    mode: TextMode::Replace,
                    text: String::new(),
                },
                "set_note",
            ),
            (
                Op::SetSourceUrl { url: String::new() },
                "set_source_url",
            ),
            (Op::MoveToTrash, "move_to_trash"),
            (Op::RestoreFromTrash, "restore_from_trash"),
        ];
        for (step, expected) in cases {
            assert_eq!(serde_json::to_value(&step).unwrap()["type"], expected);
        }
    }

    /// A unit variant must be an OBJECT with a type, not a bare string — that's
    /// what `tag = "type"` guarantees and what the TS union assumes.
    #[test]
    fn a_unit_step_still_carries_its_tag() {
        let json = serde_json::to_string(&Op::ClearAllTags).unwrap();
        assert_eq!(json, r#"{"type":"clear_all_tags"}"#);
    }

    #[test]
    fn text_mode_spellings_are_stable() {
        for (mode, expected) in [
            (TextMode::Replace, "\"replace\""),
            (TextMode::Append, "\"append\""),
            (TextMode::Prepend, "\"prepend\""),
        ] {
            assert_eq!(serde_json::to_string(&mode).unwrap(), expected);
        }
        assert_eq!(
            serde_json::to_string(&TextTarget::SourceUrl).unwrap(),
            "\"source_url\""
        );
    }

    #[test]
    fn inverse_json_shape_is_stable() {
        let inv = Inverse::RemoveTag {
            tag_id: "t1".into(),
            asset_ids: vec!["a1".into()],
        };
        let json = serde_json::to_value(&inv).unwrap();
        assert_eq!(json["type"], "remove_tag");
        assert_eq!(json["tag_id"], "t1");
        assert_eq!(json["asset_ids"][0], "a1");

        let inv = Inverse::AddTag {
            tag_id: "t1".into(),
            asset_ids: vec![],
        };
        assert_eq!(serde_json::to_value(&inv).unwrap()["type"], "add_tag");
    }

    /// The stored document is a bare ARRAY of steps, not an object wrapping one.
    /// The TS mirror and `decode` both depend on that.
    #[test]
    fn steps_encode_as_a_json_array_of_op_and_when() {
        let steps = vec![
            Step {
                op: Op::AddTags {
                    tag_ids: vec!["a".into()],
                },
                when: None,
            },
            Step {
                op: Op::RemoveTags {
                    tag_ids: vec!["b".into()],
                },
                when: None,
            },
        ];
        let encoded = encode_steps(&steps).unwrap();
        assert!(encoded.starts_with('['), "got {encoded}");

        // The op is NESTED under `op`, not flattened alongside `when` — that's
        // the shape the TS mirror transcribes and the one `flatten` would have
        // quietly broken.
        let v: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(v[0]["op"]["type"], "add_tags");
        assert_eq!(v[0]["when"], serde_json::Value::Null);
        assert_eq!(v[1]["op"]["type"], "remove_tags");
    }

    /// A v1 document is a bare array of operations. Every action written before
    /// conditions existed has to keep working, so it's upgraded on read rather
    /// than rejected by the version gate.
    #[test]
    fn a_version_1_document_upgrades_to_unconditional_steps() {
        let v1 = r#"[{"type":"add_tags","tag_ids":["t1"]},{"type":"clear_all_tags"}]"#;
        let steps = decode_steps(v1, 1).unwrap();
        assert_eq!(steps.len(), 2);
        assert!(steps.iter().all(|s| s.when.is_none()));
        assert!(matches!(steps[0].op, Op::AddTags { .. }));

        // And the v1 shape is NOT accepted as v2 — that would mean the version
        // column had drifted from the document, which should fail loudly.
        assert!(decode_steps(v1, 2).is_err());
    }

    #[test]
    fn a_condition_round_trips_through_the_document() {
        let json = r#"[{"op":{"type":"clear_all_tags"},
                        "when":{"kind":"condition","type":"media_type","types":["video"]}}]"#;
        let steps = decode_steps(json, ACTION_VERSION).unwrap();
        assert!(steps[0].when.is_some());
        let back: serde_json::Value =
            serde_json::from_str(&encode_steps(&steps).unwrap()).unwrap();
        assert_eq!(back[0]["when"]["type"], "media_type");
    }

    /// A draft arrives from TypeScript with the optional fields absent, not null
    /// — including `when`, which the editor omits entirely for an unconditional
    /// step rather than sending an explicit null.
    #[test]
    fn draft_accepts_a_minimal_payload() {
        let draft: QuickActionDraft = serde_json::from_str(
            r#"{"name":"Tag it","steps":[{"op":{"type":"add_tags","tag_ids":["t1"]}}]}"#,
        )
        .unwrap();
        assert_eq!(draft.name, "Tag it");
        assert!(draft.icon.is_none());
        assert!(draft.shortcut.is_none());
        assert_eq!(draft.steps.len(), 1);
        assert!(draft.steps[0].when.is_none());
    }

    /// An unknown step type must fail loudly. This is what makes the version gate
    /// meaningful: a newer document is skipped, never half-read.
    #[test]
    fn unknown_step_type_is_rejected() {
        assert!(serde_json::from_str::<Vec<Op>>(r#"[{"type":"resize","width":100}]"#).is_err());
    }

    #[test]
    fn a_newer_version_row_is_skipped_not_parsed() {
        let row = QuickActionRow {
            id: "a1".into(),
            name: "From the future".into(),
            icon: None,
            color: None,
            shortcut: None,
            position: 0.0,
            version: ACTION_VERSION + 1,
            steps_json: "[]".into(),
        };
        assert!(row.decode().is_none());
    }

    #[test]
    fn an_unreadable_document_is_skipped_not_fatal() {
        let row = QuickActionRow {
            id: "a1".into(),
            name: "Broken".into(),
            icon: None,
            color: None,
            shortcut: None,
            position: 0.0,
            version: ACTION_VERSION,
            steps_json: "{ not json".into(),
        };
        assert!(row.decode().is_none());
    }
}

/// The round trip against a real database.
///
/// The claim this feature rests on is that undo restores the state that existed
/// BEFORE the run — not that it removes what the run added. Those differ exactly
/// when an asset already carried the tag, which is the common case for any
/// action run twice, so it's the case these tests are built around.
#[cfg(test)]
mod exec_tests {
    use super::*;

    async fn db() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        for stmt in [
            // Named columns everywhere below, and every column reachable from a
            // step OR from a step's `when` condition is present: a positional
            // fixture is how the FTS tests broke when a column was added, and a
            // short one is how these broke twice while conditions were landing.
            "CREATE TABLE assets (id TEXT PRIMARY KEY, filename TEXT, notes TEXT, source_url TEXT, \
             asset_type TEXT NOT NULL DEFAULT 'image', \
             extension TEXT NOT NULL DEFAULT 'png', width INTEGER NOT NULL DEFAULT 1920, \
             height INTEGER NOT NULL DEFAULT 1080, file_size INTEGER NOT NULL DEFAULT 100, \
             imported_date TEXT NOT NULL DEFAULT '2026-07-26T10:00:00.000Z', deleted_at TEXT, \
             creation_date TEXT NOT NULL DEFAULT '2024-01-15T08:00:00.000Z', \
             modified_date TEXT NOT NULL DEFAULT '2025-03-03T00:00:00.000Z')",
            "CREATE TABLE tags (id TEXT PRIMARY KEY, name TEXT)",
            "CREATE TABLE folders (id TEXT PRIMARY KEY, name TEXT, parent_id TEXT)",
            "CREATE TABLE folder_auto_tags (folder_id TEXT, tag_id TEXT, PRIMARY KEY (folder_id, tag_id))",
            "CREATE TABLE assets_folders (folder_id TEXT, asset_id TEXT, position REAL, \
             added_at TEXT, PRIMARY KEY (folder_id, asset_id))",
            // The PK is what makes `INSERT OR IGNORE` a no-op rather than a
            // duplicate row, which is the whole basis of the add-delta.
            "CREATE TABLE assets_tags (asset_id TEXT, tag_id TEXT, PRIMARY KEY (asset_id, tag_id))",
            "CREATE TABLE quick_actions (id TEXT PRIMARY KEY, name TEXT, icon TEXT, color TEXT, \
             shortcut INTEGER, position REAL, steps_json TEXT, version INTEGER, created_at TEXT)",
            "CREATE TABLE action_runs (id TEXT PRIMARY KEY, action_id TEXT, name TEXT, \
             ran_at TEXT, asset_count INTEGER, is_undoable INTEGER)",
            "CREATE TABLE action_undo (run_id TEXT, seq INTEGER, payload_json TEXT, \
             PRIMARY KEY (run_id, seq))",
            "INSERT INTO assets (id, filename, notes) VALUES \
             ('a1','one.png','keep me'), ('a2','two.png',NULL), ('a3','three.png',NULL)",
            "INSERT INTO tags VALUES ('t1','hero'), ('t2','draft')",
            "INSERT INTO folders (id, name) VALUES ('f1','Work'), ('f2','Archive')",
            // a1 ALREADY carries t1 — the asset that must survive an undo intact.
            "INSERT INTO assets_tags VALUES ('a1','t1')",
            // a1 sits in f1 at a hand-arranged position, with a known added_at.
            // Both must come back byte-for-byte after an undo.
            "INSERT INTO assets_folders VALUES ('f1','a1',7.5,'2026-01-01T00:00:00.000Z')",
        ] {
            sqlx::query(stmt).execute(&pool).await.unwrap();
        }
        pool
    }

    async fn tagged_with(pool: &SqlitePool, tag: &str) -> Vec<String> {
        sqlx::query_scalar("SELECT asset_id FROM assets_tags WHERE tag_id = ? ORDER BY asset_id")
            .bind(tag)
            .fetch_all(pool)
            .await
            .unwrap()
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    /// An operation with no condition — what almost every test wants.
    fn plain(op: Op) -> Step {
        Step { op, when: None }
    }

    /// Takes bare `Op`s so the tests written before conditions existed read the
    /// same as the ones written after.
    async fn action(pool: &SqlitePool, ops: Vec<Op>) -> QuickAction {
        action_with(pool, ops.into_iter().map(plain).collect()).await
    }

    async fn action_with(pool: &SqlitePool, steps: Vec<Step>) -> QuickAction {
        create_quick_action(
            pool,
            QuickActionDraft {
                name: "Test".into(),
                icon: None,
                color: None,
                shortcut: None,
                steps,
            },
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn undo_restores_the_prior_state_not_the_empty_one() {
        let pool = db().await;
        let a = action(
            &pool,
            vec![Op::AddTags {
                tag_ids: ids(&["t1"]),
            }],
        )
        .await;

        let summary = run_action(&pool, &a.id, &ids(&["a1", "a2", "a3"]))
            .await
            .unwrap();
        assert_eq!(summary.asset_count, 3);
        assert!(summary.is_undoable);
        assert_eq!(tagged_with(&pool, "t1").await, ids(&["a1", "a2", "a3"]));

        undo_run(&pool, summary.run_id.as_deref().unwrap()).await.unwrap();
        // a1 keeps the tag it had BEFORE the run. An inverse that stripped the
        // tag from everyone would leave this empty, and would be wrong.
        assert_eq!(tagged_with(&pool, "t1").await, ids(&["a1"]));
    }

    #[tokio::test]
    async fn the_inverse_names_only_what_changed() {
        let pool = db().await;
        let a = action(
            &pool,
            vec![Op::AddTags {
                tag_ids: ids(&["t1"]),
            }],
        )
        .await;
        let summary = run_action(&pool, &a.id, &ids(&["a1", "a2", "a3"]))
            .await
            .unwrap();

        let payload: String =
            sqlx::query_scalar("SELECT payload_json FROM action_undo WHERE run_id = ? AND seq = 0")
                .bind(&summary.run_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        let inverses: Vec<Inverse> = serde_json::from_str(&payload).unwrap();

        match &inverses[..] {
            [Inverse::RemoveTag { tag_id, asset_ids }] => {
                assert_eq!(tag_id, "t1");
                // a1 is absent: it gained nothing, so undoing must not take
                // anything from it.
                assert_eq!(asset_ids, &ids(&["a2", "a3"]));
            }
            other => panic!("unexpected inverse: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_multi_step_pipeline_undoes_in_reverse() {
        let pool = db().await;
        let a = action(
            &pool,
            vec![
                Op::AddTags {
                    tag_ids: ids(&["t2"]),
                },
                Op::RemoveTags {
                    tag_ids: ids(&["t1"]),
                },
            ],
        )
        .await;

        let summary = run_action(&pool, &a.id, &ids(&["a1", "a2"])).await.unwrap();
        assert_eq!(tagged_with(&pool, "t2").await, ids(&["a1", "a2"]));
        assert!(tagged_with(&pool, "t1").await.is_empty());

        undo_run(&pool, summary.run_id.as_deref().unwrap()).await.unwrap();
        assert!(tagged_with(&pool, "t2").await.is_empty());
        assert_eq!(tagged_with(&pool, "t1").await, ids(&["a1"]));
    }

    /// A run consumed by undo must not be undoable twice — that would apply the
    /// inverse to a state it was never computed against.
    #[tokio::test]
    async fn a_run_can_only_be_undone_once() {
        let pool = db().await;
        let a = action(
            &pool,
            vec![Op::AddTags {
                tag_ids: ids(&["t2"]),
            }],
        )
        .await;
        let summary = run_action(&pool, &a.id, &ids(&["a1"])).await.unwrap();

        undo_run(&pool, summary.run_id.as_deref().unwrap()).await.unwrap();
        assert!(undo_run(&pool, summary.run_id.as_deref().unwrap()).await.is_err());
    }

    /// Deleting an action must not destroy the ability to undo a run of it. The
    /// run already happened and its inverse is self-contained.
    #[tokio::test]
    async fn a_run_outlives_the_action_that_produced_it() {
        let pool = db().await;
        let a = action(
            &pool,
            vec![Op::AddTags {
                tag_ids: ids(&["t2"]),
            }],
        )
        .await;
        let summary = run_action(&pool, &a.id, &ids(&["a1", "a2"])).await.unwrap();

        delete_quick_action(&pool, &a.id).await.unwrap();
        undo_run(&pool, summary.run_id.as_deref().unwrap()).await.unwrap();
        assert!(tagged_with(&pool, "t2").await.is_empty());
    }

    #[tokio::test]
    async fn a_step_referencing_a_deleted_tag_blocks_the_run() {
        let pool = db().await;
        let a = action(
            &pool,
            vec![Op::AddTags {
                tag_ids: ids(&["gone"]),
            }],
        )
        .await;

        assert!(run_action(&pool, &a.id, &ids(&["a1"])).await.is_err());
        let preview = preview_run(&pool, &a.id, &ids(&["a1"])).await.unwrap();
        assert_eq!(preview.problems.len(), 1, "{:?}", preview.problems);
    }

    #[tokio::test]
    async fn two_actions_cannot_share_a_shortcut() {
        let pool = db().await;
        let draft = |shortcut| QuickActionDraft {
            name: "Test".into(),
            icon: None,
            color: None,
            shortcut: Some(shortcut),
            steps: vec![plain(Op::AddTags {
                tag_ids: ids(&["t1"]),
            })],
        };
        create_quick_action(&pool, draft(1)).await.unwrap();
        let err = create_quick_action(&pool, draft(1)).await.unwrap_err();
        // Names the holder, so the message is actionable.
        assert!(err.to_string().contains("Test"), "got {err}");
    }

    // ── Folder steps ─────────────────────────────────────────────────────────

    async fn members_of(pool: &SqlitePool, folder: &str) -> Vec<(String, f64)> {
        sqlx::query_as("SELECT asset_id, position FROM assets_folders WHERE folder_id = ? \
                        ORDER BY asset_id")
            .bind(folder)
            .fetch_all(pool)
            .await
            .unwrap()
    }

    async fn folders_of(pool: &SqlitePool, asset: &str) -> Vec<String> {
        sqlx::query_scalar("SELECT folder_id FROM assets_folders WHERE asset_id = ? ORDER BY folder_id")
            .bind(asset)
            .fetch_all(pool)
            .await
            .unwrap()
    }

    /// The subtlest contract in the language: membership is not a boolean, so an
    /// undo that restores the FACT of membership while losing the hand-arranged
    /// position has silently reshuffled a folder the user ordered themselves.
    #[tokio::test]
    async fn undoing_a_leave_restores_position_and_added_at() {
        let pool = db().await;
        let a = action(
            &pool,
            vec![Op::RemoveFromFolder {
                folder_id: "f1".into(),
            }],
        )
        .await;

        let summary = run_action(&pool, &a.id, &ids(&["a1"])).await.unwrap();
        assert!(members_of(&pool, "f1").await.is_empty());

        undo_run(&pool, summary.run_id.as_deref().unwrap()).await.unwrap();
        let restored: (f64, Option<String>) = sqlx::query_as(
            "SELECT position, added_at FROM assets_folders WHERE folder_id = 'f1' AND asset_id = 'a1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(restored.0, 7.5, "hand-arranged position was lost");
        assert_eq!(restored.1.as_deref(), Some("2026-01-01T00:00:00.000Z"));
    }

    /// a1 was already in f1, so joining gains it nothing — and undoing must not
    /// evict it from a folder it was in before the run.
    #[tokio::test]
    async fn undoing_a_join_leaves_prior_members_alone() {
        let pool = db().await;
        let a = action(
            &pool,
            vec![Op::AddToFolder {
                folder_id: "f1".into(),
            }],
        )
        .await;

        let summary = run_action(&pool, &a.id, &ids(&["a1", "a2"])).await.unwrap();
        assert_eq!(members_of(&pool, "f1").await.len(), 2);

        undo_run(&pool, summary.run_id.as_deref().unwrap()).await.unwrap();
        let after = members_of(&pool, "f1").await;
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].0, "a1");
        assert_eq!(after[0].1, 7.5, "an untouched member was repositioned");
    }

    #[tokio::test]
    async fn set_folders_replaces_every_membership() {
        let pool = db().await;
        let a = action(
            &pool,
            vec![Op::SetFolders {
                folder_ids: ids(&["f2"]),
            }],
        )
        .await;

        let summary = run_action(&pool, &a.id, &ids(&["a1"])).await.unwrap();
        assert_eq!(folders_of(&pool, "a1").await, ids(&["f2"]));

        undo_run(&pool, summary.run_id.as_deref().unwrap()).await.unwrap();
        // f2 is gone AND f1 is back: an inverse that only restored f1 would
        // leave the asset in both.
        assert_eq!(folders_of(&pool, "a1").await, ids(&["f1"]));
    }

    /// An empty list is a real instruction — file these nowhere — not a
    /// half-built step to be skipped.
    #[tokio::test]
    async fn set_folders_with_no_targets_uncategorizes() {
        let pool = db().await;
        let a = action(
            &pool,
            vec![Op::SetFolders {
                folder_ids: vec![],
            }],
        )
        .await;

        let summary = run_action(&pool, &a.id, &ids(&["a1"])).await.unwrap();
        assert!(folders_of(&pool, "a1").await.is_empty());

        undo_run(&pool, summary.run_id.as_deref().unwrap()).await.unwrap();
        assert_eq!(folders_of(&pool, "a1").await, ids(&["f1"]));
    }

    #[tokio::test]
    async fn a_step_referencing_a_deleted_folder_blocks_the_run() {
        let pool = db().await;
        let a = action(
            &pool,
            vec![Op::AddToFolder {
                folder_id: "gone".into(),
            }],
        )
        .await;
        assert!(run_action(&pool, &a.id, &ids(&["a1"])).await.is_err());
    }

    // ── Text steps ───────────────────────────────────────────────────────────

    async fn note_of(pool: &SqlitePool, asset: &str) -> Option<String> {
        sqlx::query_scalar("SELECT notes FROM assets WHERE id = ?")
            .bind(asset)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn replacing_a_note_restores_the_old_text_per_asset() {
        let pool = db().await;
        let a = action(
            &pool,
            vec![Op::SetNote {
                mode: TextMode::Replace,
                text: "shot 2026".into(),
            }],
        )
        .await;

        let summary = run_action(&pool, &a.id, &ids(&["a1", "a2"])).await.unwrap();
        assert_eq!(note_of(&pool, "a1").await.as_deref(), Some("shot 2026"));

        undo_run(&pool, summary.run_id.as_deref().unwrap()).await.unwrap();
        // Two assets, two different prior values — including a NULL, which must
        // come back as NULL rather than as an empty string.
        assert_eq!(note_of(&pool, "a1").await.as_deref(), Some("keep me"));
        assert_eq!(note_of(&pool, "a2").await, None);
    }

    /// Appending must not leave an empty note starting with a blank line.
    #[tokio::test]
    async fn appending_only_separates_when_there_is_something_to_separate() {
        let pool = db().await;
        let a = action(
            &pool,
            vec![Op::SetNote {
                mode: TextMode::Append,
                text: "approved".into(),
            }],
        )
        .await;

        run_action(&pool, &a.id, &ids(&["a1", "a2"])).await.unwrap();
        assert_eq!(note_of(&pool, "a1").await.as_deref(), Some("keep me\napproved"));
        assert_eq!(note_of(&pool, "a2").await.as_deref(), Some("approved"));
    }

    #[tokio::test]
    async fn prepending_puts_the_text_first() {
        let pool = db().await;
        let a = action(
            &pool,
            vec![Op::SetNote {
                mode: TextMode::Prepend,
                text: "DRAFT".into(),
            }],
        )
        .await;
        run_action(&pool, &a.id, &ids(&["a1"])).await.unwrap();
        assert_eq!(note_of(&pool, "a1").await.as_deref(), Some("DRAFT\nkeep me"));
    }

    /// Clearing is `Replace` with nothing, and must write NULL — otherwise an
    /// emptied note and one never written would sort and filter differently.
    #[tokio::test]
    async fn replacing_with_blank_clears_to_null() {
        let pool = db().await;
        let a = action(
            &pool,
            vec![Op::SetNote {
                mode: TextMode::Replace,
                text: String::new(),
            }],
        )
        .await;
        run_action(&pool, &a.id, &ids(&["a1"])).await.unwrap();
        assert_eq!(note_of(&pool, "a1").await, None);
    }

    #[tokio::test]
    async fn clear_all_tags_restores_every_tag_it_took() {
        let pool = db().await;
        sqlx::query("INSERT INTO assets_tags VALUES ('a1','t2'), ('a2','t2')")
            .execute(&pool)
            .await
            .unwrap();

        let a = action(&pool, vec![Op::ClearAllTags]).await;
        let summary = run_action(&pool, &a.id, &ids(&["a1", "a2"])).await.unwrap();
        assert!(tagged_with(&pool, "t1").await.is_empty());
        assert!(tagged_with(&pool, "t2").await.is_empty());

        undo_run(&pool, summary.run_id.as_deref().unwrap()).await.unwrap();
        assert_eq!(tagged_with(&pool, "t1").await, ids(&["a1"]));
        assert_eq!(tagged_with(&pool, "t2").await, ids(&["a1", "a2"]));
    }

    // ── Direct manipulation ──────────────────────────────────────────────────

    /// Dragging a selection into a folder is a pipeline too, and gets the same
    /// inverse — which is the whole point of routing it through here rather than
    /// writing undo a second time for direct manipulation.
    #[tokio::test]
    async fn a_direct_run_is_undoable_like_an_action() {
        let pool = db().await;
        let steps = vec![plain(Op::AddToFolder {
            folder_id: "f2".into(),
        })];

        let summary = run_steps(
            &pool,
            RunSource::Direct { name: "Add to folder" },
            &steps,
            &ids(&["a1", "a2"]),
        )
        .await
        .unwrap();

        assert_eq!(members_of(&pool, "f2").await.len(), 2);
        undo_run(&pool, summary.run_id.as_deref().unwrap()).await.unwrap();
        assert!(members_of(&pool, "f2").await.is_empty());
    }

    /// One asset is visible and reversible by hand, so it leaves no history —
    /// otherwise a single tag click would bury the bulk mistake you actually
    /// want back.
    #[tokio::test]
    async fn a_single_asset_direct_run_leaves_no_history() {
        let pool = db().await;
        let steps = vec![plain(Op::AddToFolder {
            folder_id: "f2".into(),
        })];

        let summary = run_steps(
            &pool,
            RunSource::Direct { name: "Add to folder" },
            &steps,
            &ids(&["a1"]),
        )
        .await
        .unwrap();

        // The work still happened; only the bookkeeping was skipped.
        assert_eq!(members_of(&pool, "f2").await.len(), 1);
        assert!(summary.run_id.is_none());
        assert!(!summary.is_undoable);
        assert!(fetch_recent_runs(&pool).await.unwrap().is_empty());
    }

    /// A macro is opaque at any size, so it records even for one asset.
    #[tokio::test]
    async fn a_one_asset_action_still_records() {
        let pool = db().await;
        let a = action(
            &pool,
            vec![Op::AddTags {
                tag_ids: ids(&["t2"]),
            }],
        )
        .await;
        let summary = run_action(&pool, &a.id, &ids(&["a1"])).await.unwrap();
        assert!(summary.run_id.is_some());
    }

    /// A run from a previous session is history, not an undo offer.
    ///
    /// `action_runs` outlives the process, so without the session bound,
    /// launching Nova and pressing Ctrl+Z would reverse last week's work with
    /// nothing on screen to say what changed.
    #[tokio::test]
    async fn a_run_from_before_this_session_is_not_offered() {
        let pool = db().await;
        sqlx::query(
            "INSERT INTO action_runs (id, action_id, name, ran_at, asset_count, is_undoable)
             VALUES ('old', NULL, 'Last week', '2020-01-01T00:00:00.000Z', 5, 1)",
        )
        .execute(&pool)
        .await
        .unwrap();

        assert!(latest_undoable_run(&pool).await.unwrap().is_none());
        assert!(fetch_recent_runs(&pool).await.unwrap().is_empty());
    }

    /// Ctrl+Z reaches the newest undoable run whatever produced it, so a drag
    /// and a quick action share one history.
    #[tokio::test]
    async fn undo_latest_spans_both_kinds_of_run() {
        let pool = db().await;
        assert!(latest_undoable_run(&pool).await.unwrap().is_none());

        let a = action(
            &pool,
            vec![Op::AddTags {
                tag_ids: ids(&["t2"]),
            }],
        )
        .await;
        run_action(&pool, &a.id, &ids(&["a1", "a2"])).await.unwrap();
        run_steps(
            &pool,
            RunSource::Direct { name: "Add to folder" },
            &[plain(Op::AddToFolder {
                folder_id: "f2".into(),
            })],
            &ids(&["a1", "a2"]),
        )
        .await
        .unwrap();

        // Newest first: the drag, then the action.
        undo_run(&pool, &latest_undoable_run(&pool).await.unwrap().unwrap())
            .await
            .unwrap();
        assert!(members_of(&pool, "f2").await.is_empty());
        assert_eq!(tagged_with(&pool, "t2").await, ids(&["a1", "a2"]));

        undo_run(&pool, &latest_undoable_run(&pool).await.unwrap().unwrap())
            .await
            .unwrap();
        assert!(tagged_with(&pool, "t2").await.is_empty());
        assert!(latest_undoable_run(&pool).await.unwrap().is_none());
    }

    // ── Trash ────────────────────────────────────────────────────────────────

    async fn trashed(pool: &SqlitePool) -> Vec<String> {
        sqlx::query_scalar("SELECT id FROM assets WHERE deleted_at IS NOT NULL ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn trashing_is_undoable() {
        let pool = db().await;
        let a = action(&pool, vec![Op::MoveToTrash]).await;

        let summary = run_action(&pool, &a.id, &ids(&["a1", "a2"])).await.unwrap();
        assert_eq!(trashed(&pool).await, ids(&["a1", "a2"]));

        undo_run(&pool, summary.run_id.as_deref().unwrap()).await.unwrap();
        assert!(trashed(&pool).await.is_empty());
    }

    /// Trashing keeps everything, which is what makes restore exact rather than
    /// approximate — and is the whole reason this is a soft delete.
    #[tokio::test]
    async fn trashing_keeps_folders_and_tags() {
        let pool = db().await;
        let a = action(&pool, vec![Op::MoveToTrash]).await;
        run_action(&pool, &a.id, &ids(&["a1"])).await.unwrap();

        assert_eq!(members_of(&pool, "f1").await.len(), 1);
        assert_eq!(tagged_with(&pool, "t1").await, ids(&["a1"]));

        let restore = action(&pool, vec![Op::RestoreFromTrash]).await;
        run_action(&pool, &restore.id, &ids(&["a1"])).await.unwrap();
        assert!(trashed(&pool).await.is_empty());
        assert_eq!(members_of(&pool, "f1").await[0].1, 7.5, "position was lost");
    }

    /// An asset already in the Trash isn't named by the inverse, so undoing a
    /// trash can't drag it back out.
    #[tokio::test]
    async fn undo_leaves_assets_that_were_already_trashed() {
        let pool = db().await;
        sqlx::query("UPDATE assets SET deleted_at = '2026-01-01T00:00:00.000Z' WHERE id = 'a1'")
            .execute(&pool)
            .await
            .unwrap();

        let a = action(&pool, vec![Op::MoveToTrash]).await;
        let summary = run_action(&pool, &a.id, &ids(&["a1", "a2"])).await.unwrap();
        assert_eq!(trashed(&pool).await, ids(&["a1", "a2"]));

        undo_run(&pool, summary.run_id.as_deref().unwrap()).await.unwrap();
        assert_eq!(trashed(&pool).await, ids(&["a1"]));
    }

    /// A no-op run records no inverse, so it can't be mistaken for a change.
    #[tokio::test]
    async fn trashing_what_is_already_trashed_changes_nothing() {
        let pool = db().await;
        let a = action(&pool, vec![Op::MoveToTrash]).await;
        run_action(&pool, &a.id, &ids(&["a1"])).await.unwrap();
        let second = run_action(&pool, &a.id, &ids(&["a1"])).await.unwrap();

        let payload: String =
            sqlx::query_scalar("SELECT payload_json FROM action_undo WHERE run_id = ? AND seq = 0")
                .bind(second.run_id.as_deref().unwrap())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(payload, "[]");
    }

    // ── Folder auto-tags ─────────────────────────────────────────────────────

    async fn seed(pool: &SqlitePool, folder: &str, tag: &str) {
        sqlx::query("INSERT INTO folder_auto_tags VALUES (?, ?)")
            .bind(folder)
            .bind(tag)
            .execute(pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_folder_seeds_its_tags_onto_arriving_assets() {
        let pool = db().await;
        seed(&pool, "f2", "t2").await;

        let a = action(
            &pool,
            vec![Op::AddToFolder {
                folder_id: "f2".into(),
            }],
        )
        .await;
        run_action(&pool, &a.id, &ids(&["a1", "a2"])).await.unwrap();
        assert_eq!(tagged_with(&pool, "t2").await, ids(&["a1", "a2"]));
    }

    /// The one that would silently break: undo removes the membership AND the
    /// tags that membership caused. An inverse covering only the folder link
    /// would leave the tags stranded.
    #[tokio::test]
    async fn undo_removes_the_tags_the_folder_seeded() {
        let pool = db().await;
        seed(&pool, "f2", "t2").await;

        let a = action(
            &pool,
            vec![Op::AddToFolder {
                folder_id: "f2".into(),
            }],
        )
        .await;
        let summary = run_action(&pool, &a.id, &ids(&["a1", "a2"])).await.unwrap();

        undo_run(&pool, summary.run_id.as_deref().unwrap()).await.unwrap();
        assert!(members_of(&pool, "f2").await.is_empty());
        assert!(tagged_with(&pool, "t2").await.is_empty(), "seeded tags survived the undo");
    }

    /// An asset that already carried the tag keeps it after an undo — the seed
    /// delta is computed exactly like `AddTags`, so undo can't take away
    /// something the folder didn't give.
    #[tokio::test]
    async fn undo_leaves_tags_the_asset_already_had() {
        let pool = db().await;
        seed(&pool, "f2", "t1").await; // a1 already carries t1

        let a = action(
            &pool,
            vec![Op::AddToFolder {
                folder_id: "f2".into(),
            }],
        )
        .await;
        let summary = run_action(&pool, &a.id, &ids(&["a1", "a2"])).await.unwrap();
        assert_eq!(tagged_with(&pool, "t1").await, ids(&["a1", "a2"]));

        undo_run(&pool, summary.run_id.as_deref().unwrap()).await.unwrap();
        assert_eq!(tagged_with(&pool, "t1").await, ids(&["a1"]));
    }

    /// Leaving a folder does NOT take the tag back: by then it's the user's
    /// data, and auto-removal would delete work someone may rely on.
    #[tokio::test]
    async fn leaving_a_folder_keeps_the_seeded_tags() {
        let pool = db().await;
        seed(&pool, "f2", "t2").await;

        let add = action(
            &pool,
            vec![Op::AddToFolder {
                folder_id: "f2".into(),
            }],
        )
        .await;
        run_action(&pool, &add.id, &ids(&["a1"])).await.unwrap();

        let remove = action(
            &pool,
            vec![Op::RemoveFromFolder {
                folder_id: "f2".into(),
            }],
        )
        .await;
        run_action(&pool, &remove.id, &ids(&["a1"])).await.unwrap();

        assert!(members_of(&pool, "f2").await.is_empty());
        assert_eq!(tagged_with(&pool, "t2").await, ids(&["a1"]));
    }

    /// `SetFolders` creates membership too, so it seeds as well — and seeds from
    /// every target, not just the first.
    #[tokio::test]
    async fn set_folders_seeds_from_every_target() {
        let pool = db().await;
        seed(&pool, "f1", "t1").await;
        seed(&pool, "f2", "t2").await;

        let a = action(
            &pool,
            vec![Op::SetFolders {
                folder_ids: ids(&["f1", "f2"]),
            }],
        )
        .await;
        run_action(&pool, &a.id, &ids(&["a2"])).await.unwrap();

        assert!(tagged_with(&pool, "t1").await.contains(&"a2".to_string()));
        assert!(tagged_with(&pool, "t2").await.contains(&"a2".to_string()));
    }

    /// A folder seeds only its OWN tags. No inheritance in v1 — see the
    /// migration for why. This test is the guard against it arriving by accident.
    #[tokio::test]
    async fn a_subfolder_does_not_inherit_its_parents_tags() {
        let pool = db().await;
        sqlx::query("INSERT INTO folders (id, name, parent_id) VALUES ('f3', 'Child', 'f1')")
            .execute(&pool)
            .await
            .unwrap();
        seed(&pool, "f1", "t1").await;

        let a = action(
            &pool,
            vec![Op::AddToFolder {
                folder_id: "f3".into(),
            }],
        )
        .await;
        run_action(&pool, &a.id, &ids(&["a2"])).await.unwrap();
        assert!(!tagged_with(&pool, "t1").await.contains(&"a2".to_string()));
    }

    // ── Conditions ───────────────────────────────────────────────────────────

    fn when(json: &str) -> Option<crate::rules::RuleNode> {
        Some(serde_json::from_str(json).unwrap())
    }

    /// The gate narrows which assets a step touches, without changing what the
    /// step does to the ones it does touch.
    #[tokio::test]
    async fn a_condition_narrows_the_step_to_matching_assets() {
        let pool = db().await;
        sqlx::query("UPDATE assets SET width = 4000 WHERE id = 'a2'")
            .execute(&pool)
            .await
            .unwrap();

        let a = action_with(
            &pool,
            vec![Step {
                op: Op::AddTags {
                    tag_ids: ids(&["t2"]),
                },
                when: when(
                    r#"{"kind":"condition","type":"number","field":"width",
                        "op":"greater_than_or_equal","value":3000}"#,
                ),
            }],
        )
        .await;

        run_action(&pool, &a.id, &ids(&["a1", "a2", "a3"])).await.unwrap();
        assert_eq!(tagged_with(&pool, "t2").await, ids(&["a2"]));
    }

    /// The inverse needed no new machinery for conditions: each step already
    /// records exactly which assets it changed, so a gated step simply records
    /// fewer.
    #[tokio::test]
    async fn undoing_a_conditional_step_only_touches_what_it_changed() {
        let pool = db().await;
        sqlx::query("UPDATE assets SET width = 4000 WHERE id = 'a2'")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO assets_tags VALUES ('a3','t2')")
            .execute(&pool)
            .await
            .unwrap();

        let a = action_with(
            &pool,
            vec![Step {
                op: Op::AddTags {
                    tag_ids: ids(&["t2"]),
                },
                when: when(
                    r#"{"kind":"condition","type":"number","field":"width",
                        "op":"greater_than_or_equal","value":3000}"#,
                ),
            }],
        )
        .await;

        let summary = run_action(&pool, &a.id, &ids(&["a1", "a2", "a3"])).await.unwrap();
        assert_eq!(tagged_with(&pool, "t2").await, ids(&["a2", "a3"]));

        undo_run(&pool, summary.run_id.as_deref().unwrap()).await.unwrap();
        // a3 keeps the tag it had before: it never matched, so the inverse never
        // named it.
        assert_eq!(tagged_with(&pool, "t2").await, ids(&["a3"]));
    }

    /// Conditions are evaluated when the step RUNS, against what earlier steps
    /// left behind. That's what "then" means in a pipeline, and it's the one
    /// semantic here that can surprise.
    #[tokio::test]
    async fn a_condition_sees_earlier_steps() {
        let pool = db().await;
        let a = action_with(
            &pool,
            vec![
                plain(Op::AddTags {
                    tag_ids: ids(&["t2"]),
                }),
                Step {
                    op: Op::SetNote {
                        mode: TextMode::Replace,
                        text: "processed".into(),
                    },
                    when: when(
                        r#"{"kind":"condition","type":"tags","mode":"all",
                            "include":["t2"],"exclude":[],"untagged":false}"#,
                    ),
                },
            ],
        )
        .await;

        run_action(&pool, &a.id, &ids(&["a1"])).await.unwrap();
        // Step 1 gave it t2, so step 2's condition matched — even though it did
        // not match before the run started.
        assert_eq!(note_of(&pool, "a1").await.as_deref(), Some("processed"));
    }

    #[tokio::test]
    async fn a_step_matching_nothing_warns_but_still_runs() {
        let pool = db().await;
        let a = action_with(
            &pool,
            vec![Step {
                op: Op::AddTags {
                    tag_ids: ids(&["t2"]),
                },
                when: when(r#"{"kind":"condition","type":"media_type","types":["audio"]}"#),
            }],
        )
        .await;

        let preview = preview_run(&pool, &a.id, &ids(&["a1"])).await.unwrap();
        assert!(preview.problems.is_empty());
        assert_eq!(preview.warnings.len(), 1, "{:?}", preview.warnings);
        assert!(preview.warnings[0].contains("Step 1"));

        run_action(&pool, &a.id, &ids(&["a1"])).await.unwrap();
        assert!(tagged_with(&pool, "t2").await.is_empty());
    }

    /// A gated rename numbers within the assets it applies to, not within the
    /// whole selection — otherwise the numbers would have gaps for the assets
    /// that were skipped.
    #[tokio::test]
    async fn a_gated_rename_numbers_only_its_own_matches() {
        let pool = db().await;
        sqlx::query("UPDATE assets SET width = 4000 WHERE id IN ('a2','a3')")
            .execute(&pool)
            .await
            .unwrap();

        let a = action_with(
            &pool,
            vec![Step {
                op: rename("Big_{index}", RenameOrder::Filename, true),
                when: when(
                    r#"{"kind":"condition","type":"number","field":"width",
                        "op":"greater_than_or_equal","value":3000}"#,
                ),
            }],
        )
        .await;

        run_action(&pool, &a.id, &ids(&["a1", "a2", "a3"])).await.unwrap();
        // a1 untouched; a3 ("three.png") sorts before a2 ("two.png").
        assert_eq!(
            names(&pool).await,
            ids(&["one.png", "Big_002.png", "Big_001.png"])
        );
    }

    /// The rule editor caps nesting at two, and so does this — an action is
    /// another way to author a tree, not a way around the compiler's limits.
    #[tokio::test]
    async fn an_over_nested_condition_is_rejected_on_save() {
        let pool = db().await;
        let deep = r#"{"kind":"group","op":"all","children":[
                        {"kind":"group","op":"any","children":[
                          {"kind":"group","op":"all","children":[
                            {"kind":"group","op":"any","children":[]}]}]}]}"#;
        let err = create_quick_action(
            &pool,
            QuickActionDraft {
                name: "Too deep".into(),
                icon: None,
                color: None,
                shortcut: None,
                steps: vec![Step {
                    op: Op::ClearAllTags,
                    when: when(deep),
                }],
            },
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("nest"), "got {err}");
    }

    // ── Rename ───────────────────────────────────────────────────────────────

    fn rename(pattern: &str, order: RenameOrder, ascending: bool) -> Op {
        Op::RenameWithPattern {
            pattern: pattern.into(),
            index_order: order,
            index_ascending: ascending,
            index_start: 1,
            index_pad: 3,
            date_field: crate::assets::DateField::ImportedDate,
        }
    }

    async fn names(pool: &SqlitePool) -> Vec<String> {
        sqlx::query_scalar("SELECT filename FROM assets ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    /// The extension comes from the file, not the pattern, and survives a rename
    /// that says nothing about it.
    #[tokio::test]
    async fn renaming_keeps_each_extension() {
        let pool = db().await;
        sqlx::query("UPDATE assets SET extension = 'png' WHERE id = 'a1'")
            .execute(&pool)
            .await
            .unwrap();
        let a = action(&pool, vec![rename("Shot_{index}", RenameOrder::Filename, true)]).await;

        run_action(&pool, &a.id, &ids(&["a1"])).await.unwrap();
        assert_eq!(names(&pool).await[0], "Shot_001.png");
    }

    /// `{index}` follows the order stored in the STEP. Two runs of the same
    /// action must number identically no matter what the view is sorted by.
    #[tokio::test]
    async fn index_follows_the_steps_own_order() {
        let pool = db().await;
        for (id, ext) in [("a1", "png"), ("a2", "png"), ("a3", "png")] {
            sqlx::query("UPDATE assets SET extension = ? WHERE id = ?")
                .bind(ext)
                .bind(id)
                .execute(&pool)
                .await
                .unwrap();
        }

        // one.png, three.png, two.png by name — so descending gives 1=two.
        let a = action(&pool, vec![rename("{index}", RenameOrder::Filename, false)]).await;
        run_action(&pool, &a.id, &ids(&["a1", "a2", "a3"])).await.unwrap();
        assert_eq!(names(&pool).await, ids(&["003.png", "001.png", "002.png"]));
    }

    /// The selection arrives in click order, which must not leak into the
    /// numbering — otherwise the same action gives different results depending
    /// on the order the user happened to click.
    #[tokio::test]
    async fn selection_order_does_not_affect_numbering() {
        let pool = db().await;
        let a = action(&pool, vec![rename("{index}-{name}", RenameOrder::Filename, true)]).await;

        run_action(&pool, &a.id, &ids(&["a3", "a1", "a2"])).await.unwrap();
        let scrambled = names(&pool).await;

        // Put it back and run with the ids in a different order.
        undo_run(&pool, &fetch_recent_runs(&pool).await.unwrap()[0].id)
            .await
            .unwrap();
        run_action(&pool, &a.id, &ids(&["a1", "a2", "a3"])).await.unwrap();
        assert_eq!(names(&pool).await, scrambled);
    }

    #[tokio::test]
    async fn undo_restores_every_previous_filename() {
        let pool = db().await;
        let before = names(&pool).await;
        let a = action(&pool, vec![rename("X_{index}", RenameOrder::Filename, true)]).await;

        let summary = run_action(&pool, &a.id, &ids(&["a1", "a2", "a3"])).await.unwrap();
        assert_ne!(names(&pool).await, before);

        undo_run(&pool, summary.run_id.as_deref().unwrap()).await.unwrap();
        assert_eq!(names(&pool).await, before);
    }

    /// Legal, because Nova has no unique constraint on filenames — but almost
    /// never the intent, so the preview has to say so before the run.
    #[tokio::test]
    async fn colliding_names_warn_without_blocking() {
        let pool = db().await;
        let a = action(&pool, vec![rename("Render", RenameOrder::Filename, true)]).await;

        let preview = preview_run(&pool, &a.id, &ids(&["a1", "a2", "a3"])).await.unwrap();
        assert!(preview.problems.is_empty(), "{:?}", preview.problems);
        assert_eq!(preview.warnings.len(), 1, "{:?}", preview.warnings);
        assert!(preview.warnings[0].contains("2 assets"), "{}", preview.warnings[0]);

        // …and it still runs, because duplicate display names are legal.
        assert!(run_action(&pool, &a.id, &ids(&["a1", "a2", "a3"])).await.is_ok());
    }

    #[tokio::test]
    async fn a_pattern_with_index_does_not_warn() {
        let pool = db().await;
        let a = action(&pool, vec![rename("Render_{index}", RenameOrder::Filename, true)]).await;
        let preview = preview_run(&pool, &a.id, &ids(&["a1", "a2", "a3"])).await.unwrap();
        assert!(preview.warnings.is_empty(), "{:?}", preview.warnings);
    }

    /// A bad pattern BLOCKS, unlike a colliding one.
    #[tokio::test]
    async fn a_bad_pattern_blocks_the_run() {
        let pool = db().await;
        let a = action(&pool, vec![rename("{nope}", RenameOrder::Filename, true)]).await;

        let preview = preview_run(&pool, &a.id, &ids(&["a1"])).await.unwrap();
        assert_eq!(preview.problems.len(), 1);
        assert!(run_action(&pool, &a.id, &ids(&["a1"])).await.is_err());
    }

    /// The preview must render through the same path as the run, or it is
    /// decoration rather than a check.
    #[tokio::test]
    async fn the_preview_matches_what_the_run_produces() {
        let pool = db().await;
        let step = rename("Shot_{index}", RenameOrder::Filename, true);
        let a = action(&pool, vec![step.clone()]).await;
        let selection = ids(&["a1", "a2", "a3"]);

        let preview = preview_rename(&pool, &plain(step.clone()), &selection, 10).await.unwrap();
        assert!(preview.error.is_none());
        let predicted: Vec<String> = preview.rows.iter().map(|r| r.after.clone()).collect();

        run_action(&pool, &a.id, &selection).await.unwrap();
        let actual: Vec<String> =
            sqlx::query_scalar("SELECT filename FROM assets WHERE id IN ('a1','a2','a3')")
                .fetch_all(&pool)
                .await
                .unwrap();
        for name in &predicted {
            assert!(actual.contains(name), "{name} was predicted but not produced");
        }
    }

    /// The preview reads a handful of rows instead of the whole selection, and
    /// must still pick the SAME first few the run would number 1, 2, 3.
    #[tokio::test]
    async fn a_limited_read_matches_the_full_one() {
        let pool = db().await;
        let mut conn = pool.acquire().await.unwrap();
        let all = ids(&["a1", "a2", "a3"]);

        for (order, asc) in [
            (RenameOrder::Filename, true),
            (RenameOrder::Filename, false),
            (RenameOrder::FileSize, true),
        ] {
            let full = rename_rows(&mut conn, &all, order, asc, None).await.unwrap();
            let limited = rename_rows(&mut conn, &all, order, asc, Some(2)).await.unwrap();
            assert_eq!(limited.len(), 2);
            assert_eq!(
                limited.iter().map(|r| &r.id).collect::<Vec<_>>(),
                full.iter().take(2).map(|r| &r.id).collect::<Vec<_>>(),
                "{order:?} ascending={asc}"
            );
        }
    }

    /// The pattern box has to be usable while writing an action, which is
    /// normally before anything is selected.
    #[tokio::test]
    async fn the_preview_falls_back_to_library_samples() {
        let pool = db().await;
        let preview = preview_rename(&pool, &plain(rename("A_{index}", RenameOrder::Filename, true)), &[], 2)
            .await
            .unwrap();
        assert_eq!(preview.rows.len(), 2);
        assert!(preview.rows[0].after.starts_with("A_"));
    }

    /// A half-typed pattern is the normal state of the box, so it comes back as
    /// an inline message rather than a failed command that would toast on every
    /// keystroke.
    #[tokio::test]
    async fn the_preview_reports_a_bad_pattern_inline() {
        let pool = db().await;
        let preview = preview_rename(&pool, &plain(rename("{na", RenameOrder::Filename, true)), &[], 3)
            .await
            .unwrap();
        assert!(preview.rows.is_empty());
        assert!(preview.error.unwrap().contains("Unclosed"));
    }

    /// Repeated ids in a selection must not inflate the count or the inverse.
    #[tokio::test]
    async fn a_duplicated_selection_is_deduplicated() {
        let pool = db().await;
        let a = action(
            &pool,
            vec![Op::AddTags {
                tag_ids: ids(&["t2"]),
            }],
        )
        .await;
        let summary = run_action(&pool, &a.id, &ids(&["a1", "a1", "a2"]))
            .await
            .unwrap();
        assert_eq!(summary.asset_count, 2);
    }
}

/// The pattern language, tested without a database.
///
/// Worth its own module because a pattern is the one step whose *text* the user
/// authors, so it has a whole class of failure the others don't: it can be
/// syntactically wrong, and it can be syntactically fine and produce a name the
/// filesystem won't take.
#[cfg(test)]
mod pattern_tests {
    use super::*;

    fn row(filename: &str, ext: &str) -> RenameRow {
        RenameRow {
            id: "a1".into(),
            filename: filename.into(),
            extension: ext.into(),
            width: 1920,
            height: 1080,
            file_size: 100,
            imported_date: "2026-07-26T10:30:00.000Z".into(),
            creation_date: "2024-01-15T08:00:00.000Z".into(),
            modified_date: "2025-03-03T00:00:00.000Z".into(),
        }
    }

    fn rendered(pattern: &str, row: &RenameRow, index: i64) -> String {
        render(
            &parse_pattern(pattern).unwrap(),
            row,
            index,
            3,
            crate::assets::DateField::ImportedDate,
        )
    }

    #[test]
    fn tokens_render_from_the_asset() {
        let r = row("beach shot.jpg", "jpg");
        assert_eq!(rendered("{name}", &r, 1), "beach shot");
        assert_eq!(rendered("Render_{index}", &r, 7), "Render_007");
        assert_eq!(rendered("{date}", &r, 1), "2026-07-26");
        assert_eq!(rendered("{width}x{height}", &r, 1), "1920x1080");
        assert_eq!(rendered("shot {index} of many", &r, 12), "shot 012 of many");
    }

    /// `{date}` reads the field the step names, not whichever date is handy.
    #[test]
    fn the_date_token_follows_the_chosen_field() {
        let r = row("a.jpg", "jpg");
        let tokens = parse_pattern("{date}").unwrap();
        let at = |f| render(&tokens, &r, 1, 3, f);
        assert_eq!(at(crate::assets::DateField::ImportedDate), "2026-07-26");
        assert_eq!(at(crate::assets::DateField::CreationDate), "2024-01-15");
        assert_eq!(at(crate::assets::DateField::ModifiedDate), "2025-03-03");
    }

    /// The extension is appended from the real file, never taken from the
    /// pattern — a pattern that could set it would let a rename lie about what
    /// the file is.
    #[test]
    fn there_is_no_extension_token() {
        assert!(parse_pattern("{name}.{ext}").is_err());
        assert!(parse_pattern("{extension}").is_err());
    }

    #[test]
    fn stems_survive_odd_filenames() {
        // Case-insensitive suffix match, matching the frontend's `filenameStem`.
        assert_eq!(stem_of("PHOTO.JPG", "jpg"), "PHOTO");
        // Extensionless files keep their whole name rather than losing a char.
        assert_eq!(stem_of("README", ""), "README");
        // A name that merely CONTAINS the extension isn't truncated mid-word.
        assert_eq!(stem_of("jpg-notes.txt", "txt"), "jpg-notes");
    }

    #[test]
    fn braces_can_be_written_literally() {
        let r = row("a.jpg", "jpg");
        assert_eq!(rendered("{{{name}}}", &r, 1), "{a}");
        assert_eq!(rendered("100{{}}", &r, 1), "100{}");
    }

    #[test]
    fn a_malformed_pattern_says_what_is_wrong() {
        let msg = |p: &str| parse_pattern(p).unwrap_err().to_string();
        assert!(msg("{nope}").contains("Unknown token"), "{}", msg("{nope}"));
        assert!(msg("{name").contains("Unclosed"), "{}", msg("{name"));
        assert!(msg("name}").contains("Stray"), "{}", msg("name}"));
        assert!(msg("   ").contains("empty"), "{}", msg("   "));
    }

    /// Filenames are hardlinked under this name for outbound drag, so a pattern
    /// that can't be a filename is rejected while it's being typed rather than
    /// after it has been applied to ten thousand assets.
    #[test]
    fn characters_a_filename_cannot_hold_are_rejected() {
        for bad in ["shot: one", "a/b", "a\\b", "x?", "a|b", "q*", "a<b", "a>b", "a\"b"] {
            assert!(parse_pattern(bad).is_err(), "accepted {bad:?}");
        }
        // Spaces, dots, dashes and unicode are all fine.
        assert!(parse_pattern("Réndér 2026 - final.v2").is_ok());
    }

    #[test]
    fn padding_widens_but_never_truncates() {
        let r = row("a.jpg", "jpg");
        let tokens = parse_pattern("{index}").unwrap();
        let f = crate::assets::DateField::ImportedDate;
        assert_eq!(render(&tokens, &r, 5, 3, f), "005");
        assert_eq!(render(&tokens, &r, 1234, 3, f), "1234");
        assert_eq!(render(&tokens, &r, 5, 0, f), "5");
    }
}

#[cfg(test)]
mod undo_tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    fn plain(op: Op) -> Step {
        Step { op, when: None }
    }

    /// The estimate exists to warn BEFORE a run. It must scale with both axes,
    /// or a wide pipeline over a small selection would look free.
    #[test]
    fn undo_estimate_scales_with_steps_and_assets() {
        let one = vec![plain(Op::AddTags {
            tag_ids: ids(&["t1"]),
        })];
        let two = vec![
            plain(Op::AddTags {
                tag_ids: ids(&["t1", "t2"]),
            }),
            plain(Op::RemoveTags {
                tag_ids: ids(&["t3"]),
            }),
        ];
        assert!(estimated_undo_bytes(&two, 100) > estimated_undo_bytes(&one, 100));
        assert!(estimated_undo_bytes(&one, 1000) > estimated_undo_bytes(&one, 100));
    }

    /// A six-figure selection must still be undoable, or the budget is theatre.
    #[test]
    fn a_ten_thousand_asset_run_stays_within_budget() {
        let steps = vec![plain(Op::AddTags {
            tag_ids: ids(&["t1", "t2"]),
        })];
        assert!(estimated_undo_bytes(&steps, 10_000) <= UNDO_BUDGET_BYTES);
    }

    /// Undo runs against a library that has moved on. A deleted asset is dropped
    /// from the inverse rather than aborting the whole undo on a foreign key.
    #[test]
    fn inverse_drops_assets_that_no_longer_exist() {
        let alive: HashSet<String> = ids(&["a1", "a3"]).into_iter().collect();
        let inverse = Inverse::RemoveTag {
            tag_id: "t1".into(),
            asset_ids: ids(&["a1", "a2", "a3"]),
        };
        match inverse.retaining(&alive) {
            Inverse::RemoveTag { asset_ids, .. } => assert_eq!(asset_ids, ids(&["a1", "a3"])),
            other => panic!("variant changed: {other:?}"),
        }
    }

    #[test]
    fn a_step_with_no_tags_does_nothing() {
        assert!(!Op::AddTags { tag_ids: vec![] }.is_active());
        assert!(Op::AddTags {
            tag_ids: ids(&["t1"])
        }
        .is_active());
    }
}
