//! Tags: named, colorable lenses on assets.
//!
//! A tag is a filter concept, never a scope — applying one narrows the current
//! view through the `FilterSet`, the same as shape or size. Tags apply only to
//! assets; folders have their own membership model. Names are globally unique,
//! case-insensitively, so "Red" and "red" are one tag and create-on-the-fly
//! resolves to whatever already exists.
//!
//! T1 is the flat foundation: CRUD, assign/unassign, and the per-selection counts
//! that drive the inspector's tri-state. Groups, starring, merge, and the filter
//! predicate have their columns here already but are wired up in later phases.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::{sqlite::SqlitePool, FromRow, QueryBuilder, Sqlite};
use std::collections::HashMap;
use tracing::instrument;

/// How selected tags combine when filtering.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TagMatchMode {
    /// At least one of the included tags (OR).
    Any,
    /// Every included tag (AND). The default — the most common intent.
    #[default]
    All,
    /// Exactly the included set and nothing else.
    Equals,
}

/// A tag constraint on the manifest. Ephemeral like every other filter dimension;
/// it's part of `FilterSet`, so it also round-trips into a saved filter.
///
/// `include` and `exclude` hold tag IDS, not names — a rename must not silently
/// change what a saved filter matches. A dangling id (tag later deleted) simply
/// matches nothing, which is the safe direction.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct TagFilter {
    #[serde(default)]
    pub mode: TagMatchMode,
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Match assets with NO tags at all. A pseudo-selection, not a real tag —
    /// "Untagged" is never a row in `tags`.
    #[serde(default)]
    pub untagged: bool,
}

impl TagFilter {
    /// Empty filters add no SQL, so the unfiltered path stays untouched.
    pub fn is_active(&self) -> bool {
        self.untagged || !self.include.is_empty() || !self.exclude.is_empty()
    }

    /// Emit the predicate. The caller has already opened the conjunct (WHERE/AND).
    /// Uses the alias `att` for its subquery table so it never collides with the
    /// outer query's `a`/`af`/`c`.
    ///
    /// `'a` ties the bound tag ids to the builder's data lifetime — binding
    /// `&str` slices of `self` rather than owned strings, so nothing is cloned.
    pub fn push_predicate<'a>(&'a self, qb: &mut QueryBuilder<'a, Sqlite>) {
        let untagged_sql = "NOT EXISTS (SELECT 1 FROM assets_tags att WHERE att.asset_id = a.id)";

        // ── Include side ──────────────────────────────────────────────────────
        qb.push("(");
        if !self.include.is_empty() {
            match self.mode {
                TagMatchMode::Any => {
                    qb.push(
                        "EXISTS (SELECT 1 FROM assets_tags att \
                         WHERE att.asset_id = a.id AND att.tag_id IN (",
                    );
                    let mut sep = qb.separated(", ");
                    for id in &self.include {
                        sep.push_bind(id.as_str());
                    }
                    qb.push("))");
                }
                TagMatchMode::All => push_all_chain(qb, &self.include),
                TagMatchMode::Equals => {
                    // Exact set = right cardinality AND every member present. The
                    // count clause is the whole meaning of "no more, no less".
                    qb.push("(SELECT COUNT(*) FROM assets_tags att WHERE att.asset_id = a.id) = ")
                        .push_bind(self.include.len() as i64)
                        .push(" AND ");
                    push_all_chain(qb, &self.include);
                }
            }
            if self.untagged {
                qb.push(" OR ").push(untagged_sql);
            }
        } else if self.untagged {
            qb.push(untagged_sql);
        } else {
            qb.push("1"); // exclude-only: include side is vacuously true
        }
        qb.push(")");

        // ── Exclude side ──────────────────────────────────────────────────────
        // Skipped under EQUALS: the exact set already excludes everything else, so
        // any exclude clause is redundant (and the UI doesn't offer it there).
        if !self.exclude.is_empty() && self.mode != TagMatchMode::Equals {
            qb.push(
                " AND NOT EXISTS (SELECT 1 FROM assets_tags att \
                 WHERE att.asset_id = a.id AND att.tag_id IN (",
            );
            let mut sep = qb.separated(", ");
            for id in &self.exclude {
                sep.push_bind(id.as_str());
            }
            qb.push("))");
        }
    }
}

