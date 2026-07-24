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
use serde::Serialize;
use sqlx::{sqlite::SqlitePool, FromRow, QueryBuilder};
use std::collections::HashMap;
use tracing::instrument;

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
}

/// How many of a selection carry one tag — the raw material for all/some/none.
#[derive(Serialize, Debug, Clone, FromRow)]
pub struct TagUsage {
    pub tag_id: String,
    pub count: i64,
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
                COUNT(at.asset_id) AS usage
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
