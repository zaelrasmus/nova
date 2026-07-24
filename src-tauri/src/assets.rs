use crate::extract;
use crate::fs;
use crate::thumbnail;
use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
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

/// The one timestamp format for every date column: fixed width, always UTC `Z`,
/// always 3 fractional digits — `2026-07-23T12:34:56.123Z`.
///
/// Fixed width is the whole point. chrono's default `to_rfc3339()` emits
/// variable-length fractional seconds and a `+00:00` offset, which happens to
/// sort correctly only because ASCII puts `+` before `.` before digits. That
/// invariant is invisible in the code and one differently-formatted writer away
/// from breaking silently: a `Z` row would sort after EVERY `+00:00` row
/// regardless of its actual date. This format also matches what
/// `assets_folders.added_at` already writes, so the whole DB is one shape.
///
/// Changing this again means every existing row must be rewritten — mixed
/// formats in one column is exactly the failure described above.
fn stamp(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// `stamp(Utc::now())`, for callers outside this module. Exists so no other file
/// is tempted to format a timestamp its own way — see above for why that matters.
pub fn now_stamp() -> String {
    stamp(Utc::now())
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
///
/// `Serialize` is load-bearing, not incidental: this type IS the stored form of
/// a saved filter (`saved_filters.query_json`), which is why there's no separate
/// hand-written schema for persisted filters to drift out of sync with.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct FilterSet {
    /// Types to include. Empty = no constraint. Multiple types OR together.
    #[serde(default)]
    pub asset_types: Vec<AssetType>,
    /// Shape constraint; `None` = any.
    #[serde(default)]
    pub shape: Option<Shape>,
    /// Calendar-day range over one date column; `None` = no date filtering.
    #[serde(default)]
    pub date: Option<DateFilter>,
    /// Byte-size range; `None` = no size filtering.
    #[serde(default)]
    pub size: Option<SizeRange>,
    /// Dominant-color proximity; `None` = no color filtering.
    #[serde(default)]
    pub color: Option<ColorFilter>,
    /// Tag constraint; `None` = no tag filtering. `#[serde(default)]` means saved
    /// filters written before tags existed still parse — they just get `None`.
    #[serde(default)]
    pub tags: Option<crate::tags::TagFilter>,
}

/// Match assets containing a color close to `(r, g, b)`.
///
/// Tests every palette entry, not just the most dominant one — that's the point
/// of storing a palette (see `asset_colors`).
#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct ColorFilter {
    /// Target color as sRGB 0–255, straight from the picker or hex field.
    /// Converted to LAB here so the color science lives in exactly one place.
    pub r: u8,
    pub g: u8,
    pub b: u8,
    /// Largest perceptual distance (ΔE) still counted as a match. Roughly: 2 is
    /// a just-noticeable difference, 10 a clearly different shade, 25+ a
    /// different color family. The UI's "Accuracy" slider is the INVERSE of this
    /// — more accuracy means less tolerance.
    pub tolerance: f64,
    /// Smallest share of the image (0.0–1.0) the matching entry must cover, so a
    /// stray handful of pixels doesn't make an image "red".
    pub min_coverage: f64,
}

/// Which date column a `DateFilter` applies to.
///
/// The shared `Date` postfix is deliberate: these names and their serde values
/// mirror `OrderBy::ImportedDate` / `CreationDate` / `ModifiedDate`, so the same
/// column is called the same thing whether you're sorting or filtering by it.
/// Trimming them to `Imported`/`Creation`/`Modified` would break that symmetry
/// for a style rule.
#[allow(clippy::enum_variant_names)]
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DateField {
    ImportedDate,
    CreationDate,
    ModifiedDate,
}

impl DateField {
    fn column(self) -> &'static str {
        match self {
            DateField::ImportedDate => "a.imported_date",
            DateField::CreationDate => "a.creation_date",
            DateField::ModifiedDate => "a.modified_date",
        }
    }
}

/// A half-open instant range over one date column: `[from, until)`.
///
/// The frontend sends absolute timestamps rather than calendar days, because
/// only the client knows the user's timezone. It converts the picked LOCAL days
/// into instants, and `until` is local midnight of the day AFTER the end date —
/// so an inclusive-looking "Jan 1 → Jan 31" still contains everything stamped
/// during the 31st. Interpreting days as UTC here instead would mean a user west
/// of Greenwich clicking "Today" gets nothing they imported that evening.
///
/// JS `toISOString()` emits exactly the shape `stamp()` writes, so this stays a
/// plain lexicographic comparison against an indexed column.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DateFilter {
    pub field: DateField,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub until: Option<String>,
}

/// Inclusive byte range; either end may be open.
#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct SizeRange {
    #[serde(default)]
    pub min: Option<i64>,
    #[serde(default)]
    pub max: Option<i64>,
}

