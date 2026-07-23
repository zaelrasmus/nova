use crate::extract;
use crate::fs;
use crate::thumbnail;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rayon::prelude::{IntoParallelIterator, ParallelIterator};
use serde::{Deserialize, Serialize};
use sqlx::{sqlite::SqlitePool, FromRow, QueryBuilder, Sqlite, Type};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use tokio::sync::Semaphore;
use tracing::{debug, info, instrument, warn};

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Type)]
#[sqlx(rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum AssetType {
    Image,
    Audio,
    Video,
    Unknown,
}

/// What slice of the library the manifest covers. `All` = everything, `Folder` =
/// one folder's members, `Uncategorized` = assets with no folder membership.
/// Sent from the frontend as `{ "kind": "folder", "id": … }`.
///
/// A scope is a *place*, not a filter: it decides which rows exist, and it owns
/// the persisted sort for that place. Filters (`FilterSet`) narrow a scope; they
/// never replace one.
#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Scope {
    All,
    Folder { id: String },
    Uncategorized,
}

/// A sort criterion plus direction, persisted per scope (see `resolve_sort`).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sort {
    pub order_by: OrderBy,
    pub is_ascending: bool,
}

/// Shape of an asset, derived from its stored dimensions.
///
/// Named "shape" rather than "orientation" on purpose: `Ultrawide` and `Ratio`
/// describe *proportion*, not which way the image is turned, so "orientation"
/// would only be accurate for the first three variants.
///
/// The broad variants overlap by design — an ultrawide image is also horizontal,
/// and a 16:9 image is both. That's what the geometry says, so the filters say it
/// too rather than carving out artificial exclusive bands.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Shape {
    Horizontal,
    Vertical,
    Square,
    /// At least twice as wide as tall.
    Ultrawide,
    /// At least twice as tall as wide — the vertical counterpart of `Ultrawide`.
    PanoramicVertical,
    /// Aspect ratio `num:den`, matched within `tolerance` (in ratio units, so
    /// 0.02 on 16:9 accepts 1.758–1.798).
    ///
    /// One variant serves BOTH the fixed presets in the UI and a user-entered
    /// custom ratio — they differ only in where the numbers come from, so there's
    /// no reason for them to be different code paths.
    Ratio { num: f64, den: f64, tolerance: f64 },
}

impl Shape {
    /// Append this shape's comparison. Callers must AND it with the non-zero
    /// dimension guard (see `DIMENSIONED`).
    ///
    /// Nothing here divides: the ratio test multiplies both sides by `height *
    /// den`, so there is no divide-by-zero and no float equality in any predicate.
    fn push_predicate(self, qb: &mut QueryBuilder<'_, Sqlite>) {
        match self {
            Shape::Horizontal => {
                qb.push("a.width > a.height");
            }
            Shape::Vertical => {
                qb.push("a.height > a.width");
            }
            Shape::Square => {
                qb.push("a.width = a.height");
            }
            Shape::Ultrawide => {
                qb.push("a.width >= a.height * 2");
            }
            Shape::PanoramicVertical => {
                qb.push("a.height >= a.width * 2");
            }
            // |w/h − num/den| ≤ tolerance, with both sides multiplied by h * den.
            Shape::Ratio {
                num,
                den,
                tolerance,
            } => {
                // The UI can't produce these, but a malformed IPC payload must not
                // yield a nonsense predicate. `0` matches nothing, which surfaces
                // as the (recoverable) "no assets match these filters" state.
                if !(num.is_finite() && den.is_finite() && num > 0.0 && den > 0.0) {
                    qb.push("0");
                    return;
                }
                qb.push("ABS(a.width * ")
                    .push_bind(den)
                    .push(" - a.height * ")
                    .push_bind(num)
                    .push(") <= ")
                    .push_bind(tolerance.max(0.0))
                    .push(" * a.height * ")
                    .push_bind(den);
            }
        }
    }
}

/// Guard that must precede every shape predicate. `width`/`height` default to 0,
/// so audio (and anything whose dimensions failed to extract) is 0×0 — without
/// this, `width = height` would report every audio file as "square". Excluding
/// them is also the correct answer: a sound file has no shape.
const DIMENSIONED: &str = "a.width > 0 AND a.height > 0";

/// Ephemeral, multi-dimensional narrowing of a scope. Deliberately NOT persisted:
/// a filter that survives a restart is the classic "my library is empty, the app
/// is broken" bug. Saved filter sets are an explicit, named feature (Phase 4).
///
/// Every dimension is optional and they AND together. An empty `FilterSet` adds
/// no SQL at all, so the unfiltered path stays exactly as fast as before.
#[derive(Deserialize, Debug, Clone, Default)]
pub struct FilterSet {
    /// Types to include. Empty = no constraint. Multiple types OR together.
    #[serde(default)]
    pub asset_types: Vec<AssetType>,
    /// Shape constraint; `None` = any.
    #[serde(default)]
    pub shape: Option<Shape>,
}

/// Everything the manifest needs, as ONE object, compiled into ONE statement.
/// Keeping scope, filters and sort together is what prevents a SQL pass and a JS
/// pass that can disagree about what the view contains.
#[derive(Deserialize, Debug, Clone)]
pub struct ManifestQuery {
    pub scope: Scope,
    #[serde(default)]
    pub filters: FilterSet,
    /// Explicit override; `None` falls back to the scope's persisted sort.
    #[serde(default)]
    pub sort: Option<Sort>,
}

/// Newest-first — matches the behaviour from before sorting existed, and is the
/// fallback when a scope has no persisted row (deleted folder, un-seeded view).
const DEFAULT_SORT: Sort = Sort {
    order_by: OrderBy::ImportedDate,
    is_ascending: false,
};

