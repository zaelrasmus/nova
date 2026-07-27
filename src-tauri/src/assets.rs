use crate::extract;
use crate::fs;
use crate::thumbnail;
use anyhow::{Context, Result};
use crate::reject;
use chrono::{DateTime, SecondsFormat, Utc};
use futures_util::TryStreamExt;
use rayon::prelude::{IntoParallelIterator, ParallelIterator};
use serde::{Deserialize, Serialize};
use sqlx::{sqlite::SqlitePool, FromRow, QueryBuilder, Sqlite, Type};
use rustc_hash::FxHashMap;
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
    /// A smart folder: a place whose membership is a QUERY rather than a list.
    ///
    /// Its rule tree becomes the scope predicate, which is what makes it a place
    /// and not a saved filter — you can be *inside* it and still apply a lens on
    /// top, and clearing that lens leaves you where you were.
    Smart { id: String },
    /// A group of smart folders, browsed as the UNION of its members.
    ///
    /// Compiles to one OR of the member trees inside the SAME query — no UNION,
    /// no temp table, and no dedup pass, because we select rows from `assets`
    /// and an asset matching two members is still one row.
    SmartGroup { id: String },
    /// Assets moved to the Trash.
    ///
    /// A scope rather than a mode, which is what lets it reuse the whole read
    /// path: the grid, the virtualizer, selection, sorting and the viewer all
    /// work in here without knowing the Trash exists.
    Trash,
}

/// Where a scope's persisted sort actually lives.
///
/// Total, where this used to be an `Option<String>` whose `None` meant "it must
/// be a folder" — a fact both call sites then re-derived with an
/// `unreachable!()`. Adding a `Scope` variant and forgetting to handle it was a
/// runtime panic; now it fails to compile in the one place that decides.
pub(crate) enum SortHome<'a> {
    /// On the folder's own row, cleaned up by the FK cascade when it is deleted.
    FolderRow(&'a str),
    /// Under this key in `view_settings`, which has no FK and is swept by hand.
    ViewKey(String),
}

impl Scope {
    /// Where this scope keeps its sort.
    ///
    /// Making the STORAGE uniform (sentinel folder rows for the fixed views) was
    /// the alternative, and it would have cost a guard in every membership query
    /// instead. This is the only place the split exists; everything downstream
    /// just receives a `Sort`.
    pub(crate) fn sort_home(&self) -> SortHome<'_> {
        match self {
            Scope::All => SortHome::ViewKey("all".into()),
            Scope::Uncategorized => SortHome::ViewKey("uncategorized".into()),
            Scope::Smart { id } => SortHome::ViewKey(format!("smart:{id}")),
            Scope::SmartGroup { id } => SortHome::ViewKey(format!("smartgroup:{id}")),
            Scope::Trash => SortHome::ViewKey("trash".into()),
            Scope::Folder { id } => SortHome::FolderRow(id),
        }
    }
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
    pub(crate) fn push_predicate(self, qb: &mut QueryBuilder<'_, Sqlite>) {
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
pub(crate) const DIMENSIONED: &str = "a.width > 0 AND a.height > 0";

/// Ephemeral narrowing of a scope. Deliberately NOT persisted as a whole: a
/// filter that survives a restart is the classic "my library is empty, the app
/// is broken" bug.
///
/// Two halves, because they have different lifetimes:
///
///   * `rules` — a structured tree (see the `rules` module). This IS the stored
///     form of a saved filter, so the filter bar and a smart folder speak the
///     same language and compile through the same engine.
///   * `text` — the live search box. Ephemeral even within a session's saved
///     filter: stripped before saving, and rides here only so it flows through
///     the one `stream_manifest` path with every other lens.
///
/// An empty `FilterSet` adds no SQL at all, so the unfiltered path stays exactly
/// as fast as before.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct FilterSet {
    /// Structured conditions. `None` = no structural narrowing.
    #[serde(default)]
    pub rules: Option<crate::rules::RuleNode>,
    /// Live full-text search; `None` = no text filtering. EPHEMERAL — stripped
    /// before a filter is saved (see `save_filter`), so it never lands in the
    /// stored JSON. Rides on `FilterSet` only so it flows through the one
    /// `stream_manifest` query path with every other lens.
    #[serde(default)]
    pub text: Option<crate::search::query::TextSearch>,
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

impl ColorFilter {
    /// Append this colour's proximity test. Lives here rather than inline in the
    /// manifest query so the rule tree and the filter bar compile colour the
    /// same way — one piece of colour science, one predicate.
    pub(crate) fn push_predicate<'a>(
        &self,
        qb: &mut QueryBuilder<'a, Sqlite>,
    ) {
        let target = crate::color::srgb_to_lab(self.r, self.g, self.b);
        // Squared distance vs squared tolerance: no sqrt, so the whole predicate
        // is multiply-and-add and needs no SQLite math extension. The lightness
        // weight is computed from the TARGET's chroma (see color::lightness_weight)
        // so "red" matches dark and pale reds, while a grey search still
        // distinguishes grey from black and white.
        let l_weight = crate::color::lightness_weight(target) as f64;
        let tol_sq = self.tolerance.max(0.0).powi(2);

        qb.push("EXISTS (SELECT 1 FROM asset_colors c WHERE c.asset_id = a.id AND c.ratio >= ")
            .push_bind(self.min_coverage)
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
}

/// Which date column a `DateFilter` applies to.
///
/// The shared `Date` postfix is deliberate: these names and their serde values
/// mirror `OrderBy::ImportedDate` / `CreationDate` / `ModifiedDate`, so the same
/// column is called the same thing whether you're sorting or filtering by it.
/// Trimming them to `Imported`/`Creation`/`Modified` would break that symmetry
/// for a style rule.
#[allow(clippy::enum_variant_names)]
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DateField {
    #[default]
    ImportedDate,
    CreationDate,
    ModifiedDate,
}

impl DateField {
    pub(crate) fn column(self) -> &'static str {
        match self {
            DateField::ImportedDate => "a.imported_date",
            DateField::CreationDate => "a.creation_date",
            DateField::ModifiedDate => "a.modified_date",
        }
    }
}

/// Whether a bound is a real timestamp. Lexicographic comparison against a
/// garbage string wouldn't error, it would just return a nonsense slice — so an
/// unparseable bound is rejected at the boundary instead.
pub(crate) fn valid_stamp(s: &str) -> bool {
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
    /// Display name, carried in the LIGHT row (not just the hydrated heavy row)
    /// so name search can filter in the frontend instantly, no round trip — the
    /// common-case half of the search hybrid. Short strings; cheap in the stream.
    pub filename: String,
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
/// `idx_assets_*` index.
///
/// Two variants are SCOPE-RELATIVE — they mean one thing inside a folder and
/// another everywhere else, because the value they sort by lives on the
/// membership row rather than the asset:
///
///   * `Manual`    — `assets_folders.position` in a folder, `assets.manual_position` outside.
///   * `AddedDate` — `assets_folders.added_at` in a folder, `assets.imported_date` outside.
///
/// Both fall back to an asset-level column so every scope can answer them; the
/// folder answer is simply the more precise one.
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
    /// When the asset was put into THIS folder — the answer to "what did I add
    /// here last week", which `imported_date` can't give once an asset has been
    /// filed somewhere long after it entered the library.
    AddedDate,
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
            // Served by idx_folder_contents (folder_id, added_at) inside a
            // folder; by idx_assets_imported outside it.
            OrderBy::AddedDate if in_folder => "af.added_at",
            OrderBy::AddedDate => "a.imported_date",
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
    /// Sidebar accent, as a palette token name (see [`PIN_COLORS`]). `None` =
    /// unstyled; only meaningful while pinned, but kept independently so
    /// unpinning and re-pinning doesn't lose the colour the user chose.
    #[sqlx(default)]
    pub color: Option<String>,
    /// Rank among pinned folders. `None` = not pinned.
    #[sqlx(default)]
    pub pin_position: Option<f64>,
}

/// The pin accent palette — token names, resolved to colours by the frontend.
///
/// Kept short and well-separated on purpose: a pin's colour exists to be
/// recognised at a glance in a 52px rail, and twenty near-neighbours would be
/// no more legible than the grey wall the colours are there to prevent.
pub const PIN_COLORS: [&str; 8] = [
    "slate", "blue", "cyan", "emerald", "lime", "amber", "rose", "violet",
];

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
    /// Of those duplicates, how many were in the Trash and came back. Reported
    /// separately because "skipped 2" and "2 came back out of the Trash" are
    /// very different outcomes to the person who just dropped the file.
    pub restored: usize,
}

/// Which phase an import is in, as the progress event reports it.
///
/// Serialized with serde's DEFAULT unit-variant naming — the variant name
/// verbatim, `"ProcessingMetadata"` — which is what `+page.svelte`'s
/// `ImportStage` union mirrors. A `rename_all` here would silently rename the
/// wire values and the frontend's type would still compile, so the absence of
/// one is the contract. (There was a commented-out `camelCase` attribute here;
/// applying it would have broken exactly that.)
#[derive(Serialize, Clone, Debug)]
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

/// `contains`, not `binary_search`.
///
/// These lists are five to seven entries — a linear scan over that fits in a
/// cache line and beats a branchy binary search outright. More to the point,
/// `binary_search` silently requires the list to be SORTED, and nothing enforced
/// it: adding `"tiff"` to the end of `IMG_EXTS` would have made every TIFF
/// undetectable, which import turns into "the file was silently dropped". A
/// correctness footgun in exchange for negative performance.
fn detect_asset_type(path: &Path) -> AssetType {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    let ext = ext.as_str();

    if IMG_EXTS.contains(&ext) {
        return AssetType::Image;
    }
    if VID_EXTS.contains(&ext) {
        return AssetType::Video;
    }
    if AUD_EXTS.contains(&ext) {
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
///
/// `scope_rules` is a smart folder's tree, already loaded — resolving it is a DB
/// read and this function is deliberately synchronous, so the caller does it (see
/// `resolve_scope_rules`).
fn build_manifest_query<'a>(
    scope: &'a Scope,
    scope_rules: Option<&'a crate::rules::RuleNode>,
    filters: &'a FilterSet,
    sort: Sort,
) -> QueryBuilder<'a, Sqlite> {
    let in_folder = matches!(scope, Scope::Folder { .. });

    let mut qb = QueryBuilder::new(
        "SELECT a.id, a.width, a.height, a.asset_type, a.thumb_hash, a.is_animated, a.filename FROM assets a",
    );
    if in_folder {
        qb.push(" JOIN assets_folders af ON af.asset_id = a.id");
    }

    // Manual order inside a smart folder reads a sparse rank table. LEFT, not
    // INNER: an asset nobody has placed yet has no row, and it must still appear
    // (in the unranked tail) rather than vanish from its own folder.
    let smart_manual = matches!(sort.order_by, OrderBy::Manual);
    if let (Scope::Smart { id }, true) = (scope, smart_manual) {
        qb.push(" LEFT JOIN smart_folder_order sfo ON sfo.asset_id = a.id AND sfo.smart_folder_id = ")
            .push_bind(id.as_str());
    }

    let mut written = 0usize;

    // 0. The Trash line, written FIRST and unconditionally.
    //
    // Every scope except Trash hides deleted assets, and Trash shows nothing
    // else. Placed outside the match on purpose: as an arm it would be one more
    // thing a newly added scope has to remember, and forgetting it leaks deleted
    // assets into a view rather than failing loudly.
    conjunct(&mut qb, &mut written);
    qb.push(if matches!(scope, Scope::Trash) {
        "a.deleted_at IS NOT NULL"
    } else {
        "a.deleted_at IS NULL"
    });

    // 1. Scope — which rows exist at all.
    match scope {
        Scope::All | Scope::Trash => {}
        Scope::Folder { id } => {
            conjunct(&mut qb, &mut written);
            qb.push("af.folder_id = ").push_bind(id.as_str());
        }
        Scope::Uncategorized => {
            conjunct(&mut qb, &mut written);
            qb.push("a.id NOT IN (SELECT asset_id FROM assets_folders)");
        }
        // The smart folder's own rules ARE the scope. Note they land in the same
        // WHERE as the filter tree below and simply AND with it — that's the
        // whole composition story, and it costs no extra machinery.
        //
        // A missing or empty tree means "everything": a smart folder whose rules
        // failed to load must not look like an empty folder, which would read as
        // data loss rather than a broken rule.
        Scope::Smart { .. } => {
            if let Some(rules) = scope_rules {
                if rules.is_active() {
                    conjunct(&mut qb, &mut written);
                    rules.push_predicate(&mut qb);
                }
            }
        }
        // A group is the union of its members, and the union of NOTHING is
        // empty — so an empty group shows nothing rather than everything.
        //
        // That's the opposite of the rule above, deliberately. An empty rule
        // GROUP in the editor is a half-written filter, where showing the
        // library is the forgiving reading. An empty smart-folder group is a
        // container the user hasn't filled: showing them the whole library
        // would claim those assets are in a group that is demonstrably empty.
        Scope::SmartGroup { .. } => {
            conjunct(&mut qb, &mut written);
            match scope_rules {
                Some(rules) if rules.is_active() => rules.push_predicate(&mut qb),
                _ => {
                    qb.push("0");
                }
            }
        }
    }

    // 2. Filters — narrowing within that scope.
    //
    // ONE call, because every structural condition now lives in the rule tree
    // (see the `rules` module). The filter bar builds a flat tree, a smart
    // folder builds a nested one, and both arrive here as the same thing —
    // which is what makes "smart folder AND saved filter" compose for free.
    if let Some(rules) = &filters.rules {
        if rules.is_active() {
            conjunct(&mut qb, &mut written);
            rules.push_predicate(&mut qb);
        }
    }

    // 3. Full-text search. Stays outside the tree: it's the live search box, not
    // a stored condition, and it's the one lens that never gets saved.
    //
    // The parser (search::query) turns user input into a SAFE FTS5 MATCH string;
    // we only ever bind that string, never raw input. An Include narrows to
    // matching assets, an Exclude (the "-term" only case, which FTS5 can't
    // express as a positive MATCH) removes them.
    if let Some(text) = &filters.text {
        if !text.is_blank() {
            match text.compile() {
                crate::search::query::Compiled::Empty => {}
                crate::search::query::Compiled::Include(expr) => {
                    conjunct(&mut qb, &mut written);
                    qb.push(
                        "a.id IN (SELECT asset_id FROM search_index WHERE search_index MATCH ",
                    )
                    .push_bind(expr)
                    .push(")");
                }
                crate::search::query::Compiled::Exclude(expr) => {
                    conjunct(&mut qb, &mut written);
                    qb.push(
                        "a.id NOT IN (SELECT asset_id FROM search_index WHERE search_index MATCH ",
                    )
                    .push_bind(expr)
                    .push(")");
                }
            }
        }
    }

    // 3. Sort. The `a.id` tie-break runs in the SAME direction as the sort column
    //    so the composite (col, id) indexes stay usable scanning either way.
    let dir = if sort.is_ascending { " ASC" } else { " DESC" };

    if matches!(scope, Scope::Smart { .. }) && smart_manual {
        // Ranked block first, then everything nobody has placed yet.
        //
        // `(sfo.position IS NULL) ASC` is NOT subject to `dir`: unranked means
        // "not yet given a place", which belongs at the end whichever way the
        // ranked block runs. Reversing it would scatter new arrivals through the
        // order the user built.
        //
        // The tail's own order is newest-first — the library default — so a
        // fresh match appears where you'd look for a new asset.
        qb.push(" ORDER BY (sfo.position IS NULL) ASC, sfo.position")
            .push(dir)
            .push(", a.imported_date DESC, a.id")
            .push(dir);
    } else {
        qb.push(" ORDER BY ")
            .push(sort.order_by.sql_expr(in_folder))
            .push(dir)
            .push(", a.id")
            .push(dir);
    }

    qb
}