/// Whether a bound is a real timestamp. Lexicographic comparison against a
/// garbage string wouldn't error, it would just return a nonsense slice — so an
/// unparseable bound is rejected at the boundary instead.
fn valid_stamp(s: &str) -> bool {
    DateTime::parse_from_rfc3339(s).is_ok()
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

    /// User-authored, never derived from the file.
    #[sqlx(default)]
    pub notes: Option<String>,

    /// Where the asset came from. See the column comment in the migration for
    /// why this is stored permissively but opened strictly.
    #[sqlx(default)]
    pub source_url: Option<String>,

    /// BLAKE3 of the file's bytes; `None` when hashing failed (see
    /// `fs::hash_file`). Never leaves Rust — the frontend has no use for it and
    /// shipping it would put a fingerprint of every file in the webview.
    #[serde(skip)]
    #[sqlx(default)]
    pub content_hash: Option<String>,

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
    #[sqlx(default)]
    pub notes: Option<String>,
    #[sqlx(default)]
    pub created_at: String,
}

/// Everything an import needs that the file tree cannot tell it.
///
/// Exists so the dialog path and the drop path stay ONE pipeline: they differ
/// only in these four values, and bundling them keeps that difference visible in
/// one place instead of spreading across two near-identical functions that drift.
///
/// Rust-internal — the commands assemble it from their own arguments, so it
/// never crosses the IPC boundary and needs no serde.
#[derive(Debug)]
pub struct ImportRequest {
    /// Directories, files, or a mix — a drop hands over whatever was grabbed.
    pub sources: Vec<PathBuf>,
    /// Existing folder the whole import nests beneath. `None` = library root.
    /// Also where loose files land, which is what makes dropping a handful of
    /// images onto a folder file them there.
    pub target_folder: Option<String>,
    /// Recreate the source directory structure as folders. Off = every asset
    /// arrives free (or in `target_folder`, if there is one).
    pub import_folders: bool,
    /// Whether each source directory becomes a folder itself. See
    /// `fs::scan_directories`.
    pub include_roots: bool,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ImportResult {
    pub folders: Vec<Folder>,
    pub assets: Vec<AssetMetadata>,
    pub path_links: HashMap<String, String>,
    /// Files skipped because the library already held their exact bytes. Worth
    /// reporting rather than hiding: "imported 3 of 200" with no explanation
    /// reads as a failure, and this is the explanation.
    pub duplicates: usize,
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

    if let Some(date) = &filters.date {
        let col = date.field.column();
        // An invalid bound matches nothing rather than being dropped: silently
        // widening the result set is the dangerous direction (you'd see MORE than
        // you asked for and have no signal). Same policy as a malformed ratio.
        if let Some(from) = date.from.as_deref() {
            conjunct(&mut qb, &mut written);
            if valid_stamp(from) {
                qb.push(col).push(" >= ").push_bind(from);
            } else {
                qb.push("0");
            }
        }
        if let Some(until) = date.until.as_deref() {
            conjunct(&mut qb, &mut written);
            // Half-open: `until` is the instant AFTER the last included day.
            if valid_stamp(until) {
                qb.push(col).push(" < ").push_bind(until);
            } else {
                qb.push("0");
            }
        }
    }

    if let Some(size) = filters.size {
        if let Some(min) = size.min {
            conjunct(&mut qb, &mut written);
            qb.push("a.file_size >= ").push_bind(min);
        }
        if let Some(max) = size.max {
            conjunct(&mut qb, &mut written);
            qb.push("a.file_size <= ").push_bind(max);
        }
    }

    if let Some(c) = filters.color {
        conjunct(&mut qb, &mut written);
        let target = crate::color::srgb_to_lab(c.r, c.g, c.b);
        // Squared distance vs squared tolerance: no sqrt, so the whole predicate
        // is multiply-and-add and needs no SQLite math extension. The lightness
        // weight is computed from the TARGET's chroma (see color::lightness_weight)
        // so "red" matches dark and pale reds, while a grey search still
        // distinguishes grey from black and white.
        let l_weight = crate::color::lightness_weight(target) as f64;
        let tol_sq = c.tolerance.max(0.0).powi(2);

        qb.push("EXISTS (SELECT 1 FROM asset_colors c WHERE c.asset_id = a.id AND c.ratio >= ")
            .push_bind(c.min_coverage)
            .push(" AND ")
            .push_bind(l_weight)
            .push(" * (c.l - ")
            .push_bind(target.l as f64)
            .push(") * (c.l - ")
            .push_bind(target.l as f64)
            .push(") + (c.a - ")
            .push_bind(target.a as f64)
            .push(") * (c.a - ")
            .push_bind(target.a as f64)
            .push(") + (c.b - ")
            .push_bind(target.b as f64)
            .push(") * (c.b - ")
            .push_bind(target.b as f64)
            .push(") <= ")
            .push_bind(tol_sq)
            .push(")");
    }

    if let Some(tags) = &filters.tags {
        if tags.is_active() {
            conjunct(&mut qb, &mut written);
            tags.push_predicate(&mut qb);
        }
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
        "SELECT id, name, parent_id, position, order_by, is_ascending, notes, created_at
         FROM folders
         ORDER BY parent_id, position, name",
    )
    .fetch_all(pool)
    .await
    .context("Failed to fetch folders")?;
    Ok(folders)
}

// ── Saved filters ─────────────────────────────────────────────────────────────
//
// A saved filter is a LENS, not a place: applying one narrows whatever scope
// you're already in, so it has no parent, no sort and no position in the tree.
// (A smart folder would be the opposite — a scope that owns its own sort — and
// belongs in its own table beside `folders`, not here.)

/// Bump when `FilterSet`'s serialized shape changes incompatibly, and migrate
/// stored documents deliberately at the same time.
const SAVED_FILTER_VERSION: i64 = 1;

/// A named, reusable `FilterSet` as the frontend sees it — the JSON document is
/// already parsed, so callers never touch the stored representation.
#[derive(Serialize, Debug, Clone)]
pub struct SavedFilter {
    pub id: String,
    pub name: String,
    pub position: f64,
    pub filters: FilterSet,
}

/// Storage shape, with `query_json` still a string.
#[derive(FromRow)]
struct SavedFilterRow {
    id: String,
    name: String,
    position: f64,
    version: i64,
    query_json: String,
}

#[instrument(skip(pool))]
pub async fn fetch_saved_filters(pool: &SqlitePool) -> Result<Vec<SavedFilter>> {
    let rows = sqlx::query_as::<_, SavedFilterRow>(
        "SELECT id, name, position, version, query_json FROM saved_filters \
         ORDER BY position, name",
    )
    .fetch_all(pool)
    .await
    .context("Failed to fetch saved filters")?;

    // An unreadable row is skipped, never fatal: one filter written by a newer
    // build (or corrupted) must not take the user's whole list down with it.
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            if r.version > SAVED_FILTER_VERSION {
                warn!(id = %r.id, version = r.version, "Saved filter is from a newer version; skipping");
                return None;
            }
            match serde_json::from_str::<FilterSet>(&r.query_json) {
                Ok(filters) => Some(SavedFilter {
                    id: r.id,
                    name: r.name,
                    position: r.position,
                    filters,
                }),
                Err(e) => {
                    warn!(id = %r.id, error = %e, "Saved filter has unreadable query_json; skipping");
                    None
                }
            }
        })
        .collect())
}

