//! Full-text search index maintenance.
//!
//! `search_index` (see the migration) is a denormalised FTS5 table — one row per
//! asset, every searchable field flattened in. It's a DERIVED CACHE: nothing but
//! this module writes it, and it can always be rebuilt from the source tables.
//!
//! The sync strategy (the "Option A" decision) rests on one fact about this
//! codebase: every mutation to searchable text flows through a known Rust
//! function. So each of those calls `reindex_assets` with the affected ids —
//! a finite, enumerable set of choke points rather than "hope we caught them
//! all". The fan-out cases (folder/tag rename, deletion) resolve their affected
//! asset set through the helpers here first.

use anyhow::{Context, Result};
use sqlx::{QueryBuilder, Sqlite, SqlitePool};
use tracing::instrument;

/// Mirror of `assets.rs` — keep multi-row statements under SQLite's 32766 bind cap.
const IDS_PER_QUERY: usize = 8000;

/// The denormalisation query: one row per asset with all searchable text. Ends
/// at `FROM assets a` so callers can append a `WHERE` (or not, to index all).
///
/// Folder/tag text are correlated subqueries rather than joins so an asset in
/// many folders/tags still yields exactly one row. `COALESCE(..,'')` because
/// FTS5 columns are text and a NULL note/url would otherwise poison the row.
const INDEX_SELECT: &str = "SELECT a.id, a.filename, a.extension, \
     COALESCE(a.notes, ''), COALESCE(a.source_url, ''), \
     COALESCE((SELECT group_concat(f.name, ' ') FROM assets_folders af \
               JOIN folders f ON f.id = af.folder_id WHERE af.asset_id = a.id), ''), \
     COALESCE((SELECT group_concat(f.notes, ' ') FROM assets_folders af \
               JOIN folders f ON f.id = af.folder_id WHERE af.asset_id = a.id), ''), \
     COALESCE((SELECT group_concat(t.name, ' ') FROM assets_tags at \
               JOIN tags t ON t.id = at.tag_id WHERE at.asset_id = a.id), '') \
     FROM assets a";

const INSERT_COLS: &str =
    "INSERT INTO search_index (asset_id, name, extension, note, url, folder_text, folder_note, tag_text) ";

/// Re-derive the search rows for `ids`: delete the stale rows, re-insert from
/// current state. Idempotent, so calling it after a no-op mutation is harmless.
///
/// Runs AFTER the triggering mutation has committed, in its own transaction, so
/// it reads post-mutation state. A crash in the gap leaves at most a few stale
/// rows, fixable by `rebuild_search_index` — never corruption.
#[instrument(skip(pool, ids), fields(count = ids.len()))]
pub async fn reindex_assets(pool: &SqlitePool, ids: &[String]) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }

    let mut tx = pool
        .begin()
        .await
        .context("Failed to begin reindex transaction")?;

    for chunk in ids.chunks(IDS_PER_QUERY) {
        let mut del = QueryBuilder::<Sqlite>::new("DELETE FROM search_index WHERE asset_id IN (");
        push_id_list(&mut del, chunk);
        del.push(")");
        del.build()
            .execute(&mut *tx)
            .await
            .context("Failed to clear stale search rows")?;

        let mut ins = QueryBuilder::<Sqlite>::new(INSERT_COLS);
        ins.push(INDEX_SELECT);
        ins.push(" WHERE a.id IN (");
        push_id_list(&mut ins, chunk);
        ins.push(")");
        ins.build()
            .execute(&mut *tx)
            .await
            .context("Failed to write search rows")?;
    }

    tx.commit()
        .await
        .context("Failed to commit reindex")?;
    Ok(())
}

/// Rebuild the whole index from scratch. The initial backfill and the recovery
/// path if the index ever drifts. One DELETE + one INSERT…SELECT over every
/// asset — the correlated subqueries make it O(assets), fine at library scale.
#[instrument(skip(pool))]
pub async fn rebuild_search_index(pool: &SqlitePool) -> Result<()> {
    let mut tx = pool
        .begin()
        .await
        .context("Failed to begin index rebuild")?;

    sqlx::query("DELETE FROM search_index")
        .execute(&mut *tx)
        .await
        .context("Failed to clear search index")?;

    sqlx::query(&format!("{INSERT_COLS}{INDEX_SELECT}"))
        .execute(&mut *tx)
        .await
        .context("Failed to populate search index")?;

    tx.commit()
        .await
        .context("Failed to commit index rebuild")?;
    Ok(())
}