#[derive(Serialize, Clone, Debug, FromRow)]
pub struct AssetLightRow {
    pub id: String,
    pub width: u32,
    pub height: u32,
    pub asset_type: AssetType,
    pub thumb_hash: Option<String>,
    pub is_animated: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, FromRow)]
pub struct AssetMetadata {
    pub id: String,
    pub asset_type: AssetType,
    pub filename: String,
    pub extension: String,

    #[sqlx(rename = "path")]
    pub dest_path: String,

    #[serde(skip)]
    #[sqlx(skip)]
    pub source_path: String,

    pub width: u32,
    pub height: u32,

    /// Bytes on disk. `i64` because SQLite integers are signed — sqlx has no
    /// `Encode` for `u64`.
    pub file_size: i64,

    pub imported_date: String,
    #[sqlx(rename = "creation_date")]
    pub creation_date: String,
    #[sqlx(rename = "modified_date")]
    pub modified_date: String,

    #[sqlx(default)]
    pub thumb_hash: Option<String>,

    #[serde(skip)]
    #[sqlx(default)]
    pub thumb_config: Option<String>,

    #[sqlx(default)]
    pub is_animated: bool,

    // Runtime-only: derived from library root, not a DB column. No thumb.
    #[sqlx(skip)]
    pub thumb_path: String,
}

/// How a view sorts its assets. Stored as TEXT; each auto variant maps to an
/// `idx_assets_*` index. `Manual` is scope-dependent — inside a folder it means
/// the membership row's position, everywhere else the asset's own global
/// `manual_position`.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Type, PartialEq, Eq)]
#[sqlx(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum OrderBy {
    ImportedDate,
    CreationDate,
    ModifiedDate,
    Filename,
    FileSize,
    Resolution,
    Manual,
}

impl OrderBy {
    /// The SQL expression to ORDER BY. Every arm is a compile-time constant, so
    /// nothing user-supplied is ever interpolated into the statement — the only
    /// bound value in a manifest query is the folder id.
    fn sql_expr(self, in_folder: bool) -> &'static str {
        match self {
            OrderBy::ImportedDate => "a.imported_date",
            OrderBy::CreationDate => "a.creation_date",
            OrderBy::ModifiedDate => "a.modified_date",
            // NOCASE must match idx_assets_filename's collation or the index is
            // skipped. (Binary collation would also sort "Zebra" before "apple".)
            OrderBy::Filename => "a.filename COLLATE NOCASE",
            OrderBy::FileSize => "a.file_size",
            OrderBy::Resolution => "a.pixel_count",
            OrderBy::Manual if in_folder => "af.position",
            OrderBy::Manual => "a.manual_position",
        }
    }
}

/// One asset↔folder membership row, built at import time from each asset's
/// source parent directory. `position` seeds the manual order within the folder.
struct FolderLink {
    folder_id: String,
    asset_id: String,
    position: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone, FromRow)]
pub struct Folder {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,
    /// Sibling ordering under the same parent (fractional rank).
    pub position: f64,
    pub order_by: OrderBy,
    pub is_ascending: bool,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ImportResult {
    pub folders: Vec<Folder>,
    pub assets: Vec<AssetMetadata>,
    pub path_links: HashMap<String, String>,
}

#[derive(Serialize, Clone, Debug)]
// #[serde(rename_all = "camelCase")]
pub enum ImportStage {
    Scanning,
    ProcessingMetadata,
    CopyingFiles,
    Finalizing,
}

#[derive(Serialize, Clone, Debug)]
pub struct ImportProgress {
    pub stage: ImportStage,
    pub current: usize,
    pub total: usize,
    pub message: String,
}

pub trait ProgressReporter: Send + Sync {
    fn report(&self, progress: ImportProgress);
}

// TODO: Use phf
const IMG_EXTS: &[&str] = &["bmp", "gif", "jfif", "jpeg", "jpg", "png", "webp"];
const VID_EXTS: &[&str] = &["avi", "mkv", "mov", "mp4", "webm"];
const AUD_EXTS: &[&str] = &["flac", "m4a", "mp3", "ogg", "wav"];

fn detect_asset_type(path: &Path) -> AssetType {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();

    if IMG_EXTS.binary_search(&ext.as_str()).is_ok() {
        return AssetType::Image;
    }
    if VID_EXTS.binary_search(&ext.as_str()).is_ok() {
        return AssetType::Video;
    }
    if AUD_EXTS.binary_search(&ext.as_str()).is_ok() {
        return AssetType::Audio;
    }

    AssetType::Unknown
}

/// Open the next WHERE conjunct: emits ` WHERE ` for the first one and ` AND `
/// for every one after. Each predicate block below stays independent and can be
/// added, removed or reordered without touching its neighbours.
fn conjunct(qb: &mut QueryBuilder<'_, Sqlite>, written: &mut usize) {
    qb.push(if *written == 0 { " WHERE " } else { " AND " });
    *written += 1;
}

/// Compose the manifest statement from scope + filters + sort. Split out from
/// execution so the shape stays readable in one screen.
///
/// Only enum variants and booleans reach the SQL text; every user-supplied value
/// (folder id, asset types) is a bound parameter.
fn build_manifest_query<'a>(
    scope: &'a Scope,
    filters: &'a FilterSet,
    sort: Sort,
) -> QueryBuilder<'a, Sqlite> {
    let in_folder = matches!(scope, Scope::Folder { .. });

    let mut qb = QueryBuilder::new(
        "SELECT a.id, a.width, a.height, a.asset_type, a.thumb_hash, a.is_animated FROM assets a",
    );
    if in_folder {
        qb.push(" JOIN assets_folders af ON af.asset_id = a.id");
    }

    let mut written = 0usize;

    // 1. Scope — which rows exist at all.
    match scope {
        Scope::All => {}
        Scope::Folder { id } => {
            conjunct(&mut qb, &mut written);
            qb.push("af.folder_id = ").push_bind(id.as_str());
        }
        Scope::Uncategorized => {
            conjunct(&mut qb, &mut written);
            qb.push("a.id NOT IN (SELECT asset_id FROM assets_folders)");
        }
    }

    // 2. Filters — narrowing within that scope. Dimensions AND together; values
    //    within one dimension OR together.
    if !filters.asset_types.is_empty() {
        conjunct(&mut qb, &mut written);
        qb.push("a.asset_type IN (");
        let mut list = qb.separated(", ");
        for t in &filters.asset_types {
            list.push_bind(*t);
        }
        qb.push(")");
    }

    if let Some(shape) = filters.shape {
        conjunct(&mut qb, &mut written);
        qb.push(DIMENSIONED).push(" AND ");
        shape.push_predicate(&mut qb);
    }

    // 3. Sort. The `a.id` tie-break runs in the SAME direction as the sort column
    //    so the composite (col, id) indexes stay usable scanning either way.
    let dir = if sort.is_ascending { " ASC" } else { " DESC" };
    qb.push(" ORDER BY ")
        .push(sort.order_by.sql_expr(in_folder))
        .push(dir)
        .push(", a.id")
        .push(dir);

    qb
}