#[instrument(skip(pool, filters))]
pub async fn create_saved_filter(
    pool: &SqlitePool,
    name: &str,
    filters: &FilterSet,
) -> Result<SavedFilter> {
    let id = uuid::Uuid::new_v4().to_string();
    let query_json = serde_json::to_string(filters).context("Failed to serialize filter set")?;
    let position = sqlx::query_scalar::<_, Option<f64>>("SELECT MAX(position) FROM saved_filters")
        .fetch_one(pool)
        .await
        .context("Failed to compute saved filter position")?
        .map(|m| m + 1.0)
        .unwrap_or(0.0);

    sqlx::query(
        "INSERT INTO saved_filters (id, name, position, version, query_json, created_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(name)
    .bind(position)
    .bind(SAVED_FILTER_VERSION)
    .bind(&query_json)
    .bind(stamp(Utc::now()))
    .execute(pool)
    .await
    .context("Failed to insert saved filter")?;

    Ok(SavedFilter {
        id,
        name: name.to_string(),
        position,
        filters: filters.clone(),
    })
}

#[instrument(skip(pool))]
pub async fn rename_saved_filter(pool: &SqlitePool, id: &str, name: &str) -> Result<()> {
    let res = sqlx::query("UPDATE saved_filters SET name = ? WHERE id = ?")
        .bind(name)
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to rename saved filter")?;
    if res.rows_affected() == 0 {
        anyhow::bail!("Saved filter not found");
    }
    Ok(())
}

/// Overwrite a saved filter's definition with `filters` ("update to current").
/// Also rewrites `version`, so a document saved by an older build is brought
/// forward rather than left behind at its original version.
#[instrument(skip(pool, filters))]
pub async fn update_saved_filter(
    pool: &SqlitePool,
    id: &str,
    filters: &FilterSet,
) -> Result<()> {
    let query_json = serde_json::to_string(filters).context("Failed to serialize filter set")?;
    let res = sqlx::query("UPDATE saved_filters SET query_json = ?, version = ? WHERE id = ?")
        .bind(&query_json)
        .bind(SAVED_FILTER_VERSION)
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to update saved filter")?;
    if res.rows_affected() == 0 {
        anyhow::bail!("Saved filter not found");
    }
    Ok(())
}

#[instrument(skip(pool))]
pub async fn delete_saved_filter(pool: &SqlitePool, id: &str) -> Result<()> {
    sqlx::query("DELETE FROM saved_filters WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to delete saved filter")?;
    Ok(())
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

// ── Inspector edits ───────────────────────────────────────────────────────────
//
// Every field here is metadata: renaming an asset mutates the database ONLY. The
// file on disk stays `UUID.ext`, which is what makes rename instant, always
// reversible, and free of the failure modes a real rename carries (locked files,
// per-OS invalid characters, collisions, and a half-applied state when the FS
// write succeeds and the DB write doesn't). It's also what lets two assets share
// a display name, which a managed library must allow.

/// Partial update of an asset. `None` means "leave this column alone", `Some("")`
/// means "clear it to NULL" — a distinction the UI relies on, since it sends one
/// field at a time as the user edits it.
#[derive(Deserialize, Debug, Default)]
#[serde(default)]
pub struct AssetPatch {
    /// The name WITHOUT its extension. `filename` is always recomposed as
    /// `{stem}.{extension}` from the row's own extension, so the two columns can
    /// never drift and the extension — which describes the actual bytes, not the
    /// user's label — can't be edited away by accident.
    pub stem: Option<String>,
    pub notes: Option<String>,
    pub source_url: Option<String>,
}

/// Partial update of a folder. Same `None` vs `Some("")` contract as `AssetPatch`.
#[derive(Deserialize, Debug, Default)]
#[serde(default)]
pub struct FolderPatch {
    pub name: Option<String>,
    pub notes: Option<String>,
}

/// Trim, and treat an empty result as NULL. Used for every free-text field, so a
/// cleared box and a box full of spaces both mean "unset" rather than leaving a
/// row that looks blank but isn't.
fn blank_to_null(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Clean a user-supplied display name.
///
/// These never reach the filesystem, so this is about legibility, not safety:
/// path separators and control characters would render as garbage and would
/// break a future "export under the display name". An all-whitespace name is
/// rejected outright — a nameless row is unreadable in every list that shows it.
fn clean_name(raw: &str) -> Result<String> {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '/' | '\\'))
        .collect();
    blank_to_null(&cleaned).ok_or_else(|| anyhow::anyhow!("Name cannot be empty"))
}

/// Apply `patch` and return the updated row, so the caller refreshes its cache
/// from what was actually stored rather than from what it hoped it stored.
#[instrument(skip(pool, root))]
pub async fn update_asset(
    pool: &SqlitePool,
    root: &Path,
    id: &str,
    patch: AssetPatch,
) -> Result<AssetMetadata> {
    let filename = match patch.stem {
        Some(raw) => {
            let stem = clean_name(&raw).context("An asset needs a name")?;
            let extension: String = sqlx::query_scalar("SELECT extension FROM assets WHERE id = ?")
                .bind(id)
                .fetch_optional(pool)
                .await
                .context("Failed to read asset extension")?
                .ok_or_else(|| anyhow::anyhow!("Asset not found"))?;
            // Extensionless files exist; don't leave them with a trailing dot.
            Some(if extension.is_empty() {
                stem
            } else {
                format!("{stem}.{extension}")
            })
        }
        None => None,
    };
    let notes = patch.notes.map(|s| blank_to_null(&s));
    let source_url = patch.source_url.map(|s| blank_to_null(&s));

    if filename.is_some() || notes.is_some() || source_url.is_some() {
        let mut qb = QueryBuilder::new("UPDATE assets SET ");
        {
            let mut sep = qb.separated(", ");
            if let Some(v) = &filename {
                sep.push("filename = ");
                sep.push_bind_unseparated(v);
            }
            if let Some(v) = &notes {
                sep.push("notes = ");
                sep.push_bind_unseparated(v);
            }
            if let Some(v) = &source_url {
                sep.push("source_url = ");
                sep.push_bind_unseparated(v);
            }
        }
        qb.push(" WHERE id = ").push_bind(id);

        let res = qb
            .build()
            .execute(pool)
            .await
            .context("Failed to update asset")?;
        if res.rows_affected() == 0 {
            anyhow::bail!("Asset not found");
        }
    }

    let ids = [id.to_string()];
    fetch_assets_by_ids(pool, root, &ids)
        .await?
        .pop()
        .ok_or_else(|| anyhow::anyhow!("Asset not found"))
}

#[instrument(skip(pool))]
pub async fn create_folder(
    pool: &SqlitePool,
    name: &str,
    parent_id: Option<&str>,
) -> Result<Folder> {
    let id = uuid::Uuid::new_v4().to_string();
    let position = next_folder_position(pool, parent_id).await?;
    // Written explicitly rather than left to the column DEFAULT, so the returned
    // struct carries the same value the row does without a read-back.
    let created_at = stamp(Utc::now());

    sqlx::query(
        "INSERT INTO folders (id, name, parent_id, position, created_at, order_by, is_ascending)
         VALUES (?, ?, ?, ?, ?, 'manual', 1)",
    )
    .bind(&id)
    .bind(name)
    .bind(parent_id)
    .bind(position)
    .bind(&created_at)
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
        notes: None,
        created_at,
    })
}