/// Backfill the index if it's empty but the library has assets — i.e. an
/// existing library that just gained the `search_index` table on migration. A
/// cheap two-count check on every connect; the rebuild only runs once.
#[instrument(skip(pool))]
pub async fn ensure_indexed(pool: &SqlitePool) -> Result<()> {
    let indexed: i64 = sqlx::query_scalar("SELECT count(*) FROM search_index")
        .fetch_one(pool)
        .await
        .context("Failed to count search rows")?;
    if indexed > 0 {
        return Ok(());
    }
    let assets: i64 = sqlx::query_scalar("SELECT count(*) FROM assets")
        .fetch_one(pool)
        .await
        .context("Failed to count assets")?;
    if assets > 0 {
        tracing::info!(assets, "Search index empty — backfilling");
        rebuild_search_index(pool).await?;
    }
    Ok(())
}

// ── Affected-id helpers for the fan-out cases ────────────────────────────────

/// Direct members of `folder_id`. Matches the DIRECT-only `folder_text` rule:
/// renaming a folder changes the searchable text of exactly its own members.
#[instrument(skip(pool))]
pub async fn asset_ids_in_folder(pool: &SqlitePool, folder_id: &str) -> Result<Vec<String>> {
    sqlx::query_scalar("SELECT asset_id FROM assets_folders WHERE folder_id = ?")
        .bind(folder_id)
        .fetch_all(pool)
        .await
        .context("Failed to list folder members")
}

/// Every asset that is a direct member of any of `folder_ids` OR of one of their
/// descendants — the set whose `folder_text` a group-delete invalidates. The
/// recursive CTE walks the subtree; `DISTINCT` because an asset can sit in two
/// doomed folders at once.
#[instrument(skip(pool, folder_ids), fields(count = folder_ids.len()))]
pub async fn asset_ids_under_folders(
    pool: &SqlitePool,
    folder_ids: &[String],
) -> Result<Vec<String>> {
    if folder_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for chunk in folder_ids.chunks(IDS_PER_QUERY) {
        let mut qb = QueryBuilder::<Sqlite>::new(
            "WITH RECURSIVE doomed(id) AS ( \
                 SELECT id FROM folders WHERE id IN (",
        );
        push_id_list(&mut qb, chunk);
        qb.push(
            ") UNION \
                 SELECT f.id FROM folders f JOIN doomed d ON f.parent_id = d.id \
             ) \
             SELECT DISTINCT af.asset_id FROM assets_folders af \
             JOIN doomed ON doomed.id = af.folder_id",
        );
        let mut part: Vec<String> = qb
            .build_query_scalar()
            .fetch_all(pool)
            .await
            .context("Failed to gather assets under folders")?;
        out.append(&mut part);
    }
    Ok(out)
}

/// Every asset carrying `tag_id` — the set a tag rename/merge/delete touches.
#[instrument(skip(pool))]
pub async fn asset_ids_with_tag(pool: &SqlitePool, tag_id: &str) -> Result<Vec<String>> {
    sqlx::query_scalar("SELECT asset_id FROM assets_tags WHERE tag_id = ?")
        .bind(tag_id)
        .fetch_all(pool)
        .await
        .context("Failed to list tagged assets")
}

fn push_id_list<'a>(qb: &mut QueryBuilder<'a, Sqlite>, ids: &'a [String]) {
    let mut sep = qb.separated(", ");
    for id in ids {
        sep.push_bind(id);
    }
}