#[instrument(skip(pool))]
pub async fn fetch_manifest(
    pool: &SqlitePool,
    query: &ManifestQuery,
) -> Result<Vec<AssetLightRow>> {
    let sort = match query.sort {
        Some(s) => s,
        None => resolve_sort(pool, &query.scope).await?,
    };

    build_manifest_query(&query.scope, &query.filters, sort)
        .build_query_as::<AssetLightRow>()
        .fetch_all(pool)
        .await
        .context("Failed to fetch asset manifest")
}

// ── Persisted sort ────────────────────────────────────────────────────────────

const FOLDER_SORT_SQL: &str = "SELECT order_by, is_ascending FROM folders WHERE id = ?";
const VIEW_SORT_SQL: &str = "SELECT order_by, is_ascending FROM view_settings WHERE view_key = ?";

/// The persisted sort for a scope. Folders keep theirs on their own row (the FK
/// cascade cleans it up on delete); the two fixed views keep theirs in
/// `view_settings`.
///
/// These two functions are the ONLY place that distinction exists — every caller
/// downstream just receives a `Sort`. That's the uniformity worth having; making
/// the *storage* uniform (sentinel folders) would have cost a guard in every
/// membership query instead.
#[instrument(skip(pool))]
pub async fn resolve_sort(pool: &SqlitePool, scope: &Scope) -> Result<Sort> {
    let row: Option<(OrderBy, bool)> = match scope {
        Scope::Folder { id } => {
            sqlx::query_as(FOLDER_SORT_SQL)
                .bind(id.as_str())
                .fetch_optional(pool)
                .await
        }
        Scope::All => {
            sqlx::query_as(VIEW_SORT_SQL)
                .bind("all")
                .fetch_optional(pool)
                .await
        }
        Scope::Uncategorized => {
            sqlx::query_as(VIEW_SORT_SQL)
                .bind("uncategorized")
                .fetch_optional(pool)
                .await
        }
    }
    .context("Failed to read persisted sort")?;

    // A missing row is not an error — the user still gets a usable view.
    Ok(row
        .map(|(order_by, is_ascending)| Sort {
            order_by,
            is_ascending,
        })
        .unwrap_or(DEFAULT_SORT))
}

#[instrument(skip(pool))]
pub async fn set_sort(pool: &SqlitePool, scope: &Scope, sort: Sort) -> Result<()> {
    let res = match scope {
        Scope::Folder { id } => {
            sqlx::query("UPDATE folders SET order_by = ?, is_ascending = ? WHERE id = ?")
                .bind(sort.order_by)
                .bind(sort.is_ascending)
                .bind(id.as_str())
                .execute(pool)
                .await
        }
        // Upsert, so the fixed views keep working even if the seed rows are gone.
        _ => {
            let key = if matches!(scope, Scope::All) {
                "all"
            } else {
                "uncategorized"
            };
            sqlx::query(
                "INSERT INTO view_settings (view_key, order_by, is_ascending) VALUES (?, ?, ?) \
                 ON CONFLICT(view_key) DO UPDATE \
                 SET order_by = excluded.order_by, is_ascending = excluded.is_ascending",
            )
            .bind(key)
            .bind(sort.order_by)
            .bind(sort.is_ascending)
            .execute(pool)
            .await
        }
    }
    .context("Failed to persist sort")?;

    if res.rows_affected() == 0 {
        anyhow::bail!("Folder not found");
    }
    Ok(())
}

#[instrument(skip(pool))]
pub async fn fetch_folders(pool: &SqlitePool) -> Result<Vec<Folder>> {
    let folders = sqlx::query_as::<_, Folder>(
        "SELECT id, name, parent_id, position, order_by, is_ascending
         FROM folders
         ORDER BY parent_id, position, name",
    )
    .fetch_all(pool)
    .await
    .context("Failed to fetch folders")?;
    Ok(folders)
}

// ── Folder CRUD (app-exclusive; never touches the source filesystem) ──────────

/// Next sibling position = MAX(position)+1 among folders sharing `parent_id`
/// (NULL parent = root). `IS ?` handles the NULL comparison in one query.
async fn next_folder_position(pool: &SqlitePool, parent_id: Option<&str>) -> Result<f64> {
    let max = sqlx::query_scalar::<_, Option<f64>>(
        "SELECT MAX(position) FROM folders WHERE parent_id IS ?",
    )
    .bind(parent_id)
    .fetch_one(pool)
    .await
    .context("Failed to compute folder position")?;
    Ok(max.map(|m| m + 1.0).unwrap_or(0.0))
}