// ── Smart folder groups ───────────────────────────────────────────────────────
//
// A container in the sidebar that is ALSO a place: clicking one browses the
// union of its members. That's why it owns a sort (in `view_settings` under
// `smartgroup:<id>`) — everything except manual, which a union can't answer.

#[derive(Serialize, Debug, Clone, FromRow)]
pub struct SmartFolderGroup {
    pub id: String,
    pub name: String,
    pub notes: Option<String>,
    pub position: f64,
}

#[instrument(skip(pool))]
pub async fn fetch_smart_folder_groups(pool: &SqlitePool) -> Result<Vec<SmartFolderGroup>> {
    sqlx::query_as::<_, SmartFolderGroup>(
        "SELECT id, name, notes, position FROM rule_set_groups ORDER BY position, name",
    )
    .fetch_all(pool)
    .await
    .context("Failed to fetch smart folder groups")
}

#[instrument(skip(pool))]
pub async fn create_smart_folder_group(pool: &SqlitePool, name: &str) -> Result<SmartFolderGroup> {
    let name = clean_name(name).context("A group needs a name")?;
    let id = uuid::Uuid::new_v4().to_string();
    let position = sqlx::query_scalar::<_, Option<f64>>("SELECT MAX(position) FROM rule_set_groups")
        .fetch_one(pool)
        .await
        .context("Failed to compute group position")?
        .map(|m| m + 1.0)
        .unwrap_or(0.0);

    sqlx::query(
        "INSERT INTO rule_set_groups (id, name, position, created_at) VALUES (?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&name)
    .bind(position)
    .bind(stamp(Utc::now()))
    .execute(pool)
    .await
    .context("Failed to insert group")?;

    Ok(SmartFolderGroup {
        id,
        name,
        notes: None,
        position,
    })
}

#[instrument(skip(pool))]
pub async fn rename_smart_folder_group(pool: &SqlitePool, id: &str, name: &str) -> Result<()> {
    let name = clean_name(name).context("A group needs a name")?;
    let res = sqlx::query("UPDATE rule_set_groups SET name = ? WHERE id = ?")
        .bind(&name)
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to rename group")?;
    if res.rows_affected() == 0 {
        reject!("Group not found");
    }
    Ok(())
}

/// Delete a group. Its members are UNGROUPED, never deleted — the FK is
/// `ON DELETE SET NULL`, because removing a container must not destroy the
/// user's saved queries.
#[instrument(skip(pool))]
pub async fn delete_smart_folder_group(pool: &SqlitePool, id: &str) -> Result<()> {
    let mut tx = pool.begin().await.context("Failed to begin group delete")?;

    sqlx::query("DELETE FROM rule_set_groups WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await
        .context("Failed to delete group")?;

    // Same hand-cleanup as a smart folder: `view_settings` is a key-value table
    // with no foreign key to cascade.
    sqlx::query("DELETE FROM view_settings WHERE view_key = ?")
        .bind(format!("smartgroup:{id}"))
        .execute(&mut *tx)
        .await
        .context("Failed to clear group sort")?;

    tx.commit().await.context("Failed to commit group delete")?;
    Ok(())
}

/// Move a smart folder into a group, or out of every group with `None`.
#[instrument(skip(pool))]
pub async fn set_smart_folder_group(
    pool: &SqlitePool,
    id: &str,
    group_id: Option<&str>,
) -> Result<()> {
    let res = sqlx::query("UPDATE rule_sets SET group_id = ? WHERE id = ? AND kind = 'smart'")
        .bind(group_id)
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to move smart folder")?;
    if res.rows_affected() == 0 {
        reject!("Smart folder not found");
    }
    Ok(())
}

// ── Manual order inside a smart folder ────────────────────────────────────────
//
// The rank lives in `smart_folder_order`, sparse: only assets someone has placed
// have a row. Two operations keep it honest, and both are lazy on purpose —
// there is no "stopped matching" event to hang eager maintenance on, and
// `within_last` rules stop matching at midnight with no mutation at all.

/// Drop ranks for assets that no longer match. Called when the folder is opened
/// in manual order, and after its rules change.
///
/// Lazy rather than eager: evaluating every smart folder's predicate on every
/// tag/folder/asset mutation would put unbounded query cost on the write path,
/// and it still wouldn't catch time-relative rules. The only moment a stale rank
/// is observable is when the asset comes back — so we prune where it would show.
#[instrument(skip(pool, rules))]
async fn prune_smart_order(
    pool: &SqlitePool,
    smart_folder_id: &str,
    rules: &crate::rules::RuleNode,
) -> Result<()> {
    let mut qb = QueryBuilder::<Sqlite>::new(
        "DELETE FROM smart_folder_order WHERE smart_folder_id = ",
    );
    qb.push_bind(smart_folder_id)
        .push(" AND asset_id NOT IN (SELECT a.id FROM assets a");
    if rules.is_active() {
        qb.push(" WHERE ");
        rules.push_predicate(&mut qb);
    }
    qb.push(")");

    qb.build()
        .execute(pool)
        .await
        .context("Failed to prune smart folder order")?;
    Ok(())
}

/// Give every CURRENT member a rank, appending any that lack one.
///
/// Runs before a reorder because the fractional-rank algorithm needs a `prev`
/// and `next` to bisect between, and NULLs have no place in that arithmetic.
/// New members are appended after the current maximum in the same order the
/// unranked tail displays them, so materialising never visibly reshuffles
/// anything — what you saw is what gets numbered.
async fn materialize_smart_order(
    tx: &mut sqlx::SqliteConnection,
    smart_folder_id: &str,
    rules: &crate::rules::RuleNode,
) -> Result<()> {
    let mut qb = QueryBuilder::<Sqlite>::new(
        "INSERT INTO smart_folder_order (smart_folder_id, asset_id, position) SELECT ",
    );
    qb.push_bind(smart_folder_id)
        .push(", a.id, (SELECT COALESCE(MAX(position), -1.0) FROM smart_folder_order WHERE smart_folder_id = ")
        .push_bind(smart_folder_id)
        .push(") + ROW_NUMBER() OVER (ORDER BY a.imported_date DESC, a.id) FROM assets a WHERE a.id NOT IN \
               (SELECT asset_id FROM smart_folder_order WHERE smart_folder_id = ")
        .push_bind(smart_folder_id)
        .push(")");
    if rules.is_active() {
        qb.push(" AND ");
        rules.push_predicate(&mut qb);
    }

    qb.build()
        .execute(&mut *tx)
        .await
        .context("Failed to materialize smart folder order")?;
    Ok(())
}

/// Load the rule tree that DEFINES a smart folder scope, if the scope is one.
///
/// A scope that names a smart folder which no longer exists resolves to `None`
/// rather than failing: the grid shows everything and the sidebar has already
/// dropped the entry, which is recoverable. Erroring here would leave the user
/// staring at a toast with no view at all.
pub(crate) async fn resolve_scope_rules(
    pool: &SqlitePool,
    scope: &Scope,
) -> Result<Option<crate::rules::RuleNode>> {
    // A group resolves to ONE tree: `any` over its members. Every downstream
    // consumer — the manifest, the count, the manual-order prune — then treats a
    // group exactly like a single smart folder, because by this point it is one.
    if let Scope::SmartGroup { id } = scope {
        let rows: Vec<(String, i64, String)> = sqlx::query_as(
            "SELECT id, version, query_json FROM rule_sets \
             WHERE kind = 'smart' AND group_id = ? ORDER BY position, name",
        )
        .bind(id.as_str())
        .fetch_all(pool)
        .await
        .context("Failed to read smart folder group")?;

        let children: Vec<crate::rules::RuleNode> = rows
            .into_iter()
            .filter_map(|(member, version, json)| {
                if version > RULE_SET_VERSION {
                    warn!(id = %member, version, "Group member is from a newer version; skipping");
                    return None;
                }
                serde_json::from_str(&json)
                    .inspect_err(|e| warn!(id = %member, error = %e, "Group member has unreadable rules"))
                    .ok()
            })
            .collect();

        if children.is_empty() {
            return Ok(None); // caller renders this as "nothing", not "everything"
        }
        return Ok(Some(crate::rules::RuleNode::Group {
            op: crate::rules::GroupOp::Any,
            children,
        }));
    }

    let Scope::Smart { id } = scope else {
        return Ok(None);
    };
    let row: Option<(i64, String)> =
        sqlx::query_as("SELECT version, query_json FROM rule_sets WHERE id = ? AND kind = 'smart'")
            .bind(id.as_str())
            .fetch_optional(pool)
            .await
            .context("Failed to read smart folder rules")?;

    let Some((version, query_json)) = row else {
        warn!(id = %id, "Smart folder scope names a row that no longer exists");
        return Ok(None);
    };
    if version > RULE_SET_VERSION {
        warn!(id = %id, version, "Smart folder is from a newer version; showing everything");
        return Ok(None);
    }
    match serde_json::from_str::<crate::rules::RuleNode>(&query_json) {
        Ok(rules) => Ok(Some(rules)),
        Err(e) => {
            warn!(id = %id, error = %e, "Smart folder has unreadable rules");
            Ok(None)
        }
    }
}

/// Stream the manifest to `sink` in batches of `chunk_size`, newest batch first.
///
/// Rows are pulled off the SQLite cursor and handed over as they arrive, rather
/// than collected and then sliced. At 100,000 assets that is the difference
/// between the grid painting after the whole result set has been decoded (~20 MB
/// materialised, and cloned again per chunk on the way out) and painting as soon
/// as the first batch lands, with peak memory bounded by one batch.
///
/// `sink` returns whether to KEEP GOING. Returning `false` abandons the rest of
/// the cursor — that is how a superseded request stops paying for rows nobody
/// will look at, and it is a normal outcome rather than an error.
#[instrument(skip(pool, sink))]
pub async fn stream_manifest<F>(
    pool: &SqlitePool,
    query: &ManifestQuery,
    chunk_size: usize,
    mut sink: F,
) -> Result<usize>
where
    F: FnMut(Vec<AssetLightRow>) -> Result<bool>,
{
    let sort = match query.sort {
        Some(s) => s,
        None => resolve_sort(pool, &query.scope).await?,
    };
    let scope_rules = resolve_scope_rules(pool, &query.scope).await?;

    // Prune stale ranks where they'd be seen: opening a smart folder in manual
    // order is the only moment a rank for a no-longer-matching asset can affect
    // anything. Cheap (one DELETE against a small, indexed table) and skipped
    // entirely under every other sort, where dead rows are invisible.
    if let (Scope::Smart { id }, OrderBy::Manual, Some(rules)) =
        (&query.scope, sort.order_by, scope_rules.as_ref())
    {
        prune_smart_order(pool, id, rules).await?;
    }

    // `qb` borrows `scope_rules`, and the cursor borrows `qb` — so both have to
    // outlive the loop. Declaring them in this order does that: locals drop in
    // reverse, so the cursor goes first and the tree it depends on goes last.
    let mut qb = build_manifest_query(&query.scope, scope_rules.as_ref(), &query.filters, sort);
    let mut cursor = qb.build_query_as::<AssetLightRow>().fetch(pool);

    let mut buf: Vec<AssetLightRow> = Vec::with_capacity(chunk_size);
    let mut total = 0usize;

    while let Some(row) = cursor
        .try_next()
        .await
        .context("Failed to read the asset manifest")?
    {
        buf.push(row);
        if buf.len() >= chunk_size {
            total += buf.len();
            // `take` hands the buffer over whole — no clone of the batch, which
            // is what the old `chunk.to_vec()` was paying for.
            if !sink(std::mem::take(&mut buf))? {
                return Ok(total);
            }
            buf.reserve(chunk_size);
        }
    }

    // The partial tail. A result set of ZERO sends nothing at all, which is
    // deliberate and is the contract the frontend already expects — it treats
    // "the invoke resolved having seen no batches" as a legitimate empty result
    // and swaps to an empty grid on that basis. Emitting an empty batch here
    // would be a second way to say the same thing, and the caller would have to
    // handle both.
    if !buf.is_empty() {
        total += buf.len();
        sink(buf)?;
    }

    Ok(total)
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
    let row: Option<(OrderBy, bool)> = match scope.sort_home() {
        SortHome::FolderRow(id) => {
            sqlx::query_as(FOLDER_SORT_SQL)
                .bind(id)
                .fetch_optional(pool)
                .await
        }
        SortHome::ViewKey(key) => {
            sqlx::query_as(VIEW_SORT_SQL)
                .bind(key)
                .fetch_optional(pool)
                .await
        }
    }
    .context("Failed to read persisted sort")?;

    // A missing row is not an error — the user still gets a usable view.
    let sort = row
        .map(|(order_by, is_ascending)| Sort {
            order_by,
            is_ascending,
        })
        .unwrap_or(DEFAULT_SORT);

    // A group has no manual order: its contents are a union of queries the user
    // never arranged as one list, and dragging inside it would rewrite ranks
    // across several smart folders at once. The UI hides the option; this
    // coerces it anyway, because `view_settings` is user-editable data and a
    // stored `manual` would otherwise reach a code path that can't honour it.
    if matches!(scope, Scope::SmartGroup { .. }) && matches!(sort.order_by, OrderBy::Manual) {
        return Ok(DEFAULT_SORT);
    }
    Ok(sort)
}

#[instrument(skip(pool))]
pub async fn set_sort(pool: &SqlitePool, scope: &Scope, sort: Sort) -> Result<()> {
    let res = match scope.sort_home() {
        SortHome::FolderRow(id) => {
            sqlx::query("UPDATE folders SET order_by = ?, is_ascending = ? WHERE id = ?")
                .bind(sort.order_by)
                .bind(sort.is_ascending)
                .bind(id)
                .execute(pool)
                .await
        }
        // Upsert, so a view keeps working even with no seed row — which is
        // always the case for a smart folder, whose key is minted on first sort.
        SortHome::ViewKey(key) => {
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
        reject!("Folder not found");
    }
    Ok(())
}

/// Reorder assets within a scope's MANUAL sort by dropping a block of them at a
/// new spot.
///
/// The column written depends on the scope, mirroring how `OrderBy::Manual`
/// reads it back:
///   * a folder writes `assets_folders.position` — order LOCAL to that folder;
///   * "All" and "Uncategorized" write `assets.manual_position` — the GLOBAL
///     order, so a reorder in one is visible in the other. That's a real
///     coupling, surfaced to the user, not a bug.
///
/// `after` is the asset the block lands immediately behind, or `None` for the
/// head. The block keeps the visible order of its members. The neighbours'
/// stored positions are split, so this writes only the moved rows — O(block),
/// not O(scope) — except when the fractional gap is exhausted and the scope is
/// renumbered.
///
/// Reading the FULL stored order here (not the filtered view) is what makes a
/// reorder under an active filter land correctly: the block is midpointed
/// against whatever truly sits on either side, hidden rows included.
#[instrument(skip(pool, moved_ids), fields(scope = ?scope, moved = moved_ids.len()))]
pub async fn reorder_assets(
    pool: &SqlitePool,
    scope: &Scope,
    moved_ids: &[String],
    after: Option<&str>,
) -> Result<()> {
    if moved_ids.is_empty() {
        return Ok(());
    }

    let mut tx = pool
        .begin()
        .await
        .context("Failed to begin reorder transaction")?;

    // The scope's assets in stored manual order, as (id, position).
    let ordered: Vec<(String, f64)> = match scope {
        Scope::Folder { id } => sqlx::query_as(
            "SELECT asset_id, position FROM assets_folders
             WHERE folder_id = ? ORDER BY position, asset_id",
        )
        .bind(id.as_str())
        .fetch_all(&mut *tx)
        .await
        .context("Failed to read folder order")?,
        // `deleted_at IS NULL` here as well as in the manifest: a trashed asset
        // must not hold a rank in the order the live ones are bisected against,
        // or restoring it would drop it at an arbitrary point.
        Scope::All => sqlx::query_as(
            "SELECT id, manual_position FROM assets WHERE deleted_at IS NULL \
             ORDER BY manual_position, id",
        )
        .fetch_all(&mut *tx)
        .await
        .context("Failed to read global order")?,
        Scope::Uncategorized => sqlx::query_as(
            "SELECT id, manual_position FROM assets a
             WHERE a.deleted_at IS NULL
               AND NOT EXISTS (SELECT 1 FROM assets_folders af WHERE af.asset_id = a.id)
             ORDER BY manual_position, id",
        )
        .fetch_all(&mut *tx)
        .await
        .context("Failed to read uncategorized order")?,
        // The Trash is a holding area, not an arrangement.
        Scope::Trash => reject!("The Trash can't be reordered"),
        // A smart folder's rank lives in its own table, so that every folder
        // orders independently — reusing the global `manual_position` (as All
        // and Uncategorized do) would make dragging in one smart folder
        // silently reshuffle every other view.
        Scope::Smart { id } => {
            let rules = resolve_scope_rules(pool, scope)
                .await?
                .unwrap_or_else(crate::rules::RuleNode::empty);
            // Every member needs a rank before the bisection below can run.
            materialize_smart_order(&mut tx, id, &rules).await?;

            let mut qb = QueryBuilder::<Sqlite>::new(
                "SELECT sfo.asset_id, sfo.position FROM smart_folder_order sfo \
                 JOIN assets a ON a.id = sfo.asset_id WHERE sfo.smart_folder_id = ",
            );
            qb.push_bind(id.as_str());
            // Restricted to current members: stale ranks are pruned lazily, so
            // the list this algorithm bisects must exclude them explicitly or a
            // departed asset could become somebody's `prev`.
            if rules.is_active() {
                qb.push(" AND ");
                rules.push_predicate(&mut qb);
            }
            qb.push(" ORDER BY sfo.position, sfo.asset_id");

            qb.build_query_as()
                .fetch_all(&mut *tx)
                .await
                .context("Failed to read smart folder order")?
        }
        // Refused rather than supported: see the coercion in `resolve_sort`.
        // A group's order is a union of several folders' orders, and there is no
        // answer to "where does this land" that doesn't rewrite somebody else's.
        Scope::SmartGroup { .. } => {
            reject!("A group of smart folders has no manual order");
        }
    };

    let moved_set: std::collections::HashSet<&str> =
        moved_ids.iter().map(String::as_str).collect();
    // Membership index over the SCOPE, for the stale-selection pass below.
    // `ordered` is the whole scope — every asset in the library under
    // `Scope::All` — so testing each moved id against it with a linear scan was
    // O(moved x scope): dragging a 10k selection in a 100k library came to 10^9
    // string comparisons and a multi-second freeze. Built once, used once, and
    // the set above already does the same job in the other direction.
    let in_scope: std::collections::HashSet<&str> =
        ordered.iter().map(|(id, _)| id.as_str()).collect();

    // The block keeps the scope's own order, not the order the ids arrived in —
    // a selection is a set, so its click order is meaningless for placement.
    let mut moved: Vec<&str> = ordered
        .iter()
        .map(|(id, _)| id.as_str())
        .filter(|id| moved_set.contains(id))
        .collect();
    // Any moved id not present in the scope (a stale selection) still gets
    // placed, appended after the ones that were found.
    for id in moved_ids {
        if !in_scope.contains(id.as_str()) {
            moved.push(id.as_str());
        }
    }

    // Everything staying put, in order, with the positions we'll split.
    let remaining: Vec<(&str, f64)> = ordered
        .iter()
        .filter(|(id, _)| !moved_set.contains(id.as_str()))
        .map(|(id, pos)| (id.as_str(), *pos))
        .collect();

    // Where the block lands among the remaining rows.
    let insert_at = match after {
        Some(a) => remaining
            .iter()
            .position(|(id, _)| *id == a)
            .map(|i| i + 1)
            .unwrap_or(remaining.len()),
        None => 0,
    };

    let lower = insert_at.checked_sub(1).map(|i| remaining[i].1);
    let upper = remaining.get(insert_at).map(|(_, p)| *p);
    let n = moved.len();

    // Enough room to fan N ranks between the neighbours?
    let positions: Option<Vec<f64>> = match (lower, upper) {
        (None, None) => Some((0..n).map(|i| i as f64).collect()),
        (None, Some(u)) => Some((0..n).map(|i| u - (n - i) as f64).collect()),
        (Some(l), None) => Some((0..n).map(|i| l + 1.0 + i as f64).collect()),
        (Some(l), Some(u)) if u - l > POSITION_EPSILON * (n as f64 + 1.0) => {
            let step = (u - l) / (n as f64 + 1.0);
            Some((0..n).map(|i| l + step * (i as f64 + 1.0)).collect())
        }
        (Some(_), Some(_)) => None, // gap exhausted → renumber below
    };

    match positions {
        Some(positions) => {
            let writes: Vec<(&str, f64)> =
                moved.iter().copied().zip(positions).collect();
            write_manual_positions(&mut tx, scope, &writes).await?;
        }
        None => {
            // Rebuild the whole scope 0,1,2… in the intended final order. Rare,
            // and the only way to guarantee distinct ranks once the doubles
            // between two neighbours are used up.
            tracing::info!(scope = ?scope, "Manual positions exhausted; renumbering scope");
            let writes: Vec<(&str, f64)> = remaining[..insert_at]
                .iter()
                .map(|(id, _)| *id)
                .chain(moved.iter().copied())
                .chain(remaining[insert_at..].iter().map(|(id, _)| *id))
                .enumerate()
                .map(|(rank, id)| (id, rank as f64))
                .collect();
            write_manual_positions(&mut tx, scope, &writes).await?;
        }
    }

    tx.commit()
        .await
        .context("Failed to commit reorder transaction")?;
    Ok(())
}

/// Write manual ranks into whichever column the scope orders by.
///
/// Set-based, because BOTH callers can be large. The renumber fallback rewrites
/// the entire scope — every asset in the library under `Scope::All` — and even
/// the ordinary bisecting path writes one row per moved asset, which is the
/// whole selection when someone drags a Ctrl+A. A statement each meant a round
/// trip each.
///
/// The dispatch stays per-variant because the TARGET differs: a folder's rank
/// lives on the membership row, All/Uncategorized share the asset-level column,
/// and a smart folder has its own sparse table. There is no shared statement to
/// factor out — only a shared shape.
async fn write_manual_positions(
    tx: &mut sqlx::SqliteConnection,
    scope: &Scope,
    writes: &[(&str, f64)],
) -> Result<()> {
    if writes.is_empty() {
        return Ok(());
    }
    // Three binds per row in the widest arm; sized against SQLite's 32766 cap
    // like every other batched statement here.
    const ROWS_PER_STATEMENT: usize = 8000;

    for chunk in writes.chunks(ROWS_PER_STATEMENT) {
        match scope {
            // A bare SQLite VALUES subquery names its columns column1, column2 —
            // there is no `AS v(id, pos)` syntax to lean on.
            Scope::Folder { id } => {
                let mut qb =
                    QueryBuilder::<Sqlite>::new("UPDATE assets_folders SET position = v.column2 FROM (");
                qb.push_values(chunk, |mut b, (asset_id, position)| {
                    b.push_bind(*asset_id).push_bind(*position);
                });
                qb.push(") AS v WHERE assets_folders.asset_id = v.column1 AND assets_folders.folder_id = ")
                    .push_bind(id.as_str());
                qb.build()
                    .execute(&mut *tx)
                    .await
                    .context("Failed to write folder positions")?;
            }
            Scope::All | Scope::Uncategorized => {
                let mut qb =
                    QueryBuilder::<Sqlite>::new("UPDATE assets SET manual_position = v.column2 FROM (");
                qb.push_values(chunk, |mut b, (asset_id, position)| {
                    b.push_bind(*asset_id).push_bind(*position);
                });
                qb.push(") AS v WHERE assets.id = v.column1");
                qb.build()
                    .execute(&mut *tx)
                    .await
                    .context("Failed to write manual positions")?;
            }
            Scope::Trash => reject!("The Trash can't be reordered"),
            // Upsert: `materialize_smart_order` has already given every current
            // member a row, but an UPDATE that silently affected zero rows would
            // be the kind of failure that looks like "the drag didn't take".
            Scope::Smart { id } => {
                let mut qb = QueryBuilder::<Sqlite>::new(
                    "INSERT INTO smart_folder_order (smart_folder_id, asset_id, position) ",
                );
                qb.push_values(chunk, |mut b, (asset_id, position)| {
                    b.push_bind(id.as_str())
                        .push_bind(*asset_id)
                        .push_bind(*position);
                });
                qb.push(
                    " ON CONFLICT(smart_folder_id, asset_id) DO UPDATE SET position = excluded.position",
                );
                qb.build()
                    .execute(&mut *tx)
                    .await
                    .context("Failed to write smart folder positions")?;
            }
            // Unreachable in practice — `reorder_assets` bails before opening the
            // transaction — but stated rather than swallowed by a catch-all, so the
            // next scope variant gets a compile error here instead of a silent
            // write to the wrong column.
            Scope::SmartGroup { .. } => {
                reject!("A group of smart folders has no manual order");
            }
        }
    }
    Ok(())
}

#[instrument(skip(pool))]
pub async fn fetch_folders(pool: &SqlitePool) -> Result<Vec<Folder>> {
    let folders = sqlx::query_as::<_, Folder>(
        "SELECT id, name, parent_id, position, order_by, is_ascending, notes, created_at,
                color, pin_position
         FROM folders
         ORDER BY parent_id, position, name",
    )
    .fetch_all(pool)
    .await
    .context("Failed to fetch folders")?;
    Ok(folders)
}

// ── Rule sets: saved filters and (from Phase 2) smart folders ─────────────────
//
// One table, one document format, one compiler — see the `rules` module and the
// `rule_sets` comment in the migration. `kind` is the only difference: a filter
// is a LENS applied to whatever scope you're in, a smart folder is a PLACE that
// owns a sort. Everything below is the filter half; the smart-folder half reuses
// these same rows.

/// Bump when the stored rule document changes shape incompatibly, and migrate
/// existing rows deliberately at the same time.
///
/// v1 = the flat `FilterSet` (dimension per field, all ANDed).
/// v2 = the `RuleNode` tree.
pub(crate) const RULE_SET_VERSION: i64 = 2;

/// A named, reusable rule set as the frontend sees it — the JSON document is
/// already parsed, so callers never touch the stored representation.
#[derive(Serialize, Debug, Clone)]
pub struct SavedFilter {
    pub id: String,
    pub name: String,
    pub position: f64,
    pub rules: crate::rules::RuleNode,
}

/// Storage shape, with `query_json` still a string.
#[derive(FromRow)]
struct RuleSetRow {
    id: String,
    name: String,
    position: f64,
    version: i64,
    query_json: String,
}

#[instrument(skip(pool))]
pub async fn fetch_saved_filters(pool: &SqlitePool) -> Result<Vec<SavedFilter>> {
    let rows = sqlx::query_as::<_, RuleSetRow>(
        "SELECT id, name, position, version, query_json FROM rule_sets \
         WHERE kind = 'filter' ORDER BY position, name",
    )
    .fetch_all(pool)
    .await
    .context("Failed to fetch saved filters")?;

    // An unreadable row is skipped, never fatal: one filter written by a newer
    // build (or corrupted) must not take the user's whole list down with it.
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            if r.version > RULE_SET_VERSION {
                warn!(id = %r.id, version = r.version, "Saved filter is from a newer version; skipping");
                return None;
            }
            match serde_json::from_str::<crate::rules::RuleNode>(&r.query_json) {
                Ok(rules) => Some(SavedFilter {
                    id: r.id,
                    name: r.name,
                    position: r.position,
                    rules,
                }),
                Err(e) => {
                    warn!(id = %r.id, error = %e, "Saved filter has unreadable query_json; skipping");
                    None
                }
            }
        })
        .collect())
}