// ── Query parsing ────────────────────────────────────────────────────────────
//
// User text → a safe FTS5 MATCH expression. Two rules govern the whole design:
//
//   * We NEVER pass raw input to MATCH. FTS5 has its own syntax (quotes, parens,
//     `*`, `.`, column filters); stray input breaks it or is an injection into
//     the match expression. Every term is emitted as a properly QUOTED phrase,
//     which neutralises all of that — a bare `.` is a syntax error otherwise
//     (the fts5 probe proved it).
//   * These functions are PURE — string in, `Compiled` out, no DB, no I/O — so
//     the fiddly operator handling lives in one testable place.
//
// Trigram matches substrings but needs >= 3 characters, so shorter terms are
// dropped from the FTS query (the frontend's name-in-manifest hybrid covers
// short name lookups anyway).
pub mod query {

use serde::{Deserialize, Serialize};

/// Shortest term the trigram tokenizer can match.
const MIN_TERM_LEN: usize = 3;

/// Which columns a search looks in — the seven scope toggles from the UI, mapped
/// to `search_index` columns. All on by default: an unconfigured search covers
/// everything, and the user narrows from there.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SearchScopes {
    pub name: bool,
    pub extension: bool,
    pub note: bool,
    pub url: bool,
    pub folder_name: bool,
    pub folder_note: bool,
    pub tags: bool,
}

impl Default for SearchScopes {
    fn default() -> Self {
        Self {
            name: true,
            extension: true,
            note: true,
            url: true,
            folder_name: true,
            folder_note: true,
            tags: true,
        }
    }
}

impl SearchScopes {
    /// The active columns as their `search_index` names. Empty when every scope
    /// is off — which `compile_query` turns into `Empty` (nothing can match).
    pub fn columns(&self) -> Vec<&'static str> {
        // (toggle, column) — the column names must match the migration exactly.
        [
            (self.name, "name"),
            (self.extension, "extension"),
            (self.note, "note"),
            (self.url, "url"),
            (self.folder_name, "folder_text"),
            (self.folder_note, "folder_note"),
            (self.tags, "tag_text"),
        ]
        .into_iter()
        .filter_map(|(on, col)| on.then_some(col))
        .collect()
    }
}

/// A live text search: the query string plus the active scopes. Ephemeral —
/// stripped before a filter is SAVED (search is typed, not stored), so it never
/// reaches the saved-filter JSON.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct TextSearch {
    pub query: String,
    #[serde(default)]
    pub scopes: SearchScopes,
}

impl TextSearch {
    /// Nothing to search: blank/whitespace query.
    pub fn is_blank(&self) -> bool {
        self.query.trim().is_empty()
    }

    /// Compile against the active scopes.
    pub fn compile(&self) -> Compiled {
        compile_query(&self.query, &self.scopes.columns())
    }
}

/// A user query compiled against the active scope columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Compiled {
    /// Nothing searchable (blank input, all-too-short terms, or no active scope)
    /// — apply no search filter at all.
    Empty,
    /// Assets matching this FTS5 MATCH expression are INCLUDED. The normal case.
    Include(String),
    /// An exclusion-ONLY query (e.g. `-png`). FTS5 can't express a purely
    /// negative MATCH, so we hand back the POSITIVE form of the excluded terms
    /// and the caller applies it with `NOT IN`.
    Exclude(String),
}

/// One parsed token.
#[derive(Debug, PartialEq, Eq)]
enum Tok {
    /// A word or quoted phrase, possibly negated with a leading dash.
    Term { text: String, negated: bool },
    /// The bare uppercase `OR` operator between two terms.
    Or,
}

/// Split input into terms, honouring quotes, the leading-dash exclusion, and the
/// `OR` operator — the rules from the spec:
///   * `-term` (dash then a non-space) → exclusion; an INTERNAL dash (`meta-data`)
///     is just part of the word.
///   * `"quoted phrase"` → a literal phrase, kept verbatim (spaces, dashes, dots).
///   * bare uppercase `OR` → the OR operator (lowercase `or` is a literal word).
fn tokenize(input: &str) -> Vec<Tok> {
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    let n = chars.len();
    let mut out = Vec::new();

    while i < n {
        // Skip whitespace between tokens.
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }

        // A leading dash is exclusion ONLY when a non-space follows it; a dash at
        // the end or before a space is just a stray character, not an operator.
        let negated = chars[i] == '-' && i + 1 < n && !chars[i + 1].is_whitespace();
        if negated {
            i += 1;
        }

        let text = if i < n && chars[i] == '"' {
            // Quoted phrase: read to the closing quote (or end if unterminated).
            i += 1;
            let start = i;
            while i < n && chars[i] != '"' {
                i += 1;
            }
            let phrase: String = chars[start..i].iter().collect();
            if i < n {
                i += 1; // consume closing quote
            }
            phrase
        } else {
            // Bare word: read to the next whitespace. Internal dashes stay.
            let start = i;
            while i < n && !chars[i].is_whitespace() {
                i += 1;
            }
            chars[start..i].iter().collect()
        };

        // Bare uppercase OR (not negated, not quoted-empty) is the operator.
        if !negated && text == "OR" {
            out.push(Tok::Or);
        } else if !text.trim().is_empty() {
            out.push(Tok::Term { text, negated });
        }
    }

    out
}