#[instrument(skip(pool))]
pub async fn create_folder(
    pool: &SqlitePool,
    name: &str,
    parent_id: Option<&str>,
) -> Result<Folder> {
    let id = uuid::Uuid::new_v4().to_string();
    let position = next_folder_position(pool, parent_id).await?;

    sqlx::query(
        "INSERT INTO folders (id, name, parent_id, position, order_by, is_ascending)
         VALUES (?, ?, ?, ?, 'manual', 1)",
    )
    .bind(&id)
    .bind(name)
    .bind(parent_id)
    .bind(position)
    .execute(pool)
    .await
    .context("Failed to insert folder")?;

    Ok(Folder {
        id,
        name: name.to_string(),
        parent_id: parent_id.map(str::to_string),
        position,
        order_by: OrderBy::Manual,
        is_ascending: true,
    })
}

#[instrument(skip(pool))]
pub async fn rename_folder(pool: &SqlitePool, id: &str, name: &str) -> Result<()> {
    let res = sqlx::query("UPDATE folders SET name = ? WHERE id = ?")
        .bind(name)
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to rename folder")?;
    if res.rows_affected() == 0 {
        anyhow::bail!("Folder not found");
    }
    Ok(())
}

/// Delete a folder. The self-FK and membership FKs are `ON DELETE CASCADE`, so
/// this also removes every descendant folder and all membership rows — but never
/// the assets themselves (an asset with no remaining folder becomes uncategorized).
#[instrument(skip(pool))]
pub async fn delete_folder(pool: &SqlitePool, id: &str) -> Result<()> {
    sqlx::query("DELETE FROM folders WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to delete folder")?;
    Ok(())
}

/// Reparent a folder (and append it to the end of the new parent's siblings).
/// Rejects moving a folder into itself or one of its own descendants, which would
/// orphan the subtree — checked by walking up from the target parent to the root.
#[instrument(skip(pool))]
pub async fn move_folder(pool: &SqlitePool, id: &str, new_parent_id: Option<&str>) -> Result<()> {
    let mut cursor = new_parent_id.map(str::to_string);
    while let Some(cur) = cursor {
        if cur == id {
            anyhow::bail!("Cannot move a folder into itself or a descendant");
        }
        cursor =
            sqlx::query_scalar::<_, Option<String>>("SELECT parent_id FROM folders WHERE id = ?")
                .bind(&cur)
                .fetch_optional(pool)
                .await
                .context("Failed to walk folder ancestry")?
                .flatten();
    }

    let position = next_folder_position(pool, new_parent_id).await?;
    let res = sqlx::query("UPDATE folders SET parent_id = ?, position = ? WHERE id = ?")
        .bind(new_parent_id)
        .bind(position)
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to move folder")?;
    if res.rows_affected() == 0 {
        anyhow::bail!("Folder not found");
    }
    Ok(())
}

/// Add assets to a folder, appended after its existing members. `INSERT OR IGNORE`
/// makes re-adding an already-present asset a no-op (keeps its current position).
#[instrument(skip(pool, asset_ids), fields(count = asset_ids.len()))]
pub async fn add_assets_to_folder(
    pool: &SqlitePool,
    folder_id: &str,
    asset_ids: &[String],
) -> Result<()> {
    if asset_ids.is_empty() {
        return Ok(());
    }
    let mut position = sqlx::query_scalar::<_, Option<f64>>(
        "SELECT MAX(position) FROM assets_folders WHERE folder_id = ?",
    )
    .bind(folder_id)
    .fetch_one(pool)
    .await
    .context("Failed to compute membership position")?
    .map(|m| m + 1.0)
    .unwrap_or(0.0);

    let mut tx = pool
        .begin()
        .await
        .context("Failed to begin membership transaction")?;
    for id in asset_ids {
        sqlx::query(
            "INSERT OR IGNORE INTO assets_folders (folder_id, asset_id, position) VALUES (?, ?, ?)",
        )
        .bind(folder_id)
        .bind(id)
        .bind(position)
        .execute(&mut *tx)
        .await
        .context("Failed to add asset to folder")?;
        position += 1.0;
    }
    tx.commit()
        .await
        .context("Failed to commit membership transaction")?;
    Ok(())
}

#[instrument(skip(pool, asset_ids), fields(count = asset_ids.len()))]
pub async fn remove_assets_from_folder(
    pool: &SqlitePool,
    folder_id: &str,
    asset_ids: &[String],
) -> Result<()> {
    if asset_ids.is_empty() {
        return Ok(());
    }
    let mut qb = QueryBuilder::new("DELETE FROM assets_folders WHERE folder_id = ");
    qb.push_bind(folder_id);
    qb.push(" AND asset_id IN (");
    let mut separated = qb.separated(", ");
    for id in asset_ids {
        separated.push_bind(id);
    }
    qb.push(")");
    qb.build()
        .execute(pool)
        .await
        .context("Failed to remove assets from folder")?;
    Ok(())
}