/// Serialize a tree for storage, rejecting one the editor could never have made.
///
/// Stored documents are user data — a hand-edited library.db could carry a tree
/// nested a hundred deep, which would compile to unbounded SQL.
fn encode_rules(rules: &crate::rules::RuleNode) -> Result<String> {
    rules.validate()?;
    serde_json::to_string(rules).context("Failed to serialize rule set")
}

#[instrument(skip(pool, filters))]
pub async fn create_saved_filter(
    pool: &SqlitePool,
    name: &str,
    filters: &FilterSet,
) -> Result<SavedFilter> {
    // Same cleaning every other named row gets — a saved filter shows up in the
    // filter bar's list, so an unreadable name is as bad here as anywhere.
    let name = clean_name(name).context("A saved filter needs a name")?;
    let id = uuid::Uuid::new_v4().to_string();
    // The live search text isn't stripped here any more — it structurally can't
    // reach storage, because it was never part of the tree. That's one of the
    // things splitting FilterSet into `rules` + `text` bought.
    let rules = filters
        .rules
        .clone()
        .unwrap_or_else(crate::rules::RuleNode::empty);
    let query_json = encode_rules(&rules)?;

    let position = sqlx::query_scalar::<_, Option<f64>>(
        "SELECT MAX(position) FROM rule_sets WHERE kind = 'filter'",
    )
    .fetch_one(pool)
    .await
    .context("Failed to compute saved filter position")?
    .map(|m| m + 1.0)
    .unwrap_or(0.0);

    sqlx::query(
        "INSERT INTO rule_sets (id, kind, name, position, version, query_json, created_at) \
         VALUES (?, 'filter', ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&name)
    .bind(position)
    .bind(RULE_SET_VERSION)
    .bind(&query_json)
    .bind(stamp(Utc::now()))
    .execute(pool)
    .await
    .context("Failed to insert saved filter")?;

    Ok(SavedFilter {
        id,
        name,
        position,
        rules,
    })
}

#[instrument(skip(pool))]
pub async fn rename_saved_filter(pool: &SqlitePool, id: &str, name: &str) -> Result<()> {
    let name = clean_name(name).context("A saved filter needs a name")?;
    let res = sqlx::query("UPDATE rule_sets SET name = ? WHERE id = ?")
        .bind(&name)
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to rename saved filter")?;
    if res.rows_affected() == 0 {
        reject!("Saved filter not found");
    }
    Ok(())
}

/// Overwrite a saved filter's definition ("update to current"). Also rewrites
/// `version`, so a document saved by an older build is brought forward rather
/// than left behind at its original version.
#[instrument(skip(pool, filters))]
pub async fn update_saved_filter(pool: &SqlitePool, id: &str, filters: &FilterSet) -> Result<()> {
    let rules = filters
        .rules
        .clone()
        .unwrap_or_else(crate::rules::RuleNode::empty);
    let query_json = encode_rules(&rules)?;

    let res = sqlx::query("UPDATE rule_sets SET query_json = ?, version = ? WHERE id = ?")
        .bind(&query_json)
        .bind(RULE_SET_VERSION)
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to update saved filter")?;
    if res.rows_affected() == 0 {
        reject!("Saved filter not found");
    }
    Ok(())
}

#[instrument(skip(pool))]
pub async fn delete_saved_filter(pool: &SqlitePool, id: &str) -> Result<()> {
    sqlx::query("DELETE FROM rule_sets WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to delete saved filter")?;
    Ok(())
}

// ── Smart folders ─────────────────────────────────────────────────────────────
//
// The same rows as saved filters, with `kind = 'smart'`. What differs is how the
// app treats them: a smart folder is a PLACE (its tree becomes the scope
// predicate, and it owns a persisted sort under `view_settings`), where a filter
// is a lens you apply wherever you already are.

/// A smart folder as the sidebar sees it.
#[derive(Serialize, Debug, Clone)]
pub struct SmartFolder {
    pub id: String,
    pub name: String,
    pub notes: Option<String>,
    pub group_id: Option<String>,
    pub position: f64,
    pub rules: crate::rules::RuleNode,
    pub color: Option<String>,
    pub pin_position: Option<f64>,
}

#[derive(FromRow)]
struct SmartFolderRow {
    id: String,
    name: String,
    notes: Option<String>,
    group_id: Option<String>,
    position: f64,
    version: i64,
    query_json: String,
    color: Option<String>,
    pin_position: Option<f64>,
}

#[instrument(skip(pool))]
pub async fn fetch_smart_folders(pool: &SqlitePool) -> Result<Vec<SmartFolder>> {
    let rows = sqlx::query_as::<_, SmartFolderRow>(
        "SELECT id, name, notes, group_id, position, version, query_json, color, pin_position \
         FROM rule_sets WHERE kind = 'smart' ORDER BY position, name",
    )
    .fetch_all(pool)
    .await
    .context("Failed to fetch smart folders")?;

    // An unreadable row is skipped, never fatal — one bad document must not take
    // the whole sidebar down with it.
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            if r.version > RULE_SET_VERSION {
                warn!(id = %r.id, version = r.version, "Smart folder is from a newer version; skipping");
                return None;
            }
            match serde_json::from_str::<crate::rules::RuleNode>(&r.query_json) {
                Ok(rules) => Some(SmartFolder {
                    id: r.id,
                    name: r.name,
                    notes: r.notes,
                    group_id: r.group_id,
                    position: r.position,
                    rules,
                    color: r.color,
                    pin_position: r.pin_position,
                }),
                Err(e) => {
                    warn!(id = %r.id, error = %e, "Smart folder has unreadable query_json; skipping");
                    None
                }
            }
        })
        .collect())
}