// ── Selection aggregates ──────────────────────────────────────────────────────
//
// A selection can be arbitrarily large (Ctrl+A over the whole library), so unlike
// `fetch_assets_by_ids` — which only ever sees a visible window — these chunk
// their id lists. SQLite caps bound parameters at 32766.
const IDS_PER_QUERY: usize = 8000;

/// Deduplicate before chunking. Repeated ids inside ONE `IN (...)` list are
/// harmless, but the same id in two different chunks would be counted twice.
fn unique_ids(ids: &[String]) -> Vec<&String> {
    let mut seen = std::collections::HashSet::with_capacity(ids.len());
    ids.iter().filter(|id| seen.insert(id.as_str())).collect()
}

/// Exact totals for a set of assets.
#[derive(Serialize, Debug, Clone, Default)]
pub struct SelectionSummary {
    pub count: i64,
    pub total_bytes: i64,
}

#[instrument(skip(pool, ids), fields(ids = ids.len()))]
pub async fn selection_summary(pool: &SqlitePool, ids: &[String]) -> Result<SelectionSummary> {
    let ids = unique_ids(ids);
    let mut summary = SelectionSummary::default();

    for chunk in ids.chunks(IDS_PER_QUERY) {
        let mut qb = QueryBuilder::new(
            "SELECT COUNT(*), COALESCE(SUM(file_size), 0) FROM assets WHERE id IN (",
        );
        let mut separated = qb.separated(", ");
        for id in chunk {
            separated.push_bind(*id);
        }
        qb.push(")");

        let (count, bytes): (i64, i64) = qb
            .build_query_as()
            .fetch_one(pool)
            .await
            .context("Failed to summarize selection")?;
        summary.count += count;
        summary.total_bytes += bytes;
    }
    Ok(summary)
}