/// AND-chain of one EXISTS per tag. Each subquery hits the reverse index on its
/// own tag_id, which SQLite plans better than one GROUP BY … HAVING COUNT.
fn push_all_chain<'a>(qb: &mut QueryBuilder<'a, Sqlite>, ids: &'a [String]) {
    qb.push("(");
    for (i, id) in ids.iter().enumerate() {
        if i > 0 {
            qb.push(" AND ");
        }
        qb.push("EXISTS (SELECT 1 FROM assets_tags att WHERE att.asset_id = a.id AND att.tag_id = ")
            .push_bind(id.as_str())
            .push(")");
    }
    qb.push(")");
}

/// SQLite caps bound parameters at 32766; a selection can be the whole library.
const IDS_PER_QUERY: usize = 8000;

/// A tag with its live usage count. `usage` is computed, not stored, so it can
/// never drift from the actual `assets_tags` rows.
#[derive(Serialize, Debug, Clone, FromRow)]
pub struct Tag {
    pub id: String,
    pub name: String,
    pub color: Option<String>,
    pub group_id: Option<String>,
    pub is_starred: bool,
    pub position: f64,
    pub usage: i64,
    /// Most recent time this tag was applied to any asset (RFC 3339), or `None`
    /// if never used. Powers the "Recently used" suggestions — free here because
    /// the list already aggregates `assets_tags`.
    pub last_used: Option<String>,
}

/// How many of a selection carry one tag — the raw material for all/some/none.
#[derive(Serialize, Debug, Clone, FromRow)]
pub struct TagUsage {
    pub tag_id: String,
    pub count: i64,
}

/// A tag group with how many tags it holds. Pure organization: a tag belongs to
/// at most one, and deleting a group only ungroups its tags (never deletes them).
#[derive(Serialize, Debug, Clone, FromRow)]
pub struct TagGroup {
    pub id: String,
    pub name: String,
    pub color: Option<String>,
    pub position: f64,
    pub tag_count: i64,
}