/// FTS5 phrase literal: wrap in double quotes, doubling any embedded quote. This
/// is what makes arbitrary user text safe inside a MATCH expression.
fn quote(term: &str) -> String {
    format!("\"{}\"", term.replace('"', "\"\""))
}

/// Too short for the trigram tokenizer to match.
fn too_short(term: &str) -> bool {
    term.chars().count() < MIN_TERM_LEN
}

/// A MATCH expression restricted to ONE column, for a rule's `contains`.
///
/// `None` when the term is shorter than the trigram tokenizer can index — the
/// caller must fall back to `LIKE`, because an under-length term doesn't match
/// nothing, it can't be *asked* here at all.
///
/// Separate from `compile_query` on purpose: that one parses user search syntax
/// (`-`, `OR`, phrases) across the active scope columns. A rule condition has
/// already decided its column and operator, so its needle is one literal phrase.
pub fn column_phrase(column: &str, term: &str) -> Option<String> {
    let term = term.trim();
    (!too_short(term)).then(|| format!("{{{column}}} : {}", quote(term)))
}

/// Compile user input into a `Compiled` against the active scope `columns`
/// (the FTS5 column names — `name`, `note`, `folder_text`, …).
///
/// No active columns → `Empty`: the user turned every scope off, so nothing can
/// match.
pub fn compile_query(input: &str, columns: &[&str]) -> Compiled {
    if columns.is_empty() {
        return Compiled::Empty;
    }

    // `{name note} : ` — restricts each following phrase to the active columns.
    let col_prefix = format!("{{{}}} : ", columns.join(" "));
    let frag = |term: &str| format!("{col_prefix}{}", quote(term));

    let toks = tokenize(input);

    // Positives keep their inter-term connector (AND default, OR where written);
    // negatives are collected flat and excluded from the whole.
    let mut positive = String::new();
    let mut positive_count = 0usize;
    let mut pending_or = false;
    let mut negatives: Vec<String> = Vec::new();

    for tok in &toks {
        match tok {
            Tok::Or => {
                // Only meaningful between two positive terms; ignored otherwise.
                if positive_count > 0 {
                    pending_or = true;
                }
            }
            Tok::Term { text, negated } => {
                if too_short(text) {
                    continue;
                }
                if *negated {
                    negatives.push(frag(text));
                } else {
                    if positive_count > 0 {
                        positive.push_str(if pending_or { " OR " } else { " AND " });
                    }
                    positive.push_str(&frag(text));
                    positive_count += 1;
                    pending_or = false;
                }
            }
        }
    }

    match (positive_count, negatives.is_empty()) {
        // Nothing usable at all.
        (0, true) => Compiled::Empty,
        // Exclusion-only: hand back the excluded terms OR'd together; the caller
        // turns this into `NOT IN (… MATCH <expr>)`.
        (0, false) => Compiled::Exclude(negatives.join(" OR ")),
        // The normal case: positives, minus any exclusions. Parenthesise the
        // positive group so a trailing NOT applies to ALL of it, not just the
        // last OR-branch (FTS5 binds NOT tighter than OR).
        _ => {
            let mut expr = if positive_count > 1 {
                format!("({positive})")
            } else {
                positive
            };
            for neg in &negatives {
                expr.push_str(" NOT ");
                expr.push_str(neg);
            }
            Compiled::Include(expr)
        }
    }
}

} // mod query
// Pure DB logic — the reindex denormalisation, its idempotency, and the
// descendant closure. Re-enabled alongside the rule compiler tests: between
// them they cover every path that can silently return the WRONG assets rather
// than fail loudly.
#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal slice of the real schema, enough to exercise the denormalisation.
    async fn schema() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        for stmt in [
            "CREATE TABLE assets (id TEXT PRIMARY KEY, filename TEXT, extension TEXT, notes TEXT, source_url TEXT)",
            // `notes` here because INDEX_SELECT flattens folder notes too — this
            // fixture must track the real migration's columns, not a subset that
            // happened to be enough when it was written.
            "CREATE TABLE folders (id TEXT PRIMARY KEY, name TEXT, parent_id TEXT, notes TEXT)",
            "CREATE TABLE assets_folders (folder_id TEXT, asset_id TEXT)",
            "CREATE TABLE tags (id TEXT PRIMARY KEY, name TEXT)",
            "CREATE TABLE assets_tags (asset_id TEXT, tag_id TEXT)",
            "CREATE VIRTUAL TABLE search_index USING fts5(asset_id UNINDEXED, name, extension, note, url, folder_text, folder_note, tag_text, tokenize='trigram')",
        ] {
            sqlx::query(stmt).execute(&pool).await.unwrap();
        }
        pool
    }

    async fn match_ids(pool: &SqlitePool, expr: &str) -> Vec<String> {
        sqlx::query_scalar("SELECT asset_id FROM search_index WHERE search_index MATCH ? ORDER BY asset_id")
            .bind(expr)
            .fetch_all(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn reindex_flattens_folders_and_tags() {
        let pool = schema().await;
        sqlx::query("INSERT INTO assets VALUES ('a1','Sunset.png','png','a nice note',NULL)")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO folders (id, name, parent_id) VALUES ('f1','Landscapes',NULL)").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO assets_folders VALUES ('f1','a1')").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO tags VALUES ('t1','vector')").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO assets_tags VALUES ('a1','t1')").execute(&pool).await.unwrap();

        reindex_assets(&pool, &["a1".into()]).await.unwrap();

        // Substring hits across the flattened columns.
        assert_eq!(match_ids(&pool, "suns").await, vec!["a1"]); // name
        assert_eq!(match_ids(&pool, "andsca").await, vec!["a1"]); // folder_text
        assert_eq!(match_ids(&pool, "vect").await, vec!["a1"]); // tag_text
        // Column-scoped: a folder term must not match when scoped to name.
        assert!(match_ids(&pool, "{name} : andsca").await.is_empty());
    }

    #[tokio::test]
    async fn reindex_is_idempotent_and_reflects_changes() {
        let pool = schema().await;
        sqlx::query("INSERT INTO assets VALUES ('a1','old.png','png',NULL,NULL)")
            .execute(&pool).await.unwrap();
        reindex_assets(&pool, &["a1".into()]).await.unwrap();
        reindex_assets(&pool, &["a1".into()]).await.unwrap(); // twice → still one row

        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM search_index")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(rows, 1, "reindex must not duplicate rows");

        // Rename, reindex, and the old name is gone from the index.
        sqlx::query("UPDATE assets SET filename = 'brandnew.png' WHERE id='a1'")
            .execute(&pool).await.unwrap();
        reindex_assets(&pool, &["a1".into()]).await.unwrap();
        assert!(match_ids(&pool, "brandn").await == vec!["a1"]);
        // Quoted phrase — a bare '.' is FTS5 syntax, so terms with punctuation
        // must be quoted (a rule the S2 parser will enforce).
        assert!(match_ids(&pool, "\"old.png\"").await.is_empty());
    }

    #[tokio::test]
    async fn descendant_closure_finds_nested_members() {
        let pool = schema().await;
        // parent > child, an asset only in the child.
        sqlx::query("INSERT INTO folders (id, name, parent_id) VALUES ('p','Parent',NULL)").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO folders (id, name, parent_id) VALUES ('c','Child','p')").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO assets VALUES ('a1','x.png','png',NULL,NULL)").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO assets_folders VALUES ('c','a1')").execute(&pool).await.unwrap();

        // Deleting the PARENT must surface the asset in the child for reindex.
        let ids = asset_ids_under_folders(&pool, &["p".into()]).await.unwrap();
        assert_eq!(ids, vec!["a1"]);
    }
}