#[instrument(skip(pool, rules))]
pub async fn create_smart_folder(
    pool: &SqlitePool,
    name: &str,
    rules: &crate::rules::RuleNode,
) -> Result<SmartFolder> {
    let name = clean_name(name).context("A smart folder needs a name")?;
    let query_json = encode_rules(rules)?;
    let id = uuid::Uuid::new_v4().to_string();

    let position = sqlx::query_scalar::<_, Option<f64>>(
        "SELECT MAX(position) FROM rule_sets WHERE kind = 'smart'",
    )
    .fetch_one(pool)
    .await
    .context("Failed to compute smart folder position")?
    .map(|m| m + 1.0)
    .unwrap_or(0.0);

    sqlx::query(
        "INSERT INTO rule_sets (id, kind, name, position, version, query_json, created_at) \
         VALUES (?, 'smart', ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&name)
    .bind(position)
    .bind(RULE_SET_VERSION)
    .bind(&query_json)
    .bind(stamp(Utc::now()))
    .execute(pool)
    .await
    .context("Failed to insert smart folder")?;

    Ok(SmartFolder {
        id,
        name,
        notes: None,
        group_id: None,
        position,
        rules: rules.clone(),
        color: None,
        pin_position: None,
    })
}

/// Partial update. `None` leaves a field alone; `Some` replaces it.
#[derive(Deserialize, Debug, Default)]
#[serde(default)]
pub struct SmartFolderPatch {
    pub name: Option<String>,
    pub notes: Option<String>,
    pub rules: Option<crate::rules::RuleNode>,
}

#[instrument(skip(pool, patch))]
pub async fn update_smart_folder(
    pool: &SqlitePool,
    id: &str,
    patch: SmartFolderPatch,
) -> Result<()> {
    let name = match patch.name {
        Some(raw) => Some(clean_name(&raw).context("A smart folder needs a name")?),
        None => None,
    };
    let notes = patch.notes.map(|s| blank_to_null(&s));
    let query_json = match &patch.rules {
        Some(rules) => Some(encode_rules(rules)?),
        None => None,
    };

    if name.is_none() && notes.is_none() && query_json.is_none() {
        return Ok(());
    }

    let mut qb = QueryBuilder::new("UPDATE rule_sets SET ");
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
        if let Some(v) = &query_json {
            sep.push("query_json = ");
            sep.push_bind_unseparated(v);
            // Rewritten rules are written in the CURRENT format, so a document
            // saved by an older build is brought forward rather than left
            // behind at its original version.
            sep.push("version = ");
            sep.push_bind_unseparated(RULE_SET_VERSION);
        }
    }
    qb.push(" WHERE id = ").push_bind(id).push(" AND kind = 'smart'");

    let res = qb
        .build()
        .execute(pool)
        .await
        .context("Failed to update smart folder")?;
    if res.rows_affected() == 0 {
        reject!("Smart folder not found");
    }

    // Editing the rules is the one deliberate act that can change membership
    // wholesale, and it's exactly when the user expects the folder to shift —
    // so prune here too rather than waiting for the next manual-order open.
    if let Some(rules) = &patch.rules {
        if let Err(e) = prune_smart_order(pool, id, rules).await {
            // Non-fatal: stale ranks are invisible under every other sort, and
            // failing the edit itself would be a worse trade.
            warn!(id = %id, error = %e, "Pruning smart folder order after a rule change failed");
        }
    }
    Ok(())
}

#[instrument(skip(pool))]
pub async fn delete_smart_folder(pool: &SqlitePool, id: &str) -> Result<()> {
    let mut tx = pool
        .begin()
        .await
        .context("Failed to begin smart folder delete")?;

    sqlx::query("DELETE FROM rule_sets WHERE id = ? AND kind = 'smart'")
        .bind(id)
        .execute(&mut *tx)
        .await
        .context("Failed to delete smart folder")?;

    // The sort lives in a key-value table with no foreign key to cascade, so it
    // has to be cleaned up by hand — otherwise a new smart folder that happened
    // to reuse the id would inherit a stranger's sort.
    sqlx::query("DELETE FROM view_settings WHERE view_key = ?")
        .bind(format!("smart:{id}"))
        .execute(&mut *tx)
        .await
        .context("Failed to clear smart folder sort")?;

    tx.commit()
        .await
        .context("Failed to commit smart folder delete")?;
    Ok(())
}

/// How many assets a rule tree currently matches.
///
/// Powers the editor's live count, which is the only validation a rule set gets:
/// it tells you the rule is wrong (0 items, or 40,000) before you commit to it,
/// and it costs one COUNT through the very compiler that will serve the folder.
#[instrument(skip(pool, rules))]
pub async fn count_matching(pool: &SqlitePool, rules: &crate::rules::RuleNode) -> Result<i64> {
    // Trashed assets are excluded, because the count has to agree with what the
    // smart folder will actually show — and the manifest hides them.
    let mut qb = QueryBuilder::<Sqlite>::new("SELECT COUNT(*) FROM assets a WHERE a.deleted_at IS NULL");
    if rules.is_active() {
        qb.push(" AND ");
        rules.push_predicate(&mut qb);
    }
    qb.build_query_scalar()
        .fetch_one(pool)
        .await
        .context("Failed to count matching assets")
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
    /// Pin accent. `Some("")` clears it, like every other free-text field here.
    pub color: Option<String>,
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
pub(crate) fn clean_name(raw: &str) -> Result<String> {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '/' | '\\'))
        .collect();
    blank_to_null(&cleaned).ok_or_else(|| crate::error::rejected("Name cannot be empty"))
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
                .ok_or_else(|| crate::error::rejected("Asset not found"))?;
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
            reject!("Asset not found");
        }
    }

    // Name/notes/url are all searchable — refresh this asset's index row.
    let ids = [id.to_string()];
    if let Err(e) = crate::search::reindex_assets(pool, &ids).await {
        warn!(error = %e, asset = %id, "Reindex after asset edit failed (non-fatal)");
    }

    fetch_assets_by_ids(pool, root, &ids)
        .await?
        .pop()
        .ok_or_else(|| crate::error::rejected("Asset not found"))
}

#[instrument(skip(pool))]
pub async fn create_folder(
    pool: &SqlitePool,
    name: &str,
    parent_id: Option<&str>,
) -> Result<Folder> {
    // Cleaned here as well as in `update_folder`. Without this a folder could be
    // CREATED with a name it could never be RENAMED to — and the display name is
    // what an outbound drag hardlinks under, so "unnameable" is not cosmetic.
    let name = clean_name(name).context("A folder needs a name")?;
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
    .bind(&name)
    .bind(parent_id)
    .bind(position)
    .bind(&created_at)
    .execute(pool)
    .await
    .context("Failed to insert folder")?;

    Ok(Folder {
        id,
        name,
        parent_id: parent_id.map(str::to_string),
        position,
        order_by: OrderBy::Manual,
        is_ascending: true,
        notes: None,
        created_at,
        // A new folder is never pinned or coloured — pinning is always an
        // explicit act by the user, never a side effect of creating something.
        color: None,
        pin_position: None,
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
             -- Trashed assets keep their membership so restore is exact, but
             -- they must not be counted here or a folder would claim more than
             -- it shows.
             SELECT DISTINCT af.asset_id AS asset_id
             FROM assets_folders af
             JOIN subtree s ON af.folder_id = s.id
             JOIN assets a ON a.id = af.asset_id AND a.deleted_at IS NULL
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

    // Validate here rather than trusting the caller: the column is the source of
    // truth for what the sidebar renders, and an unknown token would resolve to
    // no colour at all — a pin that silently loses its accent.
    let color = match patch.color {
        Some(raw) => {
            let cleaned = blank_to_null(&raw);
            if let Some(c) = &cleaned {
                if !PIN_COLORS.contains(&c.as_str()) {
                    reject!("Unknown folder colour");
                }
            }
            Some(cleaned)
        }
        None => None,
    };

    if name.is_none() && notes.is_none() && color.is_none() {
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
        if let Some(v) = &color {
            sep.push("color = ");
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
        reject!("Folder not found");
    }

    // The folder's name feeds the `folder_text` of its DIRECT members — reindex
    // exactly those. (Notes aren't searchable, but a name change and a note edit
    // arrive through the same patch, so reindexing on either is simplest and
    // cheap.)
    if let Ok(members) = crate::search::asset_ids_in_folder(pool, id).await {
        if let Err(e) = crate::search::reindex_assets(pool, &members).await {
            warn!(error = %e, "Reindex after folder edit failed (non-fatal)");
        }
    }
    Ok(())
}

/// The hidden directory, inside the library root, where files are materialised
/// for an outbound OS drag. Inside the root on purpose: hard links only work
/// within a volume, and staging here guarantees the same volume as the assets.
pub const DRAG_STAGING_DIR: &str = ".drag-staging";

/// Materialise assets for dragging OUT to another application, returning the
/// absolute staged paths.
///
/// Two problems this solves, both established in the D5 analysis:
///   * on disk an asset is `UUID.ext`; dragging that hands the receiver a UUID.
///     Here it's linked under its real `filename`.
///   * a `CF_HDROP` drop can offer "move", which would delete the file out of the
///     library. A hard link shares the bytes, so if the receiver moves it only
///     the LINK dies — the library file is untouched. (Copy mode is also
///     requested on the JS side; this is the belt to that suspenders.)
///
/// Security: takes IDS, never paths. The webview names things; Rust resolves
/// locations — the same rule `source_url` follows. A caller cannot smuggle an
/// arbitrary filesystem path through this.
///
/// Each drag gets its own `<uuid>/` subdir so concurrent or repeated drags never
/// collide, and cleanup can delete one drag's files without racing another.
#[instrument(skip(pool, ids), fields(count = ids.len()))]
pub async fn stage_assets_for_drag(
    pool: &SqlitePool,
    root: &Path,
    ids: &[String],
) -> Result<Vec<String>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    let ids = unique_ids(ids);
    let mut rows: Vec<(String, String)> = Vec::new();
    for chunk in ids.chunks(IDS_PER_QUERY) {
        let mut qb = QueryBuilder::new("SELECT path, filename FROM assets WHERE id IN (");
        let mut sep = qb.separated(", ");
        for id in chunk {
            sep.push_bind(id);
        }
        qb.push(")");
        let mut part: Vec<(String, String)> = qb
            .build_query_as()
            .fetch_all(pool)
            .await
            .context("Failed to read assets to stage")?;
        rows.append(&mut part);
    }

    let dir = root
        .join(DRAG_STAGING_DIR)
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&dir).context("Failed to create drag staging dir")?;

    // Two assets can share a display name ("photo.png") — the library allows it,
    // but one directory can't. Disambiguate the SECOND onward as "photo (2).png".
    let mut seen: HashMap<String, u32> = HashMap::new();
    let mut staged = Vec::with_capacity(rows.len());

    for (rel_path, filename) in rows {
        let name = dedupe_name(&filename, &mut seen);
        let src = root.join(&rel_path);
        let dst = dir.join(&name);

        if std::fs::hard_link(&src, &dst).is_err() {
            // Cross-device (shouldn't happen under the root) or a filesystem
            // without hard links: fall back to a copy so the drag still works.
            std::fs::copy(&src, &dst)
                .with_context(|| format!("Failed to stage {}", src.display()))?;
        }
        staged.push(dst.to_string_lossy().into_owned());
    }

    Ok(staged)
}

/// Turn a filename into one unique within this drag, inserting " (n)" before the
/// extension on collision: `photo.png`, `photo (2).png`, `photo (3).png`.
fn dedupe_name(filename: &str, seen: &mut HashMap<String, u32>) -> String {
    let key = filename.to_lowercase();
    let count = seen.entry(key).or_insert(0);
    *count += 1;
    if *count == 1 {
        return filename.to_string();
    }
    match filename.rsplit_once('.') {
        Some((stem, ext)) => format!("{stem} ({count}).{ext}"),
        None => format!("{filename} ({count})"),
    }
}

/// Remove everything under the drag-staging directory. Called after a drag ends
/// (the staged links have served their purpose) and swept on library open so a
/// crash mid-drag never leaks links. Missing dir is success — nothing to clean.
#[instrument(skip_all)]
pub fn clear_drag_staging(root: &Path) -> Result<()> {
    let dir = root.join(DRAG_STAGING_DIR);
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).context("Failed to clear drag staging"),
    }
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

    // Capture the affected assets BEFORE the delete cascades away the membership
    // rows — these assets survive but lose these folders from their `folder_text`.
    // Includes descendants, since the cascade removes those folders too.
    let affected = crate::search::asset_ids_under_folders(pool, ids)
        .await
        .unwrap_or_default();

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

    if let Err(e) = crate::search::reindex_assets(pool, &affected).await {
        warn!(error = %e, "Reindex after folder delete failed (non-fatal)");
    }
    Ok(())
}