#[instrument(skip(pool, ids), fields(count = ids.len()))]
pub async fn fetch_assets_by_ids(
    pool: &SqlitePool,
    root: &Path,
    ids: &[String],
) -> Result<Vec<AssetMetadata>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    // Window sizes stay small (visible + overscan), well under SQLite's
    // 32766 bind-parameter limit, so no chunking needed.
    let mut qb = QueryBuilder::new(
        "SELECT id, asset_type, filename, extension, path, width, height, file_size, \
         imported_date, creation_date, modified_date, thumb_hash, is_animated \
         FROM assets WHERE id IN (",
    );
    let mut separated = qb.separated(", ");
    for id in ids {
        separated.push_bind(id);
    }
    qb.push(")");

    let mut rows = qb
        .build_query_as::<AssetMetadata>()
        .fetch_all(pool)
        .await
        .context("Failed to fetch assets by ids")?;

    for row in &mut rows {
        // Resolve the stored relative `path` back to an absolute path the webview
        // can load. (Joining an already-absolute path from an older library is a
        // no-op — the absolute arm wins — so pre-T2.2 rows still resolve.)
        row.dest_path = root.join(&row.dest_path).to_string_lossy().into_owned();

        // Derive the thumbnail path from the root, only when the file actually
        // exists. Otherwise leave "" so the frontend falls back to full res.
        if row.thumb_hash.is_some() {
            let candidate = root.join("thumbnails").join(format!("{}.webp", row.id));
            if candidate.exists() {
                row.thumb_path = candidate.to_string_lossy().into_owned();
            }
        }
    }
    Ok(rows)
}

#[instrument(skip(pool, assets, folders, links),
    fields(assets = assets.len(), folders = folders.len(), links = links.len()))]
async fn persist_import(
    pool: &SqlitePool,
    assets: &[AssetMetadata],
    folders: &[Folder],
    links: &[FolderLink],
) -> Result<()> {
    let start = std::time::Instant::now();

    // SQLite caps bound parameters at 32766. With 15 columns per row, keep each
    // multi-row INSERT well under that (15 * 1500 = 22500 params per statement)
    const ROWS_PER_INSERT: usize = 1500;

    let mut tx = pool
        .begin()
        .await
        .context("Failed to begin database transaction")?;

    // 1. Folders first — the self-referencing FK needs parents before children.
    //    WalkDir's pre-order scan already yields them in that order, and FK checks
    //    are immediate, so insertion order is what makes this valid.
    for chunk in folders.chunks(ROWS_PER_INSERT) {
        let mut qb = QueryBuilder::new(
            "INSERT INTO folders (id, name, parent_id, position, order_by, is_ascending) ",
        );
        qb.push_values(chunk, |mut b, f| {
            b.push_bind(&f.id)
                .push_bind(&f.name)
                .push_bind(f.parent_id.as_deref())
                .push_bind(f.position)
                .push_bind(f.order_by)
                .push_bind(f.is_ascending);
        });
        qb.build()
            .execute(&mut *tx)
            .await
            .context("Failed to insert folder chunk")?;
    }

    // 2. Assets. Global manual order continues after whatever is already in the
    //    library, so "manual" means import order until drag-to-reorder lands.
    let manual_base =
        sqlx::query_scalar::<_, Option<f64>>("SELECT MAX(manual_position) FROM assets")
            .fetch_one(&mut *tx)
            .await
            .context("Failed to compute manual position base")?
            .map(|m| m + 1.0)
            .unwrap_or(0.0);

    for (chunk_idx, chunk) in assets.chunks(ROWS_PER_INSERT).enumerate() {
        let base = manual_base + (chunk_idx * ROWS_PER_INSERT) as f64;
        let mut offset = 0.0f64;

        let mut qb = QueryBuilder::new(
            "INSERT INTO assets (id, asset_type, filename, extension, path, \
                   width, height, file_size, manual_position, imported_date, creation_date, \
                   modified_date, thumb_hash, thumb_config, is_animated) ",
        );

        qb.push_values(chunk, |mut b, asset| {
            b.push_bind(&asset.id)
                .push_bind(asset.asset_type)
                .push_bind(&asset.filename)
                .push_bind(&asset.extension)
                .push_bind(&asset.dest_path)
                .push_bind(asset.width)
                .push_bind(asset.height)
                .push_bind(asset.file_size)
                .push_bind(base + offset)
                .push_bind(&asset.imported_date)
                .push_bind(&asset.creation_date)
                .push_bind(&asset.modified_date)
                .push_bind(asset.thumb_hash.as_deref())
                .push_bind(asset.thumb_config.as_deref())
                .push_bind(asset.is_animated);
            offset += 1.0;
        });

        qb.build()
            .execute(&mut *tx)
            .await
            .context("Failed to batch insert asset chunk")?;
    }

    // 3. Menbership links last - each references a folder and asset inserted above
    for chunk in links.chunks(ROWS_PER_INSERT) {
        let mut qb =
            QueryBuilder::new("INSERT INTO assets_folders (folder_id, asset_id, position) ");
        qb.push_values(chunk, |mut b, l| {
            b.push_bind(&l.folder_id)
                .push_bind(&l.asset_id)
                .push_bind(l.position);
        });
        qb.build()
            .execute(&mut *tx)
            .await
            .context("Failed to insert membership chunk")?;
    }

    tx.commit()
        .await
        .context("Failed to commit asset transaction")?;

    // Fold the WAL back into the main DB after a large write so the -wal file
    // doesnt grow too large. Non-fatal: The data is already comitted.

    if let Err(e) = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(pool)
        .await
    {
        warn!(error = %e, "WAL checkpoint after persist failed (non-fatal)");
    }

    info!(
        assets = assets.len(),
        folders = folders.len(),
        links = links.len(),
        elapsed_ms = start.elapsed().as_millis(),
        "Import persisted to database"
    );
    Ok(())
}