/// How many of the queried assets sit in one folder — the raw material for the
/// UI's all / some / none tri-state.
#[derive(Serialize, Debug, Clone)]
pub struct FolderMembership {
    pub folder_id: String,
    pub count: i64,
}

/// Membership counts for every folder that holds at least one of `ids`. Folders
/// holding none are simply absent, which the caller reads as zero.
#[instrument(skip(pool, ids), fields(ids = ids.len()))]
pub async fn folder_membership(pool: &SqlitePool, ids: &[String]) -> Result<Vec<FolderMembership>> {
    let ids = unique_ids(ids);
    let mut totals: HashMap<String, i64> = HashMap::new();

    for chunk in ids.chunks(IDS_PER_QUERY) {
        let mut qb = QueryBuilder::new(
            "SELECT folder_id, COUNT(DISTINCT asset_id) FROM assets_folders WHERE asset_id IN (",
        );
        let mut separated = qb.separated(", ");
        for id in chunk {
            separated.push_bind(*id);
        }
        qb.push(") GROUP BY folder_id");

        let rows: Vec<(String, i64)> = qb
            .build_query_as()
            .fetch_all(pool)
            .await
            .context("Failed to read folder membership")?;
        for (folder_id, count) in rows {
            *totals.entry(folder_id).or_insert(0) += count;
        }
    }

    Ok(totals
        .into_iter()
        .map(|(folder_id, count)| FolderMembership { folder_id, count })
        .collect())
}

/// What a folder contains, counting every descendant folder.
#[derive(Serialize, Debug, Clone)]
pub struct FolderStats {
    pub asset_count: i64,
    pub total_bytes: i64,
    /// Subfolders below this one at any depth, excluding the folder itself.
    pub descendant_folders: i64,
}

/// Aggregate a folder's whole subtree.
///
/// `DISTINCT` is not optional here: an asset can be a member of BOTH a parent and
/// one of its children, and a plain join would count it twice and add its bytes
/// twice. The size sum therefore runs over the deduplicated id set rather than
/// over the join — `SUM(DISTINCT file_size)` would be a different (and wrong)
/// thing entirely, collapsing two same-sized files into one.
///
/// Deliberately a separate call made when a folder is SELECTED, never part of
/// `fetch_folders`: listing N folders would otherwise run N recursive CTEs.
#[instrument(skip(pool))]
pub async fn folder_stats(pool: &SqlitePool, folder_id: &str) -> Result<FolderStats> {
    // `UNION` (not `UNION ALL`) also makes this terminate if the tree ever
    // contained a cycle — repeated ids stop the recursion instead of hanging.
    // `move_folder` already rejects cycles; this is the second line of defence.
    let (descendant_folders, asset_count, total_bytes): (i64, i64, i64) = sqlx::query_as(
        "WITH RECURSIVE subtree(id) AS (
             SELECT id FROM folders WHERE id = ?
             UNION
             SELECT f.id FROM folders f JOIN subtree s ON f.parent_id = s.id
         ),
         members AS (
             SELECT DISTINCT af.asset_id AS asset_id
             FROM assets_folders af JOIN subtree s ON af.folder_id = s.id
         )
         SELECT
             (SELECT COUNT(*) FROM subtree) - 1,
             (SELECT COUNT(*) FROM members),
             (SELECT COALESCE(SUM(a.file_size), 0)
              FROM members m JOIN assets a ON a.id = m.asset_id)",
    )
    .bind(folder_id)
    .fetch_one(pool)
    .await
    .context("Failed to compute folder stats")?;

    Ok(FolderStats {
        asset_count,
        total_bytes,
        descendant_folders,
    })
}

#[instrument(skip(pool))]
pub async fn update_folder(pool: &SqlitePool, id: &str, patch: FolderPatch) -> Result<()> {
    let name = match patch.name {
        Some(raw) => Some(clean_name(&raw).context("A folder needs a name")?),
        None => None,
    };
    let notes = patch.notes.map(|s| blank_to_null(&s));

    if name.is_none() && notes.is_none() {
        return Ok(());
    }

    let mut qb = QueryBuilder::new("UPDATE folders SET ");
    {
        let mut sep = qb.separated(", ");
        if let Some(v) = &name {
            sep.push("name = ");
            sep.push_bind_unseparated(v);
        }
        if let Some(v) = &notes {
            sep.push("notes = ");
            sep.push_bind_unseparated(v);
        }
    }
    qb.push(" WHERE id = ").push_bind(id);

    let res = qb
        .build()
        .execute(pool)
        .await
        .context("Failed to update folder")?;
    if res.rows_affected() == 0 {
        anyhow::bail!("Folder not found");
    }
    Ok(())
}