/// Smallest gap between two sibling positions we will still split.
///
/// Fractional ranks halve the interval on every insert into the same spot, and a
/// double runs out of mantissa after roughly fifty of them — at which point two
/// siblings silently take the same position and the order becomes whatever the
/// name tie-breaker says. Below this threshold the sibling set is renumbered
/// instead. Rare, bounded, and invisible; the alternative is a bug that only
/// appears after weeks of use and cannot be reproduced on demand.
const POSITION_EPSILON: f64 = 1e-6;

/// Reject moving a folder into itself or one of its own descendants, which would
/// detach the subtree from the root.
///
/// Walks UP from the prospective parent, so this costs the tree's DEPTH rather
/// than its size — the descendant set of the folder being moved could be most of
/// the library, while the ancestor chain is a handful of rows.
async fn assert_not_descendant(
    pool: &SqlitePool,
    id: &str,
    new_parent_id: Option<&str>,
) -> Result<()> {
    let mut cursor = new_parent_id.map(str::to_string);
    while let Some(cur) = cursor {
        if cur == id {
            reject!("Cannot move a folder into itself or a descendant");
        }
        cursor =
            sqlx::query_scalar::<_, Option<String>>("SELECT parent_id FROM folders WHERE id = ?")
                .bind(&cur)
                .fetch_optional(pool)
                .await
                .context("Failed to walk folder ancestry")?
                .flatten();
    }
    Ok(())
}

/// Place `id` under `new_parent`, immediately after sibling `after` — or first,
/// when `after` is `None`.
///
/// This is the "drop between two rows" half of tree drag & drop; `move_folder`
/// is the "drop onto a row" half and appends instead. Splitting the neighbours'
/// positions writes ONE row rather than renumbering the list, which is what
/// keeps a reorder O(1) in a tree of any size.
#[instrument(skip(pool))]
pub async fn reorder_folder(
    pool: &SqlitePool,
    id: &str,
    new_parent: Option<&str>,
    after: Option<&str>,
) -> Result<()> {
    assert_not_descendant(pool, id, new_parent).await?;

    let mut tx = pool
        .begin()
        .await
        .context("Failed to begin reorder transaction")?;

    // Siblings in display order, EXCLUDING the folder being moved: when
    // reordering inside one parent, its own current slot is not a neighbour of
    // where it's going. Ordering must match `fetch_folders` or the index the
    // frontend computed would point somewhere else.
    let siblings: Vec<(String, f64)> = sqlx::query_as(
        "SELECT id, position FROM folders
         WHERE parent_id IS ? AND id <> ?
         ORDER BY position, name",
    )
    .bind(new_parent)
    .bind(id)
    .fetch_all(&mut *tx)
    .await
    .context("Failed to read sibling folders")?;

    // The index the folder lands AT. An `after` that isn't among the siblings
    // (a stale tree on the frontend) appends rather than failing.
    let insert_at = match after {
        Some(a) => siblings
            .iter()
            .position(|(sid, _)| sid == a)
            .map(|i| i + 1)
            .unwrap_or(siblings.len()),
        None => 0,
    };

    let prev = insert_at
        .checked_sub(1)
        .and_then(|i| siblings.get(i))
        .map(|(_, p)| *p);
    let next = siblings.get(insert_at).map(|(_, p)| *p);

    let position = match (prev, next) {
        (None, None) => 0.0,                                  // first child
        (None, Some(n)) => n - 1.0,                           // new head
        (Some(p), None) => p + 1.0,                           // new tail
        (Some(p), Some(n)) if n - p > POSITION_EPSILON => (p + n) / 2.0,
        // Gap exhausted — renumber the siblings 0,1,2…, leaving a hole at
        // `insert_at` for the arriving folder.
        (Some(_), Some(_)) => {
            tracing::info!(parent = ?new_parent, "Sibling positions exhausted; renumbering");
            for (rank, (sid, _)) in siblings.iter().enumerate() {
                let renumbered = if rank < insert_at {
                    rank as f64
                } else {
                    rank as f64 + 1.0
                };
                sqlx::query("UPDATE folders SET position = ? WHERE id = ?")
                    .bind(renumbered)
                    .bind(sid)
                    .execute(&mut *tx)
                    .await
                    .context("Failed to renumber sibling folders")?;
            }
            insert_at as f64
        }
    };

    let res = sqlx::query("UPDATE folders SET parent_id = ?, position = ? WHERE id = ?")
        .bind(new_parent)
        .bind(position)
        .bind(id)
        .execute(&mut *tx)
        .await
        .context("Failed to reorder folder")?;
    if res.rows_affected() == 0 {
        reject!("Folder not found");
    }

    tx.commit()
        .await
        .context("Failed to commit reorder transaction")?;
    Ok(())
}

// ── Pins ──────────────────────────────────────────────────────────────────────
//
// The sidebar's shortlist holds BOTH folders and smart folders, in ONE order the
// user arranges freely. That means a single rank space spanning two tables:
// `folders.pin_position` and `rule_sets.pin_position` are compared against each
// other, and a reorder writes to whichever table owns the row.
//
// Two rank spaces would have been less code and the wrong model — the pins would
// interleave by accident of their independent numbering rather than by choice.

/// Which table a pin lives in.
///
/// Decoded straight from the `'folder'` / `'smart'` literals in `fetch_pins`'s
/// UNION, so the sqlx and serde names must agree — one spelling for the DB and
/// the frontend both.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Type)]
#[sqlx(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum PinKind {
    Folder,
    Smart,
}

impl PinKind {
    fn table(self) -> &'static str {
        match self {
            PinKind::Folder => "folders",
            PinKind::Smart => "rule_sets",
        }
    }
}

/// One entry in the pinned list, whichever kind it is.
#[derive(Serialize, Debug, Clone, FromRow)]
pub struct PinnedItem {
    pub kind: PinKind,
    pub id: String,
    pub name: String,
    pub color: Option<String>,
    pub position: f64,
}

/// The pinned list, in the user's order, across both kinds.
#[instrument(skip(pool))]
pub async fn fetch_pins(pool: &SqlitePool) -> Result<Vec<PinnedItem>> {
    sqlx::query_as::<_, PinnedItem>(
        "SELECT 'folder' AS kind, id, name, color, pin_position AS position FROM folders \
           WHERE pin_position IS NOT NULL \
         UNION ALL \
         SELECT 'smart' AS kind, id, name, color, pin_position AS position FROM rule_sets \
           WHERE kind = 'smart' AND pin_position IS NOT NULL \
         ORDER BY position, name",
    )
    .fetch_all(pool)
    .await
    .context("Failed to fetch pins")
}

/// One past the tail of the SHARED rank space.
async fn next_pin_position(pool: &SqlitePool) -> Result<f64> {
    let max: Option<f64> = sqlx::query_scalar(
        "SELECT MAX(p) FROM (SELECT MAX(pin_position) AS p FROM folders \
         UNION ALL SELECT MAX(pin_position) AS p FROM rule_sets)",
    )
    .fetch_one(pool)
    .await
    .context("Failed to compute pin position")?;
    Ok(max.map(|m| m + 1.0).unwrap_or(0.0))
}

/// Pin or unpin, whichever kind. Pinning appends; unpinning clears the rank but
/// keeps the colour, so re-pinning restores the look.
#[instrument(skip(pool))]
pub async fn set_pinned(pool: &SqlitePool, kind: PinKind, id: &str, pinned: bool) -> Result<()> {
    // Read first so "doesn't exist" and "already in that state" stay
    // distinguishable — with a bare UPDATE both look like 0 rows affected.
    let sql = format!("SELECT pin_position FROM {} WHERE id = ?", kind.table());
    let current: Option<Option<f64>> = sqlx::query_scalar(&sql)
        .bind(id)
        .fetch_optional(pool)
        .await
        .context("Failed to read pin state")?;

    let Some(position) = current else {
        reject!("Not found");
    };
    if position.is_some() == pinned {
        return Ok(()); // idempotent: re-pinning must not shuffle the order
    }

    let value = if pinned {
        Some(next_pin_position(pool).await?)
    } else {
        None
    };
    let sql = format!("UPDATE {} SET pin_position = ? WHERE id = ?", kind.table());
    sqlx::query(&sql)
        .bind(value)
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to update pin")?;
    Ok(())
}

/// Set or clear a pin's accent, whichever kind. Validated against [`PIN_COLORS`]
/// here as well as in `update_folder`, because this path bypasses that patch.
#[instrument(skip(pool))]
pub async fn set_pin_color(
    pool: &SqlitePool,
    kind: PinKind,
    id: &str,
    color: Option<&str>,
) -> Result<()> {
    if let Some(c) = color {
        if !PIN_COLORS.contains(&c) {
            reject!("Unknown pin colour");
        }
    }
    let sql = format!("UPDATE {} SET color = ? WHERE id = ?", kind.table());
    let res = sqlx::query(&sql)
        .bind(color)
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to set pin colour")?;
    if res.rows_affected() == 0 {
        reject!("Not found");
    }
    Ok(())
}

/// Drag-to-reorder across the whole pinned list. `after` is the pin it lands
/// behind; `None` means it becomes the first.
#[instrument(skip(pool))]
pub async fn reorder_pin(
    pool: &SqlitePool,
    kind: PinKind,
    id: &str,
    after_kind: Option<PinKind>,
    after_id: Option<&str>,
) -> Result<()> {
    let mut tx = pool
        .begin()
        .await
        .context("Failed to begin pin reorder transaction")?;

    // Every other pin, in display order, across both tables.
    let pins: Vec<PinnedItem> = sqlx::query_as(
        "SELECT * FROM (\
           SELECT 'folder' AS kind, id, name, color, pin_position AS position FROM folders \
             WHERE pin_position IS NOT NULL \
           UNION ALL \
           SELECT 'smart' AS kind, id, name, color, pin_position AS position FROM rule_sets \
             WHERE kind = 'smart' AND pin_position IS NOT NULL) \
         ORDER BY position, name",
    )
    .fetch_all(&mut *tx)
    .await
    .context("Failed to read pins")?;

    let others: Vec<&PinnedItem> = pins
        .iter()
        .filter(|p| !(p.kind == kind && p.id == id))
        .collect();

    // An `after` that isn't in the list (a stale sidebar) appends rather than
    // failing — the same forgiving rule the folder tree uses.
    let insert_at = match (after_kind, after_id) {
        (Some(ak), Some(aid)) => others
            .iter()
            .position(|p| p.kind == ak && p.id == aid)
            .map(|i| i + 1)
            .unwrap_or(others.len()),
        _ => 0,
    };

    let prev = insert_at.checked_sub(1).and_then(|i| others.get(i)).map(|p| p.position);
    let next = others.get(insert_at).map(|p| p.position);

    let position = match (prev, next) {
        (None, None) => 0.0,
        (None, Some(n)) => n - 1.0,
        (Some(p), None) => p + 1.0,
        (Some(p), Some(n)) if n - p > POSITION_EPSILON => (p + n) / 2.0,
        // Gap exhausted — renumber, leaving a hole for the arriving pin. Writes
        // land in whichever table owns each row.
        (Some(_), Some(_)) => {
            tracing::info!("Pin positions exhausted; renumbering");
            for (rank, p) in others.iter().enumerate() {
                let renumbered = if rank < insert_at { rank as f64 } else { rank as f64 + 1.0 };
                let sql = format!("UPDATE {} SET pin_position = ? WHERE id = ?", p.kind.table());
                sqlx::query(&sql)
                    .bind(renumbered)
                    .bind(&p.id)
                    .execute(&mut *tx)
                    .await
                    .context("Failed to renumber pins")?;
            }
            insert_at as f64
        }
    };

    let sql = format!(
        "UPDATE {} SET pin_position = ? WHERE id = ? AND pin_position IS NOT NULL",
        kind.table()
    );
    let res = sqlx::query(&sql)
        .bind(position)
        .bind(id)
        .execute(&mut *tx)
        .await
        .context("Failed to reorder pin")?;
    if res.rows_affected() == 0 {
        reject!("That item isn't pinned");
    }

    tx.commit()
        .await
        .context("Failed to commit pin reorder transaction")?;
    Ok(())
}

/// A few assets a rule set currently matches, for the sidebar preview.
///
/// Deliberately the light row and a small LIMIT: this answers "what's in here
/// right now", which a list of rules can't, and it must stay cheap enough to run
/// on hover.
#[instrument(skip(pool, rules))]
pub async fn preview_matches(
    pool: &SqlitePool,
    rules: &crate::rules::RuleNode,
    limit: i64,
) -> Result<Vec<AssetLightRow>> {
    let mut qb = QueryBuilder::<Sqlite>::new(
        "SELECT a.id, a.width, a.height, a.asset_type, a.thumb_hash, a.is_animated, a.filename \
         FROM assets a WHERE a.deleted_at IS NULL",
    );
    if rules.is_active() {
        qb.push(" AND ");
        rules.push_predicate(&mut qb);
    }
    qb.push(" ORDER BY a.imported_date DESC, a.id DESC LIMIT ")
        .push_bind(limit.clamp(1, 50));

    qb.build_query_as::<AssetLightRow>()
        .fetch_all(pool)
        .await
        .context("Failed to preview matches")
}