fn build_asset_metadata(src: PathBuf) -> Option<AssetMetadata> {
    let asset_type = detect_asset_type(&src);
    let meta = std::fs::metadata(&src)
        .inspect_err(|e| warn!(path = ?src, error = %e, "Could not read file metadata, skipping"))
        .ok()?;

    let modified: DateTime<Utc> = meta
        .modified()
        .map(DateTime::<Utc>::from)
        .unwrap_or_else(|_| Utc::now());

    let created: DateTime<Utc> = meta
        .created()
        .map(DateTime::<Utc>::from)
        .unwrap_or(modified);

    let ext = src.extension()?.to_str()?;
    let id = uuid::Uuid::new_v4().to_string();
    // Stored RELATIVE to the library root so a `.library` stays portable when the
    // folder is moved or renamed. Forward slashes join correctly on every platform;
    // the absolute path is derived at copy time and on read (fetch_assets_by_ids).
    let dest_path = format!("assets/{}.{}", id, ext);

    // Import-time extraction is CHEAP metadata only (dimensions, animation flag).
    // The thumbnail + ThumbHash are produced later by the background pipeline, so
    // import never blocks on decode/encode. A failure yields "no visual" — the
    // asset still persists (never drop a user's file).
    let visual = extract::extractor_for(asset_type)
        .extract(&src)
        .unwrap_or_else(|e| {
            warn!(path = ?src, error = %e, "Metadata extraction failed; keeping asset with no visual");
            extract::ExtractedVisual::default()
        });

    Some(AssetMetadata {
        id,
        asset_type,
        filename: src.file_name()?.to_string_lossy().into_owned(),
        extension: ext.to_string(),
        dest_path,
        source_path: src.to_string_lossy().into_owned(),
        width: visual.width,
        height: visual.height,
        file_size: meta.len() as i64,
        imported_date: Utc::now().to_rfc3339(),
        creation_date: created.to_rfc3339(),
        modified_date: modified.to_rfc3339(),
        thumb_hash: None, // generated later by generate_pending_thumbnails
        thumb_config: None,
        is_animated: visual.is_animated,
        thumb_path: String::new(),
    })
}

/// Copy every staged original into the library, returning the ids whose copy
/// FAILED so the caller can drop them (never persist a row pointing at a missing
/// file). Individual failures are non-fatal; only a task panic aborts.
#[instrument(skip(reporter, assets), fields(total = assets.len()))]
async fn copy_assets(
    reporter: Arc<dyn ProgressReporter>,
    assets: &[AssetMetadata],
    root: &Path,
) -> Result<HashSet<String>> {
    let start = std::time::Instant::now();
    let semaphore = Arc::new(Semaphore::new(10));
    let completed = Arc::new(AtomicUsize::new(0));
    let total = assets.len();
    let mut handles = Vec::with_capacity(total);

    for asset in assets {
        let permit = Arc::clone(&semaphore)
            .acquire_owned()
            .await
            .context("Failed to acquire semaphore permit for file copy")?;

        let src = PathBuf::from(&asset.source_path);
        // dest_path is relative to the library root; join to get the copy target.
        let dst = root.join(&asset.dest_path);
        let reporter = Arc::clone(&reporter);
        let completed = Arc::clone(&completed);
        let filename = asset.filename.clone();
        let id = asset.id.clone();

        handles.push(tokio::spawn(async move {
            let _permit = permit;

            match tokio::fs::copy(&src, &dst).await {
                Ok(bytes) => {
                    let current = completed.fetch_add(1, Ordering::Relaxed) + 1;
                    debug!(file = %filename, bytes, "File copied");

                    reporter.report(ImportProgress {
                        stage: ImportStage::CopyingFiles,
                        current,
                        total,
                        message: format!("Importing: {}", filename),
                    });
                    None
                }
                Err(e) => {
                    warn!(src = ?src, error = %e, "Failed to copy file, skipping");
                    Some(id)
                }
            }
        }));
    }

    let mut failed = HashSet::new();
    for handle in handles {
        if let Some(id) = handle.await.context("File copy task panicked")? {
            failed.insert(id);
        }
    }

    if !failed.is_empty() {
        warn!(failed = failed.len(), total, "Import had copy failures");
    }

    info!(
        total,
        failed = failed.len(),
        elapsed_ms = start.elapsed().as_millis(),
        "File copy stage complete"
    );

    Ok(failed)
}

/// Best-effort removal of copied originals after a failed persist, so a failed
/// import leaves nothing orphaned in `assets/`. Missing files are ignored.
async fn cleanup_orphans(root: &Path, assets: &[AssetMetadata]) {
    for a in assets {
        let path = root.join(&a.dest_path);
        match tokio::fs::remove_file(&path).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => warn!(path = ?path, error = %e, "Failed to remove orphaned file"),
        }
    }
}