/// Normalize a user-typed tag name: trim, drop a leading `#` (display sugar),
/// collapse internal whitespace. Returns `None` for anything empty, which the
/// callers turn into a clean error rather than storing a blank tag.
fn normalize_name(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_start_matches('#').trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Deduplicate ids before chunking. Repeats inside one `IN (...)` are harmless,
/// but the same id split across two chunks would be counted twice.
fn unique_ids(ids: &[String]) -> Vec<&String> {
    let mut seen = std::collections::HashSet::with_capacity(ids.len());
    ids.iter().filter(|id| seen.insert(id.as_str())).collect()
}

/// Every tag, alphabetical, each with its usage count. One `GROUP BY`, not a
/// count per tag. A LEFT JOIN so a tag at zero usage still appears — orphan tags
/// are legal and only an explicit delete removes them.
#[instrument(skip(pool))]
pub async fn fetch_tags(pool: &SqlitePool) -> Result<Vec<Tag>> {
    let tags = sqlx::query_as::<_, Tag>(
        "SELECT t.id, t.name, t.color, t.group_id, t.is_starred, t.position,
                COUNT(at.asset_id) AS usage, MAX(at.added_at) AS last_used
         FROM tags t
         LEFT JOIN assets_tags at ON at.tag_id = t.id
         GROUP BY t.id
         ORDER BY t.name COLLATE NOCASE",
    )
    .fetch_all(pool)
    .await
    .context("Failed to fetch tags")?;
    Ok(tags)
}

/// Find a tag by name (case-insensitive), or create it. Returns the tag id.
///
/// This is the create-on-the-fly primitive: typing a name that already exists
/// must reuse it, not collide with the unique index. The lookup-then-insert races
/// against a concurrent create, so the insert is `ON CONFLICT DO NOTHING` and the
/// id is re-read afterwards — whoever won, both callers end up with the same row.
#[instrument(skip(pool))]
pub async fn ensure_tag(pool: &SqlitePool, raw_name: &str) -> Result<String> {
    let name = normalize_name(raw_name).context("Tag name cannot be empty")?;

    if let Some(id) = lookup_tag_id(pool, &name).await? {
        return Ok(id);
    }

    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO tags (id, name) VALUES (?, ?) ON CONFLICT DO NOTHING")
        .bind(&id)
        .bind(&name)
        .execute(pool)
        .await
        .context("Failed to create tag")?;

    // Either our insert landed, or a concurrent one did — re-read to get whichever
    // id is now canonical for this name.
    lookup_tag_id(pool, &name)
        .await?
        .context("Tag vanished immediately after creation")
}

async fn lookup_tag_id(pool: &SqlitePool, name: &str) -> Result<Option<String>> {
    sqlx::query_scalar::<_, String>("SELECT id FROM tags WHERE name = ? COLLATE NOCASE")
        .bind(name)
        .fetch_optional(pool)
        .await
        .context("Failed to look up tag")
}

/// Rename a tag. Because assets link by id, this propagates to every asset for
/// free. Re-checks name uniqueness so "red" can't be renamed onto an existing
/// "Blue".
#[instrument(skip(pool))]
pub async fn rename_tag(pool: &SqlitePool, id: &str, raw_name: &str) -> Result<()> {
    let name = normalize_name(raw_name).context("Tag name cannot be empty")?;

    if let Some(existing) = lookup_tag_id(pool, &name).await? {
        if existing != id {
            anyhow::bail!("A tag named \"{name}\" already exists");
        }
    }

    let res = sqlx::query("UPDATE tags SET name = ? WHERE id = ?")
        .bind(&name)
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to rename tag")?;
    if res.rows_affected() == 0 {
        anyhow::bail!("Tag not found");
    }
    Ok(())
}

/// Delete a tag globally. The `assets_tags` cascade drops every assignment. This
/// is the ONLY thing that removes a tag — unassigning from an asset never does,
/// even at zero usage.
#[instrument(skip(pool))]
pub async fn delete_tag(pool: &SqlitePool, id: &str) -> Result<()> {
    sqlx::query("DELETE FROM tags WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to delete tag")?;
    Ok(())
}

/// Apply a tag to a set of assets. `INSERT OR IGNORE`, so re-applying to an asset
/// that already has it is a silent no-op — which is exactly what the tri-state
/// "apply to all" needs.
#[instrument(skip(pool, asset_ids), fields(assets = asset_ids.len()))]
pub async fn assign_tag(pool: &SqlitePool, tag_id: &str, asset_ids: &[String]) -> Result<()> {
    if asset_ids.is_empty() {
        return Ok(());
    }
    let ids = unique_ids(asset_ids);
    let mut tx = pool.begin().await.context("Failed to begin tag assignment")?;
    for chunk in ids.chunks(IDS_PER_QUERY) {
        let mut qb = QueryBuilder::new("INSERT OR IGNORE INTO assets_tags (asset_id, tag_id) ");
        qb.push_values(chunk, |mut b, id| {
            b.push_bind(*id).push_bind(tag_id);
        });
        qb.build()
            .execute(&mut *tx)
            .await
            .context("Failed to assign tag")?;
    }
    tx.commit().await.context("Failed to commit tag assignment")?;
    Ok(())
}

/// Remove a tag from a set of assets. Never touches the tag row itself.
#[instrument(skip(pool, asset_ids), fields(assets = asset_ids.len()))]
pub async fn unassign_tag(pool: &SqlitePool, tag_id: &str, asset_ids: &[String]) -> Result<()> {
    if asset_ids.is_empty() {
        return Ok(());
    }
    let ids = unique_ids(asset_ids);
    let mut tx = pool.begin().await.context("Failed to begin tag removal")?;
    for chunk in ids.chunks(IDS_PER_QUERY) {
        let mut qb = QueryBuilder::new("DELETE FROM assets_tags WHERE tag_id = ");
        qb.push_bind(tag_id).push(" AND asset_id IN (");
        let mut separated = qb.separated(", ");
        for id in chunk {
            separated.push_bind(*id);
        }
        qb.push(")");
        qb.build()
            .execute(&mut *tx)
            .await
            .context("Failed to remove tag")?;
    }
    tx.commit().await.context("Failed to commit tag removal")?;
    Ok(())
}

/// Per-tag counts across a selection: for each tag, how many of these assets
/// carry it. Tags on none of them are simply absent (the caller reads that as
/// zero → "none"). Count == selection size → "all"; between → "some".
#[instrument(skip(pool, asset_ids), fields(assets = asset_ids.len()))]
pub async fn tag_usage_for_assets(
    pool: &SqlitePool,
    asset_ids: &[String],
) -> Result<Vec<TagUsage>> {
    let ids = unique_ids(asset_ids);
    let mut totals: HashMap<String, i64> = HashMap::new();

    for chunk in ids.chunks(IDS_PER_QUERY) {
        let mut qb = QueryBuilder::new(
            "SELECT tag_id, COUNT(DISTINCT asset_id) FROM assets_tags WHERE asset_id IN (",
        );
        let mut separated = qb.separated(", ");
        for id in chunk {
            separated.push_bind(*id);
        }
        qb.push(") GROUP BY tag_id");

        let rows: Vec<(String, i64)> = qb
            .build_query_as()
            .fetch_all(pool)
            .await
            .context("Failed to read tag usage")?;
        for (tag_id, count) in rows {
            *totals.entry(tag_id).or_insert(0) += count;
        }
    }

    Ok(totals
        .into_iter()
        .map(|(tag_id, count)| TagUsage { tag_id, count })
        .collect())
}

/// Set (or clear, with `None`) a tag's color. Empty strings normalize to `None`
/// so a cleared picker stores NULL rather than "".
#[instrument(skip(pool))]
pub async fn set_tag_color(pool: &SqlitePool, id: &str, color: Option<String>) -> Result<()> {
    let color = color.filter(|c| !c.trim().is_empty());
    let res = sqlx::query("UPDATE tags SET color = ? WHERE id = ?")
        .bind(color)
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to set tag color")?;
    if res.rows_affected() == 0 {
        anyhow::bail!("Tag not found");
    }
    Ok(())
}

/// Star or unstar a tag (the manager's "Starred" pin).
#[instrument(skip(pool))]
pub async fn set_tag_starred(pool: &SqlitePool, id: &str, starred: bool) -> Result<()> {
    let res = sqlx::query("UPDATE tags SET is_starred = ? WHERE id = ?")
        .bind(starred)
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to star tag")?;
    if res.rows_affected() == 0 {
        anyhow::bail!("Tag not found");
    }
    Ok(())
}

/// Move a tag into a group, or out of one with `None`. A bad `group_id` is a
/// no-op the FK would reject, so it's validated by existence implicitly.
#[instrument(skip(pool))]
pub async fn set_tag_group(pool: &SqlitePool, id: &str, group_id: Option<String>) -> Result<()> {
    let res = sqlx::query("UPDATE tags SET group_id = ? WHERE id = ?")
        .bind(group_id)
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to move tag")?;
    if res.rows_affected() == 0 {
        anyhow::bail!("Tag not found");
    }
    Ok(())
}

/// Merge `source` into `target`: every asset tagged `source` gains `target`
/// (skipping any it already has), then `source` is deleted. One transaction so a
/// failure can't leave assets tagged with a tag that no longer exists.
///
/// Irreversible — the caller confirms first. `INSERT OR IGNORE` is what handles
/// assets that already carry both tags; without it the composite PK would abort
/// the merge on the first such asset.
#[instrument(skip(pool))]
pub async fn merge_tags(pool: &SqlitePool, source: &str, target: &str) -> Result<()> {
    if source == target {
        anyhow::bail!("Cannot merge a tag into itself");
    }
    let mut tx = pool.begin().await.context("Failed to begin tag merge")?;

    sqlx::query(
        "INSERT OR IGNORE INTO assets_tags (asset_id, tag_id)
         SELECT asset_id, ? FROM assets_tags WHERE tag_id = ?",
    )
    .bind(target)
    .bind(source)
    .execute(&mut *tx)
    .await
    .context("Failed to reassign assets during merge")?;

    // Deleting the source cascades its now-redundant assets_tags rows.
    let res = sqlx::query("DELETE FROM tags WHERE id = ?")
        .bind(source)
        .execute(&mut *tx)
        .await
        .context("Failed to delete merged tag")?;
    if res.rows_affected() == 0 {
        anyhow::bail!("Source tag not found");
    }

    tx.commit().await.context("Failed to commit tag merge")?;
    Ok(())
}

// ── Tag groups ────────────────────────────────────────────────────────────────

/// Trim a group name and reject an empty result.
fn clean_group_name(raw: &str) -> Result<String> {
    let name = raw.trim();
    if name.is_empty() {
        anyhow::bail!("Group name cannot be empty");
    }
    Ok(name.to_string())
}

/// Every group with its tag count, ordered by position then name.
#[instrument(skip(pool))]
pub async fn fetch_tag_groups(pool: &SqlitePool) -> Result<Vec<TagGroup>> {
    let groups = sqlx::query_as::<_, TagGroup>(
        "SELECT g.id, g.name, g.color, g.position, COUNT(t.id) AS tag_count
         FROM tag_groups g
         LEFT JOIN tags t ON t.group_id = g.id
         GROUP BY g.id
         ORDER BY g.position, g.name COLLATE NOCASE",
    )
    .fetch_all(pool)
    .await
    .context("Failed to fetch tag groups")?;
    Ok(groups)
}

#[instrument(skip(pool))]
pub async fn create_tag_group(pool: &SqlitePool, raw_name: &str) -> Result<String> {
    let name = clean_group_name(raw_name)?;
    let id = uuid::Uuid::new_v4().to_string();
    let position: f64 = sqlx::query_scalar::<_, Option<f64>>("SELECT MAX(position) FROM tag_groups")
        .fetch_one(pool)
        .await
        .context("Failed to compute group position")?
        .map(|m| m + 1.0)
        .unwrap_or(0.0);

    sqlx::query("INSERT INTO tag_groups (id, name, position) VALUES (?, ?, ?)")
        .bind(&id)
        .bind(&name)
        .bind(position)
        .execute(pool)
        .await
        .context("Failed to create tag group")?;
    Ok(id)
}

#[instrument(skip(pool))]
pub async fn rename_tag_group(pool: &SqlitePool, id: &str, raw_name: &str) -> Result<()> {
    let name = clean_group_name(raw_name)?;
    let res = sqlx::query("UPDATE tag_groups SET name = ? WHERE id = ?")
        .bind(&name)
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to rename tag group")?;
    if res.rows_affected() == 0 {
        anyhow::bail!("Group not found");
    }
    Ok(())
}

#[instrument(skip(pool))]
pub async fn set_tag_group_color(pool: &SqlitePool, id: &str, color: Option<String>) -> Result<()> {
    let color = color.filter(|c| !c.trim().is_empty());
    let res = sqlx::query("UPDATE tag_groups SET color = ? WHERE id = ?")
        .bind(color)
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to set group color")?;
    if res.rows_affected() == 0 {
        anyhow::bail!("Group not found");
    }
    Ok(())
}

/// Delete a group. The `tags.group_id` FK is `ON DELETE SET NULL`, so its tags
/// survive and become ungrouped — never deleted.
#[instrument(skip(pool))]
pub async fn delete_tag_group(pool: &SqlitePool, id: &str) -> Result<()> {
    sqlx::query("DELETE FROM tag_groups WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to delete tag group")?;
    Ok(())
}