/// Reparent a folder (and append it to the end of the new parent's siblings).
/// Rejects moving a folder into itself or one of its own descendants, which would
/// orphan the subtree — checked by walking up from the target parent to the root.
#[instrument(skip(pool))]
pub async fn move_folder(pool: &SqlitePool, id: &str, new_parent_id: Option<&str>) -> Result<()> {
    assert_not_descendant(pool, id, new_parent_id).await?;

    let position = next_folder_position(pool, new_parent_id).await?;
    let res = sqlx::query("UPDATE folders SET parent_id = ?, position = ? WHERE id = ?")
        .bind(new_parent_id)
        .bind(position)
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to move folder")?;
    if res.rows_affected() == 0 {
        reject!("Folder not found");
    }
    Ok(())
}

// ── Composable mutations ─────────────────────────────────────────────────────
//
// Each mutation below exists twice, on purpose:
//
//   `foo(pool, …)`    — the complete unit of work. Opens its own transaction,
//                       commits, then runs the post-commit side effects (the
//                       reindex, which is deliberately non-fatal). A Tauri
//                       command calls this one.
//   `foo_in(conn, …)` — the SQL alone, inside a transaction the CALLER owns,
//                       with no side effects whatsoever.
//
// A transaction boundary is a claim about the USE CASE, not about the data
// operation. One command is one atomic act, so owning the boundary is right for
// the outer form; a quick action sequences several of these and needs them to
// commit or fail as one, so it needs the inner form.
//
// The contract for an `_in` function is SQL and nothing else — no `begin()` (it
// has no pool to begin from, so the signature makes that unrepresentable), no
// reindex, and no logging of a commit that hasn't happened yet.
//
// The OUTER halves are gone. `add_assets_to_folder`, `move_assets_to_folder` and
// `remove_assets_from_folder` each owned a transaction and a reindex for exactly
// one caller — a Tauri command — and those commands were deleted when membership
// was rerouted through `run_steps`, which sequences the `_in` halves inside a
// transaction it owns and reindexes once for the whole pipeline. Keeping them
// would have preserved a second path to the same writes with no undo record.
//
// `move_assets_to_folder` in particular was noted here as evidence the split was
// overdue: it existed only because add + remove could not share a transaction
// from a caller. `Op::SetFolders` is that capability, generalised — so the
// hand-fused special case retired with the problem it worked around.

// ── Permanent deletion ───────────────────────────────────────────────────────
//
// Deliberately NOT a pipeline step. Every `Op` can state its inverse; this one
// cannot, and a step that silently isn't undoable inside a run that claims to be
// would be worse than no delete at all. It lives here, reachable only from the
// Trash, and always behind a confirmation.

/// How many assets are in the Trash. Drives the sidebar badge.
#[instrument(skip(pool))]
pub async fn trash_count(pool: &SqlitePool) -> Result<i64> {
    sqlx::query_scalar("SELECT COUNT(*) FROM assets WHERE deleted_at IS NOT NULL")
        .fetch_one(pool)
        .await
        .context("Failed to count the Trash")
}

/// Delete assets and their bytes for good.
///
/// Order matters: the DATABASE row goes first, then the files. A crash between
/// the two leaves an orphaned file (wasted disk, invisible, reapable) rather
/// than a row pointing at nothing (a broken thumbnail in the grid forever).
/// Same reasoning as the import pipeline's write-then-commit ordering, mirrored.
#[instrument(skip(pool, root, ids), fields(count = ids.len()))]
pub async fn purge_assets(pool: &SqlitePool, root: &Path, ids: &[String]) -> Result<usize> {
    if ids.is_empty() {
        return Ok(0);
    }

    // Paths captured before the delete cascades them away.
    let mut paths: Vec<(String, String)> = Vec::with_capacity(ids.len());
    for chunk in ids.chunks(IDS_PER_QUERY) {
        let mut qb = QueryBuilder::new("SELECT id, extension FROM assets WHERE id IN (");
        let mut sep = qb.separated(", ");
        for id in chunk {
            sep.push_bind(id);
        }
        // Only ever from the Trash: purging a live asset would be a delete the
        // user never saw coming, so the guard lives in the query rather than in
        // whichever caller remembers it.
        qb.push(") AND deleted_at IS NOT NULL");
        paths.extend(
            qb.build_query_as::<(String, String)>()
                .fetch_all(pool)
                .await
                .context("Failed to read the assets to delete")?,
        );
    }
    if paths.is_empty() {
        return Ok(0);
    }
    let purge_ids: Vec<&String> = paths.iter().map(|(id, _)| id).collect();

    let mut tx = pool
        .begin()
        .await
        .context("Failed to begin the delete transaction")?;
    for chunk in purge_ids.chunks(IDS_PER_QUERY) {
        // `search_index` is an FTS5 virtual table — no foreign keys, so it is
        // the one place the cascade can't reach and must be cleared by hand.
        let mut del = QueryBuilder::<Sqlite>::new("DELETE FROM search_index WHERE asset_id IN (");
        let mut sep = del.separated(", ");
        for id in chunk {
            sep.push_bind(*id);
        }
        del.push(")");
        del.build()
            .execute(&mut *tx)
            .await
            .context("Failed to clear search rows")?;

        // Memberships, tags, colours and smart-folder ranks all cascade.
        let mut qb = QueryBuilder::<Sqlite>::new("DELETE FROM assets WHERE id IN (");
        let mut sep = qb.separated(", ");
        for id in chunk {
            sep.push_bind(*id);
        }
        qb.push(")");
        qb.build()
            .execute(&mut *tx)
            .await
            .context("Failed to delete assets")?;
    }
    tx.commit()
        .await
        .context("Failed to commit the delete transaction")?;

    // Best-effort, and after the commit. A file we can't remove is disk we
    // haven't reclaimed; it is not a reason to keep a row the user deleted.
    let assets_dir = root.join("assets");
    let thumbs_dir = root.join("thumbnails");
    for (id, ext) in &paths {
        let original = assets_dir.join(format!("{id}.{ext}"));
        if let Err(e) = tokio::fs::remove_file(&original).await {
            if e.kind() != std::io::ErrorKind::NotFound {
                warn!(path = ?original, error = %e, "Could not remove the original (non-fatal)");
            }
        }
        let thumb = thumbs_dir.join(format!("{id}.webp"));
        if let Err(e) = tokio::fs::remove_file(&thumb).await {
            if e.kind() != std::io::ErrorKind::NotFound {
                warn!(path = ?thumb, error = %e, "Could not remove the thumbnail (non-fatal)");
            }
        }
    }

    Ok(paths.len())
}

/// Purge everything in the Trash.
#[instrument(skip(pool, root))]
pub async fn empty_trash(pool: &SqlitePool, root: &Path) -> Result<usize> {
    let ids: Vec<String> =
        sqlx::query_scalar("SELECT id FROM assets WHERE deleted_at IS NOT NULL")
            .fetch_all(pool)
            .await
            .context("Failed to read the Trash")?;
    purge_assets(pool, root, &ids).await
}

// ── Folder auto-tags ─────────────────────────────────────────────────────────

/// The tags this folder seeds onto arriving assets.
#[instrument(skip(pool))]
pub async fn fetch_folder_auto_tags(pool: &SqlitePool, folder_id: &str) -> Result<Vec<String>> {
    sqlx::query_scalar("SELECT tag_id FROM folder_auto_tags WHERE folder_id = ?")
        .bind(folder_id)
        .fetch_all(pool)
        .await
        .context("Failed to read the folder's auto-tags")
}

/// Replace the whole set. A set, not a list — order carries no meaning here, so
/// delete-then-insert is both correct and the smallest thing that works.
#[instrument(skip(pool, tag_ids), fields(count = tag_ids.len()))]
pub async fn set_folder_auto_tags(
    pool: &SqlitePool,
    folder_id: &str,
    tag_ids: &[String],
) -> Result<()> {
    let mut tx = pool
        .begin()
        .await
        .context("Failed to begin the auto-tag transaction")?;

    sqlx::query("DELETE FROM folder_auto_tags WHERE folder_id = ?")
        .bind(folder_id)
        .execute(&mut *tx)
        .await
        .context("Failed to clear the folder's auto-tags")?;

    if !tag_ids.is_empty() {
        let mut qb = QueryBuilder::new("INSERT OR IGNORE INTO folder_auto_tags (folder_id, tag_id) ");
        qb.push_values(tag_ids, |mut b, tag_id| {
            b.push_bind(folder_id).push_bind(tag_id);
        });
        qb.build()
            .execute(&mut *tx)
            .await
            .context("Failed to set the folder's auto-tags")?;
    }

    tx.commit()
        .await
        .context("Failed to commit the auto-tag transaction")?;
    Ok(())
}

/// The auto-tags of one folder, inside a caller-owned transaction.
///
/// Deliberately does NOT walk ancestors: a folder seeds only its own tags. See
/// the migration for why inheritance is left out of v1.
pub(crate) async fn auto_tags_of(
    conn: &mut sqlx::SqliteConnection,
    folder_id: &str,
) -> Result<Vec<String>> {
    sqlx::query_scalar("SELECT tag_id FROM folder_auto_tags WHERE folder_id = ?")
        .bind(folder_id)
        .fetch_all(&mut *conn)
        .await
        .context("Failed to read the folder's auto-tags")
}

/// Seed auto-tags for every folder an import just filed assets into.
///
/// Grouped by folder so a 2,000-file import into three folders costs three
/// lookups rather than two thousand. Not recorded as an undoable run: import has
/// no undo at all today, and a half-undoable import would be worse than a
/// consistently un-undoable one.
async fn seed_import_auto_tags(pool: &SqlitePool, links: &[FolderLink]) -> Result<()> {
    if links.is_empty() {
        return Ok(());
    }
    let mut by_folder: HashMap<&str, Vec<String>> = HashMap::new();
    for link in links {
        by_folder
            .entry(link.folder_id.as_str())
            .or_default()
            .push(link.asset_id.clone());
    }

    let mut tx = pool
        .begin()
        .await
        .context("Failed to begin the auto-tag transaction")?;
    for (folder_id, asset_ids) in by_folder {
        for tag_id in auto_tags_of(&mut tx, folder_id).await? {
            crate::tags::assign_tag_in(&mut tx, &tag_id, &asset_ids).await?;
        }
    }
    tx.commit()
        .await
        .context("Failed to commit the auto-tag transaction")?;
    Ok(())
}

/// Every asset currently in a folder. Backs the explicit "apply to what's
/// already here" one-off, which is the only retroactive path.
#[instrument(skip(pool))]
pub async fn folder_member_ids(pool: &SqlitePool, folder_id: &str) -> Result<Vec<String>> {
    sqlx::query_scalar(
        "SELECT af.asset_id FROM assets_folders af \
         JOIN assets a ON a.id = af.asset_id AND a.deleted_at IS NULL \
         WHERE af.folder_id = ?",
    )
        .bind(folder_id)
        .fetch_all(pool)
        .await
        .context("Failed to read the folder's members")
}

/// Append assets to a folder inside a caller-owned transaction.
///
/// `INSERT OR IGNORE` keeps an already-present asset at the position it has, so
/// re-adding never reshuffles a folder the user has arranged by hand.
pub(crate) async fn add_assets_to_folder_in(
    conn: &mut sqlx::SqliteConnection,
    folder_id: &str,
    asset_ids: &[String],
) -> Result<()> {
    let base = sqlx::query_scalar::<_, Option<f64>>(
        "SELECT MAX(position) FROM assets_folders WHERE folder_id = ?",
    )
    .bind(folder_id)
    .fetch_one(&mut *conn)
    .await
    .context("Failed to compute membership position")?
    .map(|m| m + 1.0)
    .unwrap_or(0.0);

    // Multi-row INSERT, not one statement per asset. This is the path a Ctrl+A
    // followed by a drag into a folder takes, so its input is the SELECTION —
    // which can be the whole library — and a statement each meant one round trip
    // per asset. `remove_assets_from_folder_in` below has always chunked; this
    // half simply never caught up.
    //
    // Three binds per row, so the chunk is sized against SQLite's 32766
    // parameter cap the same way `persist_import` sizes its own.
    const ROWS_PER_INSERT: usize = 8000;

    for (chunk_idx, chunk) in asset_ids.chunks(ROWS_PER_INSERT).enumerate() {
        // Position continues across chunks, so the folder's order is the order
        // the ids arrived in rather than restarting at `base` every 8,000.
        let chunk_base = base + (chunk_idx * ROWS_PER_INSERT) as f64;
        let mut offset = 0.0f64;

        let mut qb = QueryBuilder::new(
            "INSERT OR IGNORE INTO assets_folders (folder_id, asset_id, position) ",
        );
        qb.push_values(chunk, |mut b, id| {
            b.push_bind(folder_id)
                .push_bind(id)
                .push_bind(chunk_base + offset);
            offset += 1.0;
        });
        qb.build()
            .execute(&mut *conn)
            .await
            .context("Failed to add assets to folder")?;
    }
    Ok(())
}