#[instrument(skip(reporter, pool), fields(source = %source_dir.display()))]
pub async fn import_assets(
    reporter: Arc<dyn ProgressReporter>,
    source_dir: PathBuf,
    pool: SqlitePool,
    library_root: PathBuf,
    import_folders: bool,
) -> Result<ImportResult> {
    let pipeline_start = std::time::Instant::now();

    reporter.report(ImportProgress {
        stage: ImportStage::Scanning,
        current: 0,
        total: 0,
        message: "Scanning folder structure...".into(),
    });

    // Stage 1: Resolve destination directory.
    let assets_dir = library_root.join("assets");
    let thumbs_dir = library_root.join("thumbnails");
    fs::ensure_dir(&assets_dir).await?;
    fs::ensure_dir(&thumbs_dir).await?;

    // Stage 2: Walk directory tree.
    let scan_start = std::time::Instant::now();
    let (folders, folder_id_by_path) = if import_folders {
        fs::scan_directories(&source_dir)
    } else {
        (Vec::new(), HashMap::new())
    };
    let discovered_files = fs::collect_files(&source_dir);
    let file_count = discovered_files.len();

    info!(
        folders = folders.len(),
        files = file_count,
        elapsed_ms = scan_start.elapsed().as_millis(),
        "Directory scan complete"
    );

    reporter.report(ImportProgress {
        stage: ImportStage::ProcessingMetadata,
        current: 0,
        total: file_count,
        message: format!("Processing {} files...", file_count),
    });

    // Stage 3: Build metadata in parallel (CPU-bound via Rayon).
    let metadata_start = std::time::Instant::now();

    let mut staged_assets: Vec<AssetMetadata> = discovered_files
        .into_par_iter()
        .filter(|p| !matches!(detect_asset_type(p), AssetType::Unknown))
        .filter_map(build_asset_metadata)
        .collect();

    info!(
        count = staged_assets.len(),
        elapsed_ms = metadata_start.elapsed().as_millis(),
        "Metadata stage complete"
    );

    reporter.report(ImportProgress {
        stage: ImportStage::CopyingFiles,
        current: 0,
        total: staged_assets.len(),
        message: "Copying files...".into(),
    });

    // Stage 4: Copy files with bounded concurrency (I/O-bound via Tokio). Drop any
    // asset whose file failed to copy so we never persist a row pointing at a
    // missing file (T1.3).
    let failed = copy_assets(reporter.clone(), &staged_assets, &library_root).await?;
    if !failed.is_empty() {
        warn!(
            dropped = failed.len(),
            "Dropping assets whose file copy failed"
        );
        staged_assets.retain(|a| !failed.contains(&a.id));
    }

    // Resolve each surviving asset to the folder its source file lived in.
    // `source_path` is retained on AssetMetadata, so no rescan is needed. Files
    // directly under the import root have no scanned parent folder → they stay
    // free; `folder_id_by_path` is empty when importing without structure. Built
    // after the copy so a link never references a dropped asset.
    let links: Vec<FolderLink> = {
        let mut counters: HashMap<String, f64> = HashMap::new();
        staged_assets
            .iter()
            .filter_map(|a| {
                let parent = Path::new(&a.source_path).parent()?;
                let folder_id = folder_id_by_path.get(parent)?;
                let position = counters.entry(folder_id.clone()).or_insert(0.0);
                let link = FolderLink {
                    folder_id: folder_id.clone(),
                    asset_id: a.id.clone(),
                    position: *position,
                };
                *position += 1.0;
                Some(link)
            })
            .collect()
    };

    // Stage 5: Persist all metadata atomically. If it fails, remove the originals
    // we just copied so a failed import leaves nothing orphaned on disk (T1.3).
    reporter.report(ImportProgress {
        stage: ImportStage::Finalizing,
        current: 0,
        total: staged_assets.len(),
        message: "Saving to database...".into(),
    });

    if let Err(e) = persist_import(&pool, &staged_assets, &folders, &links).await {
        cleanup_orphans(&library_root, &staged_assets).await;
        return Err(e);
    }

    info!(
        assets = staged_assets.len(),
        folders = folders.len(),
        elapsed_ms = pipeline_start.elapsed().as_millis(),
        "Import pipeline complete"
    );

    Ok(ImportResult {
        folders,
        assets: staged_assets,
        path_links: folder_id_by_path
            .into_iter()
            .map(|(k, v)| (k.to_string_lossy().into_owned(), v))
            .collect(),
    })
}

// ── Background thumbnail pipeline ─────────────────────────────────────────────
//
// Thumbnails are a disposable, rebuildable cache. Import persists rows with
// thumb_hash = NULL; this stage fills them in afterward (and on next launch,
// resuming any that were interrupted) without ever blocking import or the UI.

#[derive(FromRow, Clone)]
struct PendingThumb {
    id: String,
    extension: String,
}

/// One generated thumbnail's DB-facing result.
struct ThumbUpdate {
    id: String,
    thumb_hash: String,
    thumb_config: String,
    wrote_file: bool,
}

/// A completed thumbnail, ready for the UI to patch into its row in place: the
/// `thumb_hash` drives the placeholder and `thumb_path` the full thumbnail, so no
/// manifest reload or re-fetch is needed.
#[derive(Serialize, Clone)]
pub struct ThumbReady {
    pub id: String,
    pub thumb_hash: String,
    pub thumb_path: String,
}

/// Progress sink for thumbnail generation. `ready` is the batch that just
/// completed. Never dropped/throttled by the caller — losing a batch would leave
/// rows un-patched until the next reload.
pub trait ThumbProgress: Send + Sync {
    fn report(&self, done: usize, total: usize, ready: &[ThumbReady]);
}

/// Clear the entire thumbnail cache so it regenerates from scratch: delete the
/// on-disk WebP files and NULL the per-row thumb columns. Used by "Rebuild
/// thumbnails" (e.g. after changing the quality mode). Wiping the directory
/// matters for correctness — otherwise a re-encode could leave a stale file, and
/// an image that now SKIPs (small enough) would keep an orphaned WebP that
/// `fetch_assets_by_ids`'s `.exists()` check would still serve. Callers run the
/// generation pass afterward.
#[instrument(skip(pool, root))]
pub async fn reset_thumbnails(pool: &SqlitePool, root: &Path) -> Result<()> {
    let thumbs_dir = root.join("thumbnails");
    if thumbs_dir.exists() {
        if let Err(e) = tokio::fs::remove_dir_all(&thumbs_dir).await {
            warn!(dir = ?thumbs_dir, error = %e, "Failed to clear thumbnails dir (non-fatal)");
        }
    }
    fs::ensure_dir(&thumbs_dir).await?;

    sqlx::query(
        "UPDATE assets SET thumb_hash = NULL, thumb_config = NULL WHERE asset_type = 'image'",
    )
    .execute(pool)
    .await
    .context("Failed to reset thumbnail columns")?;

    info!("Thumbnail cache cleared for rebuild");
    Ok(())
}