/// Delete a folder. The self-FK and membership FKs are `ON DELETE CASCADE`, so
/// this also removes every descendant folder and all membership rows — but never
/// the assets themselves (an asset with no remaining folder becomes uncategorized).
/// Delete one or more folders.
///
/// One statement rather than a loop: deleting a parent cascades its children, so
/// a loop would hit rows that no longer exist — harmless per-row, but it leaves a
/// half-applied tree if any call in the middle fails. Selecting a parent and its
/// child together is normal in a tree, so this is the common case, not an edge.
#[instrument(skip(pool, ids), fields(count = ids.len()))]
pub async fn delete_folders(pool: &SqlitePool, ids: &[String]) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let mut qb = QueryBuilder::new("DELETE FROM folders WHERE id IN (");
    let mut separated = qb.separated(", ");
    for id in ids {
        separated.push_bind(id);
    }
    qb.push(")");

    qb.build()
        .execute(pool)
        .await
        .context("Failed to delete folders")?;
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
         imported_date, creation_date, modified_date, notes, source_url, thumb_hash, is_animated \
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

    // SQLite caps bound parameters at 32766. With 16 columns per row, keep each
    // multi-row INSERT well under that (16 * 1500 = 24000 params per statement)
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
            "INSERT INTO folders (id, name, parent_id, position, created_at, order_by, is_ascending) ",
        );
        qb.push_values(chunk, |mut b, f| {
            b.push_bind(&f.id)
                .push_bind(&f.name)
                .push_bind(f.parent_id.as_deref())
                .push_bind(f.position)
                .push_bind(&f.created_at)
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
                   modified_date, content_hash, thumb_hash, thumb_config, is_animated) ",
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
                .push_bind(asset.content_hash.as_deref())
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
    //
    // OR IGNORE because dedup can route a link to an EXISTING asset that is
    // already a member of that folder — re-importing a folder you've imported
    // before. The (folder_id, asset_id) PK would otherwise abort the whole
    // transaction over a row that is already correct.
    for chunk in links.chunks(ROWS_PER_INSERT) {
        let mut qb = QueryBuilder::new(
            "INSERT OR IGNORE INTO assets_folders (folder_id, asset_id, position) ",
        );
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

    // Hashed here rather than at copy time because this stage is already a Rayon
    // par_iter over files — the parallelism is free, and the result is needed
    // BEFORE the copy stage so a duplicate never touches the disk at all.
    let content_hash = crate::fs::hash_file(&src);

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
        imported_date: stamp(Utc::now()),
        creation_date: stamp(created),
        modified_date: stamp(modified),
        // User-authored fields; an imported file has neither until someone types
        // one. `source_url` will be filled in by the download path when it lands.
        notes: None,
        source_url: None,
        content_hash,
        thumb_hash: None, // generated later by generate_pending_thumbnails
        thumb_config: None,
        is_animated: visual.is_animated,
        thumb_path: String::new(),
    })
}

/// Staged assets, partitioned by whether the library already holds their bytes.
struct DedupSplit {
    /// Genuinely new — copy these and insert rows for them.
    fresh: Vec<AssetMetadata>,
    /// Bytes we already have. Pairs the EXISTING asset's id with the staged
    /// entry it displaced, because the staged entry still carries the
    /// `source_path` that decides which folder the duplicate was headed for.
    /// Dropping it silently would lose that organisational intent.
    duplicates: Vec<(String, AssetMetadata)>,
}

/// Partition staged assets against the library's existing fingerprints.
///
/// Catches BOTH kinds of duplicate, which is why one pass builds a map rather
/// than issuing a query per file:
///   * already in the library — the re-drop case;
///   * repeated within this very batch — one image sitting in two of the dropped
///     folders. The first occurrence wins and the rest resolve to its id, so a
///     batch can never insert two rows with the same hash and trip the unique
///     index mid-transaction.
///
/// Unhashed assets (see `fs::hash_file`) always land in `fresh`: without a
/// fingerprint there is nothing to compare, and refusing them would lose files.
#[instrument(skip(pool, staged), fields(staged = staged.len()))]
async fn split_duplicates(pool: &SqlitePool, staged: Vec<AssetMetadata>) -> Result<DedupSplit> {
    let hashes: Vec<&str> = staged
        .iter()
        .filter_map(|a| a.content_hash.as_deref())
        .collect();

    // hash -> id of whichever asset owns those bytes.
    let mut owner: HashMap<String, String> = HashMap::new();

    for chunk in hashes.chunks(IDS_PER_QUERY) {
        let mut qb = QueryBuilder::new("SELECT content_hash, id FROM assets WHERE content_hash IN (");
        let mut separated = qb.separated(", ");
        for h in chunk {
            separated.push_bind(*h);
        }
        qb.push(")");
        let rows: Vec<(String, String)> = qb
            .build_query_as()
            .fetch_all(pool)
            .await
            .context("Failed to look up existing content hashes")?;
        owner.extend(rows);
    }

    let mut split = DedupSplit {
        fresh: Vec::with_capacity(staged.len()),
        duplicates: Vec::new(),
    };

    for asset in staged {
        match asset.content_hash.as_deref() {
            Some(hash) => match owner.get(hash) {
                Some(existing) => split.duplicates.push((existing.clone(), asset)),
                None => {
                    // First sighting of these bytes — claim them, so later copies
                    // in this same batch resolve here instead of duplicating.
                    owner.insert(hash.to_string(), asset.id.clone());
                    split.fresh.push(asset);
                }
            },
            None => split.fresh.push(asset),
        }
    }

    Ok(split)
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

#[instrument(skip(reporter, pool, request),
    fields(sources = request.sources.len(), target = ?request.target_folder))]