/// Drop folder membership inside a caller-owned transaction.
///
/// Chunked, unlike the single statement this replaced: a quick action can run
/// over a Ctrl+A selection, and SQLite caps bound parameters at 32766.
pub(crate) async fn remove_assets_from_folder_in(
    conn: &mut sqlx::SqliteConnection,
    folder_id: &str,
    asset_ids: &[String],
) -> Result<()> {
    for chunk in asset_ids.chunks(IDS_PER_QUERY) {
        let mut qb = QueryBuilder::new("DELETE FROM assets_folders WHERE folder_id = ");
        qb.push_bind(folder_id);
        qb.push(" AND asset_id IN (");
        let mut separated = qb.separated(", ");
        for id in chunk {
            separated.push_bind(id);
        }
        qb.push(")");
        qb.build()
            .execute(&mut *conn)
            .await
            .context("Failed to remove assets from folder")?;
    }
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
    let visual = extract::extract_visual(asset_type, &src)
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
        // Saturating, not `as`. SQLite integers are signed, and a bogus length
        // from a corrupt or exotic filesystem would WRAP under `as i64` — a
        // negative size that then poisons `SUM(file_size)` in folder stats and
        // silently inverts every size filter. Clamping is wrong by a rounding
        // error at a scale no file reaches; wrapping is wrong by 2^64.
        file_size: i64::try_from(meta.len()).unwrap_or(i64::MAX),
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

/// Bring back any deduplicated asset that was sitting in the Trash.
///
/// The dedup lookup deliberately matches trashed assets — `content_hash` is
/// UNIQUE, so it has to, or re-importing deleted bytes would trip the index.
/// Which leaves one question: what should re-importing a file you threw away
/// do? Nothing visible is the wrong answer. Dropping a file into Nova says "I
/// want this", so it comes back out of the Trash, with every folder and tag it
/// had — restore is exact, which is the whole reason Trash is a soft delete.
#[instrument(skip(pool, duplicates), fields(count = duplicates.len()))]
async fn restore_trashed_duplicates(
    pool: &SqlitePool,
    duplicates: &[(String, AssetMetadata)],
) -> Result<usize> {
    if duplicates.is_empty() {
        return Ok(0);
    }
    let ids: Vec<&String> = duplicates.iter().map(|(id, _)| id).collect();
    let mut restored = 0usize;
    for chunk in ids.chunks(IDS_PER_QUERY) {
        let mut qb =
            QueryBuilder::<Sqlite>::new("UPDATE assets SET deleted_at = NULL WHERE id IN (");
        let mut sep = qb.separated(", ");
        for id in chunk {
            sep.push_bind(*id);
        }
        qb.push(") AND deleted_at IS NOT NULL");
        restored += qb
            .build()
            .execute(pool)
            .await
            .context("Failed to restore re-imported assets")?
            .rows_affected() as usize;
    }
    Ok(restored)
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
    //
    // Sized up front and Fx-hashed: this is one entry per staged file plus one
    // per pre-existing match, so a 100k-file import fills it 100k+ times with
    // 64-char hex keys. Both the rehash-and-move on growth and SipHash's
    // per-key cost are pure overhead on a map that never leaves this function.
    let mut owner: FxHashMap<String, String> =
        FxHashMap::with_capacity_and_hasher(staged.len(), Default::default());

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

    // Re-importing something you deleted brings it back, rather than appearing
    // to do nothing at all.
    let restored = restore_trashed_duplicates(&pool, &duplicates).await?;
    if restored > 0 {
        info!(count = restored, "Restored re-imported assets from the Trash");
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

    // Folders seed their auto-tags onto what just landed in them. Import writes
    // membership rows directly rather than through `add_assets_to_folder_in`, so
    // this is the second (and only other) place membership is created.
    //
    // Non-fatal, and deliberately AFTER the point of no return: the assets are
    // on disk and in the database, and failing to tag them is not a reason to
    // undo an import the user can already see.
    if let Err(e) = seed_import_auto_tags(&pool, &links).await {
        warn!(error = %e, "Auto-tagging after import failed (non-fatal)");
    }

    // Index the new assets AND the deduped ones — a duplicate wasn't copied but
    // gained folder membership, so its `folder_text` changed. Non-fatal: a
    // failed reindex leaves those rows findable only after the next rebuild, but
    // the import itself stands.
    let reindex_ids: Vec<String> = staged_assets
        .iter()
        .map(|a| a.id.clone())
        .chain(duplicates.iter().map(|(existing_id, _)| existing_id.clone()))
        .collect();
    if let Err(e) = crate::search::reindex_assets(&pool, &reindex_ids).await {
        warn!(error = %e, "Reindex after import failed (non-fatal)");
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
        restored,
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
         WHERE thumb_hash IS NULL AND asset_type = 'image' AND deleted_at IS NULL \
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
         WHERE thumb_hash IS NULL AND asset_type = 'image' AND deleted_at IS NULL AND id IN (",
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
        sqlx::query_scalar("SELECT COUNT(*) FROM assets WHERE asset_type = 'image' AND deleted_at IS NULL")
            .fetch_one(pool)
            .await
            .context("Failed to count images")?;
    // `deleted_at IS NULL` here too, not just on `total`. Trashed assets keep
    // their palette rows (restore has to be exact), so without this an analyzed
    // asset in the Trash counts toward the numerator but not the denominator —
    // and the UI reports "analyzed 1,050 of 1,000".
    let analyzed: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT asset_id) FROM asset_colors c \
         JOIN assets a ON a.id = c.asset_id \
         WHERE a.asset_type = 'image' AND a.deleted_at IS NULL",
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

    // One multi-row INSERT rather than one per swatch. A palette is ~8 entries,
    // so per asset this is small — but it runs for EVERY image, and the caller
    // does a chunk of 64 at a time: the old form cost ~576 statements per chunk,
    // and a full-library colour pass over 10,000 images cost ~90,000.
    if palette.is_empty() {
        return Ok(());
    }
    let mut qb = QueryBuilder::<Sqlite>::new("INSERT INTO asset_colors (asset_id, l, a, b, ratio) ");
    qb.push_values(palette, |mut b, entry| {
        b.push_bind(asset_id)
            .push_bind(entry.lab.l as f64)
            .push_bind(entry.lab.a as f64)
            .push_bind(entry.lab.b as f64)
            .push_bind(entry.ratio as f64);
    });
    qb.build()
        .execute(&mut **tx)
        .await
        .context("Failed to insert palette entries")?;
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
         WHERE asset_type = 'image' AND deleted_at IS NULL AND id NOT IN (SELECT asset_id FROM asset_colors) \
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

/// End-to-end backend tests for the manifest query.
///
/// The rule tests cover the compiler and the wire tests cover parsing, but
/// neither covers the two together THROUGH `ManifestQuery` — which is the actual
/// path a filter takes. That gap is where a filter can parse fine, compile fine,
/// and still not narrow anything.
#[cfg(test)]
mod manifest_tests {
    use super::*;

    async fn db() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        for stmt in [
            "CREATE TABLE assets (id TEXT PRIMARY KEY, asset_type TEXT, filename TEXT, \
             notes TEXT, source_url TEXT, file_size INTEGER, width INTEGER, height INTEGER, \
             extension TEXT, imported_date TEXT, creation_date TEXT, modified_date TEXT, \
             manual_position REAL, thumb_hash TEXT, is_animated INTEGER, deleted_at TEXT)",
            "CREATE TABLE assets_tags (asset_id TEXT, tag_id TEXT)",
            "CREATE TABLE assets_folders (folder_id TEXT, asset_id TEXT, position REAL)",
            "INSERT INTO assets (id, asset_type, filename, notes, source_url, file_size, width, height, extension, imported_date, creation_date, modified_date, manual_position, thumb_hash, is_animated) VALUES ('i1','image','a.png',NULL,NULL,10,100,100,'png',\
             '2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z',0,NULL,0)",
            "INSERT INTO assets (id, asset_type, filename, notes, source_url, file_size, width, height, extension, imported_date, creation_date, modified_date, manual_position, thumb_hash, is_animated) VALUES ('v1','video','b.mp4',NULL,NULL,20,100,100,'mp4',\
             '2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z',1,NULL,0)",
            "INSERT INTO assets_tags VALUES ('i1','t1')",
        ] {
            sqlx::query(stmt).execute(&pool).await.unwrap();
        }
        pool
    }

    async fn ids_for(pool: &SqlitePool, query_json: &str) -> Vec<String> {
        let query: ManifestQuery = serde_json::from_str(query_json).expect("query must parse");
        let mut qb = build_manifest_query(
            &query.scope,
            None,
            &query.filters,
            query.sort.unwrap_or(DEFAULT_SORT),
        );
        qb.build_query_as::<AssetLightRow>()
            .fetch_all(pool)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect()
    }

    /// The exact payload the filter bar sends when you click "Videos".
    #[tokio::test]
    async fn media_type_filter_narrows_the_manifest() {
        let pool = db().await;
        let ids = ids_for(
            &pool,
            r#"{"scope":{"kind":"all"},"filters":{"rules":{"kind":"group","op":"all","children":[
                {"kind":"condition","type":"media_type","types":["video"]}]},"text":null},
                "sort":{"order_by":"imported_date","is_ascending":false}}"#,
        )
        .await;
        assert_eq!(ids, vec!["v1"], "clicking Videos must not leave images in");
    }

    /// The batching itself, which is what the grid's first paint depends on.
    ///
    /// `sort` is passed explicitly so these don't need a `view_settings` table —
    /// `resolve_sort` is covered by its own tests, and this is about the cursor.
    async fn stream_batches(
        pool: &SqlitePool,
        scope: Scope,
        chunk: usize,
        stop_after: Option<usize>,
    ) -> (Vec<Vec<String>>, usize) {
        let query = ManifestQuery {
            scope,
            filters: FilterSet::default(),
            sort: Some(DEFAULT_SORT),
        };
        let mut batches: Vec<Vec<String>> = Vec::new();
        let total = stream_manifest(pool, &query, chunk, |b| {
            batches.push(b.into_iter().map(|r| r.id).collect());
            Ok(stop_after.is_none_or(|n| batches.len() < n))
        })
        .await
        .unwrap();
        (batches, total)
    }

    /// Rows arrive split across batches, in order, with none lost at the seam.
    #[tokio::test]
    async fn streaming_splits_rows_into_batches() {
        let pool = db().await;
        let (batches, total) = stream_batches(&pool, Scope::All, 1, None).await;
        assert_eq!(batches, vec![vec!["v1"], vec!["i1"]]);
        assert_eq!(total, 2);
    }

    /// A chunk larger than the result set is one batch, not one per row.
    #[tokio::test]
    async fn streaming_sends_one_batch_when_it_fits() {
        let pool = db().await;
        let (batches, total) = stream_batches(&pool, Scope::All, 100, None).await;
        assert_eq!(batches, vec![vec!["v1", "i1"]]);
        assert_eq!(total, 2);
    }

    /// A superseded request abandons the cursor rather than paying for rows
    /// nobody will render — the backend half of the frontend's load token.
    #[tokio::test]
    async fn streaming_stops_when_the_sink_declines() {
        let pool = db().await;
        let (batches, total) = stream_batches(&pool, Scope::All, 1, Some(1)).await;
        assert_eq!(batches, vec![vec!["v1"]], "must not keep draining");
        assert_eq!(total, 1, "the count reports what was actually sent");
    }

    /// Zero matches sends ZERO batches. The frontend treats "resolved having
    /// seen nothing" as a legitimate empty result, so an empty batch would be a
    /// redundant second way to say it.
    #[tokio::test]
    async fn streaming_an_empty_result_sends_nothing() {
        let pool = db().await;
        let (batches, total) = stream_batches(&pool, Scope::Trash, 10, None).await;
        assert!(batches.is_empty(), "got {batches:?}");
        assert_eq!(total, 0);
    }

    /// And when you select a tag to include.
    #[tokio::test]
    async fn tag_include_filter_narrows_the_manifest() {
        let pool = db().await;
        let ids = ids_for(
            &pool,
            r#"{"scope":{"kind":"all"},"filters":{"rules":{"kind":"group","op":"all","children":[
                {"kind":"condition","type":"tags","mode":"all","include":["t1"],"exclude":[],"untagged":false}]},
                "text":null},"sort":{"order_by":"imported_date","is_ascending":false}}"#,
        )
        .await;
        assert_eq!(ids, vec!["i1"]);
    }
}

/// Manual order inside a smart folder.
///
/// The riskiest surface in the feature: a wrong rank doesn't crash, it just puts
/// the user's assets in the wrong order, which nothing else would catch.
#[cfg(test)]
mod smart_order_tests {
    use super::*;

    const SMART: &str = "s1";

    async fn db() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        for stmt in [
            "CREATE TABLE assets (id TEXT PRIMARY KEY, asset_type TEXT, filename TEXT, \
             notes TEXT, source_url TEXT, file_size INTEGER, width INTEGER, height INTEGER, \
             extension TEXT, imported_date TEXT, creation_date TEXT, modified_date TEXT, \
             manual_position REAL, thumb_hash TEXT, is_animated INTEGER, deleted_at TEXT)",
            "CREATE TABLE assets_tags (asset_id TEXT, tag_id TEXT)",
            "CREATE TABLE assets_folders (folder_id TEXT, asset_id TEXT, position REAL)",
            "CREATE TABLE smart_folder_order (smart_folder_id TEXT, asset_id TEXT, \
             position REAL NOT NULL, PRIMARY KEY (smart_folder_id, asset_id))",
            // `reorder_assets` resolves the folder's predicate from here rather
            // than trusting the caller, so the row has to exist.
            "CREATE TABLE rule_sets (id TEXT PRIMARY KEY, kind TEXT, name TEXT, \
             version INTEGER, query_json TEXT)",
        ] {
            sqlx::query(stmt).execute(&pool).await.unwrap();
        }
        sqlx::query("INSERT INTO rule_sets VALUES (?, 'smart', 'Videos', ?, ?)")
            .bind(SMART)
            .bind(RULE_SET_VERSION)
            .bind(serde_json::to_string(&rules()).unwrap())
            .execute(&pool)
            .await
            .unwrap();
        // Three videos, newest first by imported_date: v3, v2, v1. Plus an image
        // that the smart folder's rules exclude.
        for (id, ty, date) in [
            ("v1", "video", "2026-01-01T00:00:00.000Z"),
            ("v2", "video", "2026-01-02T00:00:00.000Z"),
            ("v3", "video", "2026-01-03T00:00:00.000Z"),
            ("i1", "image", "2026-01-04T00:00:00.000Z"),
        ] {
            sqlx::query(
                "INSERT INTO assets (id, asset_type, filename, notes, source_url, file_size, width, height, extension, imported_date, creation_date, modified_date, manual_position, thumb_hash, is_animated) VALUES (?, ?, 'f', NULL, NULL, 1, 10, 10, 'x', ?, ?, ?, 0, NULL, 0)",
            )
            .bind(id).bind(ty).bind(date).bind(date).bind(date)
            .execute(&pool).await.unwrap();
        }
        pool
    }

    /// "Every video" — the scope predicate under test.
    fn rules() -> crate::rules::RuleNode {
        serde_json::from_str(
            r#"{"kind":"condition","type":"media_type","types":["video"]}"#,
        )
        .unwrap()
    }

    async fn order(pool: &SqlitePool) -> Vec<String> {
        let scope = Scope::Smart { id: SMART.into() };
        let rules = rules();
        let sort = Sort { order_by: OrderBy::Manual, is_ascending: true };
        let filters = FilterSet::default();
        build_manifest_query(&scope, Some(&rules), &filters, sort)
            .build_query_as::<AssetLightRow>()
            .fetch_all(pool)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect()
    }

    async fn reorder(pool: &SqlitePool, moved: &str, after: Option<&str>) {
        reorder_assets(pool, &Scope::Smart { id: SMART.into() }, &[moved.into()], after)
            .await
            .unwrap();
    }

    /// With nothing placed, the folder still reads newest-first — the unranked
    /// tail is a real order, not a pile.
    #[tokio::test]
    async fn unranked_members_sort_newest_first() {
        let pool = db().await;
        assert_eq!(order(&pool).await, vec!["v3", "v2", "v1"]);
    }

    #[tokio::test]
    async fn dragging_reorders_and_persists() {
        let pool = db().await;
        // Put the oldest at the very front.
        reorder(&pool, "v1", None).await;
        assert_eq!(order(&pool).await, vec!["v1", "v3", "v2"]);
    }

    /// The whole reason this table exists: two smart folders (and All assets)
    /// must order independently.
    #[tokio::test]
    async fn reordering_does_not_touch_the_global_rank() {
        let pool = db().await;
        reorder(&pool, "v1", None).await;
        let globals: Vec<f64> = sqlx::query_scalar("SELECT manual_position FROM assets")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert!(
            globals.iter().all(|p| *p == 0.0),
            "smart folder reorder must not write assets.manual_position, got {globals:?}"
        );
    }

    /// A new match appends at the bottom rather than landing wherever its
    /// import date would put it among ranked rows.
    #[tokio::test]
    async fn new_members_append_after_the_ranked_block() {
        let pool = db().await;
        reorder(&pool, "v1", None).await; // ranks v1, v3, v2

        sqlx::query(
            "INSERT INTO assets (id, asset_type, filename, notes, source_url, file_size, width, height, extension, imported_date, creation_date, modified_date, manual_position, thumb_hash, is_animated) VALUES ('v4','video','f',NULL,NULL,1,10,10,'x',\
             '2026-01-09T00:00:00.000Z','2026-01-09T00:00:00.000Z','2026-01-09T00:00:00.000Z',0,NULL,0)",
        )
        .execute(&pool).await.unwrap();

        // Newest of all, but unplaced — so it goes last, not first.
        assert_eq!(order(&pool).await, vec!["v1", "v3", "v2", "v4"]);
    }

    /// Leaving and returning must lose the old slot: removing what made an asset
    /// match is deliberate, so it comes back as a newcomer.
    #[tokio::test]
    async fn a_departed_asset_loses_its_rank() {
        let pool = db().await;
        reorder(&pool, "v1", None).await;
        assert_eq!(order(&pool).await, vec!["v1", "v3", "v2"]);

        // v1 stops matching, and the folder is opened (prune runs).
        sqlx::query("UPDATE assets SET asset_type = 'image' WHERE id = 'v1'")
            .execute(&pool).await.unwrap();
        prune_smart_order(&pool, SMART, &rules()).await.unwrap();

        // ...then matches again. It has no rank now, so it sorts to the tail.
        sqlx::query("UPDATE assets SET asset_type = 'video' WHERE id = 'v1'")
            .execute(&pool).await.unwrap();
        assert_eq!(order(&pool).await, vec!["v3", "v2", "v1"]);
    }

    /// Pruning must only drop the departed — a bug here silently erases an
    /// order the user built by hand.
    #[tokio::test]
    async fn pruning_keeps_ranks_of_current_members() {
        let pool = db().await;
        reorder(&pool, "v1", None).await;
        prune_smart_order(&pool, SMART, &rules()).await.unwrap();
        assert_eq!(order(&pool).await, vec!["v1", "v3", "v2"]);
    }
}