/// Generate thumbnails for every image whose `thumb_hash` is still NULL. Used by
/// an explicit "generate all" action; on-view generation uses the by-ids variant.
/// Idempotent and resumable — `thumb_hash IS NULL` is the only "pending" marker.
#[instrument(skip(pool, root, progress))]
pub async fn generate_pending_thumbnails(
    pool: &SqlitePool,
    root: &Path,
    config: thumbnail::ThumbConfig,
    progress: Arc<dyn ThumbProgress>,
) -> Result<usize> {
    let pending: Vec<PendingThumb> = sqlx::query_as::<_, PendingThumb>(
        "SELECT id, extension FROM assets \
         WHERE thumb_hash IS NULL AND asset_type = 'image' \
         ORDER BY imported_date DESC, id DESC",
    )
    .fetch_all(pool)
    .await
    .context("Failed to query pending thumbnails")?;

    run_generation(pool, root, config, pending, progress).await
}

/// Generate thumbnails only for the given ids that are still missing one — the
/// lazy, on-view path. Ids already generated, non-image, or unknown are ignored.
#[instrument(skip(pool, root, progress, ids), fields(requested = ids.len()))]
pub async fn generate_thumbnails_for_ids(
    pool: &SqlitePool,
    root: &Path,
    config: thumbnail::ThumbConfig,
    ids: &[String],
    progress: Arc<dyn ThumbProgress>,
) -> Result<usize> {
    if ids.is_empty() {
        return Ok(0);
    }

    let mut qb = QueryBuilder::new(
        "SELECT id, extension FROM assets \
         WHERE thumb_hash IS NULL AND asset_type = 'image' AND id IN (",
    );
    let mut separated = qb.separated(", ");
    for id in ids {
        separated.push_bind(id);
    }
    qb.push(")");

    let pending: Vec<PendingThumb> = qb
        .build_query_as::<PendingThumb>()
        .fetch_all(pool)
        .await
        .context("Failed to query pending thumbnails by id")?;

    run_generation(pool, root, config, pending, progress).await
}

/// Shared chunked generator: decode/resize/encode on the blocking pool (Rayon
/// fan-out), persist per chunk, and report each completed batch for in-place UI
/// patching.
async fn run_generation(
    pool: &SqlitePool,
    root: &Path,
    config: thumbnail::ThumbConfig,
    pending: Vec<PendingThumb>,
    progress: Arc<dyn ThumbProgress>,
) -> Result<usize> {
    let total = pending.len();
    if total == 0 {
        return Ok(0);
    }

    let assets_dir = root.join("assets");
    let thumbs_dir = root.join("thumbnails");
    fs::ensure_dir(&thumbs_dir).await?;

    info!(total, "Thumbnail generation started");
    let start = std::time::Instant::now();

    // Small enough that the UI sees updates often; large enough to keep Rayon fed.
    const CHUNK: usize = 64;
    let mut done = 0usize;

    for chunk in pending.chunks(CHUNK) {
        let chunk = chunk.to_vec();
        let chunk_len = chunk.len();
        let job_assets = assets_dir.clone();
        let job_thumbs = thumbs_dir.clone();

        let results: Vec<ThumbUpdate> = tokio::task::spawn_blocking(move || {
            chunk
                .into_par_iter()
                .filter_map(|p| {
                    let src = job_assets.join(format!("{}.{}", p.id, p.extension));
                    let dest = job_thumbs.join(format!("{}.webp", p.id));
                    match thumbnail::generate(&src, &dest, config) {
                        Ok(t) => Some(ThumbUpdate {
                            id: p.id,
                            thumb_hash: t.thumb_hash,
                            thumb_config: t.thumb_config,
                            wrote_file: t.wrote_file,
                        }),
                        Err(e) => {
                            warn!(id = %p.id, error = %e, "Thumbnail generation failed; leaving row NULL");
                            None
                        }
                    }
                })
                .collect()
        })
        .await
        .context("Thumbnail generation task panicked")?;

        update_thumbnails(pool, &results).await?;

        let ready: Vec<ThumbReady> = results
            .iter()
            .map(|u| ThumbReady {
                id: u.id.clone(),
                thumb_hash: u.thumb_hash.clone(),
                // Empty when no file was written (source small enough)
                // the UI fallsback to the original
                thumb_path: if u.wrote_file {
                    thumbs_dir
                        .join(format!("{}.webp", u.id))
                        .to_string_lossy()
                        .into_owned()
                } else {
                    String::new()
                },
            })
            .collect();
        done += chunk_len;
        progress.report(done, total, &ready);
    }

    if let Err(e) = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(pool)
        .await
    {
        warn!(error = %e, "WAL checkpoint after thumbnail generation failed (non-fatal)");
    }

    info!(
        total,
        elapsed_ms = start.elapsed().as_millis(),
        "Thumbnail generation complete"
    );
    Ok(total)
}

#[instrument(skip(pool, updates), fields(count = updates.len()))]
async fn update_thumbnails(pool: &SqlitePool, updates: &[ThumbUpdate]) -> Result<()> {
    if updates.is_empty() {
        return Ok(());
    }
    let mut tx = pool
        .begin()
        .await
        .context("Failed to begin thumbnail update transaction")?;

    for u in updates {
        sqlx::query("UPDATE assets SET thumb_hash = ?, thumb_config = ? WHERE id = ?")
            .bind(&u.thumb_hash)
            .bind(&u.thumb_config)
            .bind(&u.id)
            .execute(&mut *tx)
            .await
            .context("Failed to update thumbnail row")?;
    }

    tx.commit()
        .await
        .context("Failed to commit thumbnail updates")?;
    Ok(())
}