pub async fn import_assets(
    reporter: Arc<dyn ProgressReporter>,
    request: ImportRequest,
    pool: SqlitePool,
    library_root: PathBuf,
) -> Result<ImportResult> {
    let pipeline_start = std::time::Instant::now();
    let ImportRequest {
        sources,
        target_folder,
        import_folders,
        include_roots,
    } = request;

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
        // Continue the target's existing children rather than restarting at 0,
        // so dropped folders append instead of interleaving with what's there.
        let root_position = next_folder_position(&pool, target_folder.as_deref()).await?;
        fs::scan_directories(
            &sources,
            include_roots,
            target_folder.as_deref(),
            root_position,
        )
    } else {
        (Vec::new(), HashMap::new())
    };
    let discovered_files = fs::collect_files(&sources);
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

    let scanned: Vec<AssetMetadata> = discovered_files
        .into_par_iter()
        .filter(|p| !matches!(detect_asset_type(p), AssetType::Unknown))
        .filter_map(build_asset_metadata)
        .collect();

    info!(
        count = scanned.len(),
        elapsed_ms = metadata_start.elapsed().as_millis(),
        "Metadata stage complete"
    );

    // Stage 3b: drop anything whose bytes the library already holds. Done before
    // the copy so a duplicate costs a hash comparison instead of a file write.
    let DedupSplit {
        fresh: mut staged_assets,
        duplicates,
    } = split_duplicates(&pool, scanned).await?;

    if !duplicates.is_empty() {
        info!(
            count = duplicates.len(),
            "Skipping files already in the library; linking the existing assets instead"
        );
    }

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
    // Duplicates join in here: the file wasn't copied, but the folder it sat in
    // still says where the user wanted it, so the EXISTING asset picks up that
    // membership. Re-dropping a folder you've already imported therefore
    // organises rather than doing nothing.
    let links: Vec<FolderLink> = {
        let mut counters: HashMap<String, f64> = HashMap::new();
        staged_assets
            .iter()
            .map(|a| (a.id.as_str(), a.source_path.as_str()))
            .chain(
                duplicates
                    .iter()
                    .map(|(existing_id, staged)| (existing_id.as_str(), staged.source_path.as_str())),
            )
            .filter_map(|(asset_id, source_path)| {
                // A file whose directory was scanned joins that folder. Anything
                // else — a loose dropped file, or every file when structure is
                // off — falls back to the drop target. With no target (the
                // dialog path) it stays free, preserving the old behaviour where
                // files directly under the import root are uncategorized.
                let folder_id = Path::new(source_path)
                    .parent()
                    .and_then(|p| folder_id_by_path.get(p).cloned())
                    .or_else(|| target_folder.clone())?;
                let position = counters.entry(folder_id.clone()).or_insert(0.0);
                let link = FolderLink {
                    folder_id,
                    asset_id: asset_id.to_string(),
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
        duplicates: duplicates.len(),
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
    palette: Vec<crate::color::PaletteEntry>,
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
                            palette: t.palette,
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

// ── Color analysis ────────────────────────────────────────────────────────────

/// One palette entry as the UI consumes it: sRGB for display plus the share of
/// the image it covers.
///
/// Stored in CIELAB and converted on read rather than kept as extra RGB columns.
/// Eight rows per asset makes the conversion free, it needs no schema change or
/// backfill, and it keeps every color transform in `color.rs` instead of spread
/// across a table definition.
#[derive(Serialize, Debug, Clone)]
pub struct PaletteSwatch {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    /// Share of the sampled pixels, 0.0–1.0.
    pub ratio: f32,
}

/// An asset's palette, most-covering first. Empty means "not analyzed yet",
/// which is the same signal `color_coverage` reports library-wide.
#[instrument(skip(pool))]
pub async fn fetch_palette(pool: &SqlitePool, asset_id: &str) -> Result<Vec<PaletteSwatch>> {
    let rows: Vec<(f32, f32, f32, f32)> = sqlx::query_as(
        "SELECT l, a, b, ratio FROM asset_colors WHERE asset_id = ? ORDER BY ratio DESC",
    )
    .bind(asset_id)
    .fetch_all(pool)
    .await
    .context("Failed to fetch asset palette")?;

    Ok(rows
        .into_iter()
        .map(|(l, a, b, ratio)| {
            let (r, g, blue) = crate::color::lab_to_srgb(crate::color::Lab { l, a, b });
            PaletteSwatch {
                r,
                g,
                b: blue,
                ratio,
            }
        })
        .collect())
}

/// How much of the library has a color palette. An asset with no `asset_colors`
/// rows simply hasn't been analyzed, and a color filter cannot match it — so the
/// UI reports this instead of silently under-reporting results.
#[derive(Serialize, Debug, Clone)]
pub struct ColorCoverage {
    pub analyzed: i64,
    pub total: i64,
}

#[instrument(skip(pool))]
pub async fn color_coverage(pool: &SqlitePool) -> Result<ColorCoverage> {
    let total: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM assets WHERE asset_type = 'image'")
            .fetch_one(pool)
            .await
            .context("Failed to count images")?;
    let analyzed: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT asset_id) FROM asset_colors c \
         JOIN assets a ON a.id = c.asset_id WHERE a.asset_type = 'image'",
    )
    .fetch_one(pool)
    .await
    .context("Failed to count analyzed images")?;
    Ok(ColorCoverage { analyzed, total })
}

/// Overwrite one asset's palette inside an existing transaction.
async fn replace_palette(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    asset_id: &str,
    palette: &[crate::color::PaletteEntry],
) -> Result<()> {
    sqlx::query("DELETE FROM asset_colors WHERE asset_id = ?")
        .bind(asset_id)
        .execute(&mut **tx)
        .await
        .context("Failed to clear existing palette")?;

    for entry in palette {
        sqlx::query("INSERT INTO asset_colors (asset_id, l, a, b, ratio) VALUES (?, ?, ?, ?, ?)")
            .bind(asset_id)
            .bind(entry.lab.l as f64)
            .bind(entry.lab.a as f64)
            .bind(entry.lab.b as f64)
            .bind(entry.ratio as f64)
            .execute(&mut **tx)
            .await
            .context("Failed to insert palette entry")?;
    }
    Ok(())
}

#[derive(FromRow, Clone)]
struct PendingColor {
    id: String,
    extension: String,
}

/// Backfill palettes for images that don't have one yet.
///
/// Reads the generated THUMBNAIL where one exists — a ~320px WebP decodes in a
/// fraction of the time the original would, which is what makes analyzing an
/// existing library practical. Falls back to the original for assets whose source
/// was small enough that no thumbnail file was written.
#[instrument(skip(pool, root, progress))]
pub async fn analyze_colors(
    pool: &SqlitePool,
    root: &Path,
    progress: Arc<dyn ThumbProgress>,
) -> Result<usize> {
    let pending: Vec<PendingColor> = sqlx::query_as::<_, PendingColor>(
        "SELECT id, extension FROM assets \
         WHERE asset_type = 'image' AND id NOT IN (SELECT asset_id FROM asset_colors) \
         ORDER BY imported_date DESC, id DESC",
    )
    .fetch_all(pool)
    .await
    .context("Failed to query images pending color analysis")?;

    let total = pending.len();
    if total == 0 {
        return Ok(0);
    }

    let assets_dir = root.join("assets");
    let thumbs_dir = root.join("thumbnails");
    info!(total, "Color analysis started");
    let start = std::time::Instant::now();

    const CHUNK: usize = 64;
    let mut done = 0usize;

    for chunk in pending.chunks(CHUNK) {
        let chunk = chunk.to_vec();
        let chunk_len = chunk.len();
        let job_assets = assets_dir.clone();
        let job_thumbs = thumbs_dir.clone();

        let results: Vec<(String, Vec<crate::color::PaletteEntry>)> =
            tokio::task::spawn_blocking(move || {
                chunk
                    .into_par_iter()
                    .filter_map(|p| {
                        let thumb = job_thumbs.join(format!("{}.webp", p.id));
                        let src = if thumb.exists() {
                            thumb
                        } else {
                            job_assets.join(format!("{}.{}", p.id, p.extension))
                        };
                        match image::open(&src) {
                            Ok(img) => Some((p.id, crate::color::extract_palette(&img))),
                            Err(e) => {
                                warn!(id = %p.id, error = %e, "Color analysis failed; leaving unanalyzed");
                                None
                            }
                        }
                    })
                    .collect()
            })
            .await
            .context("Color analysis task panicked")?;

        let mut tx = pool
            .begin()
            .await
            .context("Failed to begin color analysis transaction")?;
        for (id, palette) in &results {
            replace_palette(&mut tx, id, palette).await?;
        }
        tx.commit()
            .await
            .context("Failed to commit color analysis")?;

        done += chunk_len;
        progress.report(done, total, &[]);
    }

    info!(
        total,
        elapsed_ms = start.elapsed().as_millis(),
        "Color analysis complete"
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

        // Replace rather than append: a rebuild re-extracts, and duplicated
        // palette rows would skew every coverage ratio.
        replace_palette(&mut tx, &u.id, &u.palette).await?;
    }

    tx.commit()
        .await
        .context("Failed to commit thumbnail updates")?;
    Ok(())
}