/// Manual order in a FOLDER and in the global scope.
///
/// `smart_order_tests` covers `Scope::Smart`, which is the third of the three
/// targets `write_manual_positions` dispatches to — the folder membership row
/// and the asset-level column had none. Both are set-based `UPDATE … FROM
/// (VALUES …)` statements, and the folder one carries an extra bound predicate
/// after the values list, which is the easiest thing here to get subtly wrong.
#[cfg(test)]
mod manual_order_tests {
    use super::*;

    async fn db() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        for stmt in [
            "CREATE TABLE assets (id TEXT PRIMARY KEY, manual_position REAL NOT NULL, \
             deleted_at TEXT)",
            "CREATE TABLE assets_folders (folder_id TEXT NOT NULL, asset_id TEXT NOT NULL, \
             position REAL NOT NULL, PRIMARY KEY (folder_id, asset_id))",
            "INSERT INTO assets VALUES ('a1', 0.0, NULL), ('a2', 1.0, NULL), ('a3', 2.0, NULL)",
            // a2 and a3 are in BOTH folders — the overlap is the point.
            "INSERT INTO assets_folders VALUES ('f1','a1',0.0), ('f1','a2',1.0), ('f1','a3',2.0)",
            "INSERT INTO assets_folders VALUES ('f2','a2',0.0), ('f2','a3',1.0)",
        ] {
            sqlx::query(stmt).execute(&pool).await.unwrap();
        }
        pool
    }

    async fn folder_order(pool: &SqlitePool, folder: &str) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT asset_id FROM assets_folders WHERE folder_id = ? ORDER BY position, asset_id",
        )
        .bind(folder)
        .fetch_all(pool)
        .await
        .unwrap()
    }

    async fn global_order(pool: &SqlitePool) -> Vec<String> {
        sqlx::query_scalar("SELECT id FROM assets ORDER BY manual_position, id")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    /// The folder predicate after the VALUES list, made executable: a reorder
    /// inside f1 must not move the same assets where they sit in f2. Drop that
    /// `AND folder_id = ?` and this test is what notices.
    #[tokio::test]
    async fn reordering_a_folder_leaves_other_folders_alone() {
        let pool = db().await;
        reorder_assets(&pool, &Scope::Folder { id: "f1".into() }, &["a3".into()], None)
            .await
            .unwrap();

        assert_eq!(folder_order(&pool, "f1").await, vec!["a3", "a1", "a2"]);
        assert_eq!(
            folder_order(&pool, "f2").await,
            vec!["a2", "a3"],
            "a reorder in f1 rewrote f2's positions"
        );
    }

    /// A folder reorder writes the MEMBERSHIP row, never the asset-level column
    /// that All and Uncategorized share.
    #[tokio::test]
    async fn reordering_a_folder_leaves_the_global_rank_alone() {
        let pool = db().await;
        reorder_assets(&pool, &Scope::Folder { id: "f1".into() }, &["a3".into()], None)
            .await
            .unwrap();
        assert_eq!(global_order(&pool).await, vec!["a1", "a2", "a3"]);
    }

    #[tokio::test]
    async fn reordering_all_writes_the_global_rank() {
        let pool = db().await;
        reorder_assets(&pool, &Scope::All, &["a3".into()], None)
            .await
            .unwrap();
        assert_eq!(global_order(&pool).await, vec!["a3", "a1", "a2"]);
    }

    /// The renumber fallback — the branch that rewrites the WHOLE scope.
    ///
    /// Reached by exhausting the fractional gap, which normally takes ~50 drops
    /// at the same spot; here the positions are seeded a nanometre apart so it
    /// fires on the first try. Worth pinning precisely because it is rare: it is
    /// the path that used to issue one statement per asset in the library.
    #[tokio::test]
    async fn an_exhausted_gap_renumbers_the_whole_scope() {
        let pool = db().await;
        sqlx::query("UPDATE assets SET manual_position = 1e-9 WHERE id = 'a2'")
            .execute(&pool)
            .await
            .unwrap();

        // Drop a3 between a1 (0.0) and a2 (1e-9): far too tight to bisect.
        reorder_assets(&pool, &Scope::All, &["a3".into()], Some("a1"))
            .await
            .unwrap();

        assert_eq!(global_order(&pool).await, vec!["a1", "a3", "a2"]);
        let positions: Vec<f64> =
            sqlx::query_scalar("SELECT manual_position FROM assets ORDER BY manual_position")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            positions,
            vec![0.0, 1.0, 2.0],
            "renumbering must leave clean whole ranks, not a tighter gap"
        );
    }
}

/// Groups of smart folders, browsed as a union.
#[cfg(test)]
mod group_tests {
    use super::*;

    async fn db() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        for stmt in [
            "CREATE TABLE assets (id TEXT PRIMARY KEY, asset_type TEXT, filename TEXT, \
             notes TEXT, source_url TEXT, file_size INTEGER, width INTEGER, height INTEGER, \
             extension TEXT, imported_date TEXT, creation_date TEXT, modified_date TEXT, \
             manual_position REAL, thumb_hash TEXT, is_animated INTEGER, deleted_at TEXT)",
            "CREATE TABLE assets_tags (asset_id TEXT, tag_id TEXT)",
            "CREATE TABLE assets_folders (folder_id TEXT, asset_id TEXT, position REAL)",
            "CREATE TABLE rule_sets (id TEXT PRIMARY KEY, kind TEXT, name TEXT, group_id TEXT, \
             position REAL, version INTEGER, query_json TEXT)",
            "INSERT INTO assets (id, asset_type, filename, notes, source_url, file_size, width, height, extension, imported_date, creation_date, modified_date, manual_position, thumb_hash, is_animated) VALUES ('i1','image','a.png',NULL,NULL,1,1,1,'png',\
             '2026-01-01T00:00:00.000Z','x','x',0,NULL,0)",
            "INSERT INTO assets (id, asset_type, filename, notes, source_url, file_size, width, height, extension, imported_date, creation_date, modified_date, manual_position, thumb_hash, is_animated) VALUES ('v1','video','b.mp4',NULL,NULL,1,1,1,'mp4',\
             '2026-01-02T00:00:00.000Z','x','x',0,NULL,0)",
            "INSERT INTO assets (id, asset_type, filename, notes, source_url, file_size, width, height, extension, imported_date, creation_date, modified_date, manual_position, thumb_hash, is_animated) VALUES ('a1','audio','c.mp3',NULL,NULL,1,1,1,'mp3',\
             '2026-01-03T00:00:00.000Z','x','x',0,NULL,0)",
        ] {
            sqlx::query(stmt).execute(&pool).await.unwrap();
        }
        pool
    }

    async fn add_member(pool: &SqlitePool, id: &str, group: Option<&str>, ty: &str) {
        let json = format!(
            r#"{{"kind":"condition","type":"media_type","types":["{ty}"]}}"#
        );
        sqlx::query("INSERT INTO rule_sets VALUES (?, 'smart', ?, ?, 0, ?, ?)")
            .bind(id).bind(id).bind(group).bind(RULE_SET_VERSION).bind(json)
            .execute(pool).await.unwrap();
    }

    async fn ids_in_group(pool: &SqlitePool, group: &str) -> Vec<String> {
        let scope = Scope::SmartGroup { id: group.into() };
        let rules = resolve_scope_rules(pool, &scope).await.unwrap();
        build_manifest_query(&scope, rules.as_ref(), &FilterSet::default(), DEFAULT_SORT)
            .build_query_as::<AssetLightRow>()
            .fetch_all(pool)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect()
    }

    /// The union, deduplicated for free — we select rows from `assets`, so an
    /// asset matching two members is still one row, with no UNION and no
    /// DISTINCT pass.
    #[tokio::test]
    async fn a_group_is_the_union_of_its_members() {
        let pool = db().await;
        add_member(&pool, "s_vid", Some("g1"), "video").await;
        add_member(&pool, "s_aud", Some("g1"), "audio").await;
        add_member(&pool, "s_img", None, "image").await; // ungrouped, must not leak in

        let mut ids = ids_in_group(&pool, "g1").await;
        ids.sort();
        assert_eq!(ids, vec!["a1", "v1"]);
    }

    /// Overlapping members must not double-count.
    #[tokio::test]
    async fn overlapping_members_yield_one_row_each() {
        let pool = db().await;
        add_member(&pool, "s_a", Some("g1"), "video").await;
        add_member(&pool, "s_b", Some("g1"), "video").await;
        assert_eq!(ids_in_group(&pool, "g1").await, vec!["v1"]);
    }

    /// The union of NOTHING is empty — not everything.
    ///
    /// This deliberately differs from an empty rule GROUP in the editor, which
    /// constrains nothing: that one is a half-written filter, where showing the
    /// library is the forgiving reading. An empty container is genuinely empty,
    /// and showing the whole library would claim otherwise.
    #[tokio::test]
    async fn an_empty_group_shows_nothing() {
        let pool = db().await;
        add_member(&pool, "s_img", None, "image").await;
        assert!(ids_in_group(&pool, "g_empty").await.is_empty());
    }

    /// `view_settings` is user-editable data, so a stored `manual` must not
    /// reach the reorder path that can't honour it for a union.
    #[tokio::test]
    async fn a_group_never_resolves_to_manual_sort() {
        let pool = db().await;
        sqlx::query("CREATE TABLE view_settings (view_key TEXT PRIMARY KEY, order_by TEXT, is_ascending INTEGER)")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO view_settings VALUES ('smartgroup:g1', 'manual', 1)")
            .execute(&pool).await.unwrap();

        let sort = resolve_sort(&pool, &Scope::SmartGroup { id: "g1".into() })
            .await
            .unwrap();
        assert!(!matches!(sort.order_by, OrderBy::Manual), "got {:?}", sort.order_by);
    }
}

/// The wire spelling of `Scope`.
///
/// Every other scope test constructs `Scope::SmartGroup` in Rust, which never
/// exercises serde's rename — so a frontend sending `"smartgroup"` against a
/// variant serde calls `"smart_group"` compiled, passed every test, and failed
/// at runtime. This pins the names the frontend must use.
#[cfg(test)]
mod scope_wire_tests {
    use super::*;

    #[test]
    fn scope_kinds_match_the_frontend() {
        for (json, expected) in [
            (r#"{"kind":"all"}"#, "all"),
            (r#"{"kind":"uncategorized"}"#, "uncategorized"),
            (r#"{"kind":"folder","id":"f1"}"#, "folder"),
            (r#"{"kind":"smart","id":"s1"}"#, "smart"),
            (r#"{"kind":"smart_group","id":"g1"}"#, "smart_group"),
            (r#"{"kind":"trash"}"#, "trash"),
        ] {
            let scope: Scope =
                serde_json::from_str(json).unwrap_or_else(|e| panic!("{expected}: {e}"));
            // Round-trips through the same name the frontend sent.
            let kind = match scope {
                Scope::All => "all",
                Scope::Uncategorized => "uncategorized",
                Scope::Folder { .. } => "folder",
                Scope::Smart { .. } => "smart",
                Scope::SmartGroup { .. } => "smart_group",
                Scope::Trash => "trash",
            };
            assert_eq!(kind, expected);
        }
    }
}

/// The wire vocabulary for pins.
///
/// Written BEFORE the frontend that consumes it, because the last three bugs in
/// this feature were all a name that compiled on both sides and disagreed across
/// the IPC boundary. `PinKind` is worse than most: it crosses THREE boundaries —
/// SQL literal, serde, and TypeScript — so all three spellings are pinned here.
#[cfg(test)]
mod pin_wire_tests {
    use super::*;

    #[test]
    fn pin_kind_spellings_agree() {
        // serde, as the frontend sends and receives it.
        assert_eq!(serde_json::to_string(&PinKind::Folder).unwrap(), r#""folder""#);
        assert_eq!(serde_json::to_string(&PinKind::Smart).unwrap(), r#""smart""#);
        let k: PinKind = serde_json::from_str(r#""smart""#).unwrap();
        assert_eq!(k, PinKind::Smart);

        // …and the SQL literals `fetch_pins` decodes from must match those.
        assert_eq!(PinKind::Folder.table(), "folders");
        assert_eq!(PinKind::Smart.table(), "rule_sets");
    }

    /// The keys the sidebar reads. A rename here silently empties the pin list.
    #[test]
    fn pinned_item_has_the_keys_the_sidebar_reads() {
        let json = serde_json::to_value(PinnedItem {
            kind: PinKind::Smart,
            id: "s1".into(),
            name: "Renders".into(),
            color: Some("blue".into()),
            position: 1.0,
        })
        .unwrap();

        for key in ["kind", "id", "name", "color", "position"] {
            assert!(json.get(key).is_some(), "PinnedItem lost `{key}`: {json}");
        }
        assert_eq!(json["kind"], "smart");
    }
}
