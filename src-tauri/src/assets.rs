//! Asset domain — models, file-type detection, and the import pipeline.
//!
//! The import pipeline runs in five stages:
//! 1. Resolve the library root from the active database connection.
//! 2. Scan the source directory for subdirectories and files.
//! 3. Build asset metadata in parallel (Rayon — CPU-bound).
//! 4. Copy files concurrently (Tokio + bounded semaphore — I/O-bound).
//! 5. Persist all metadata in a single atomic database transaction.

use crate::fs;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rayon::prelude::{IntoParallelIterator, ParallelIterator};
use serde::{Deserialize, Serialize};
use sqlx::{sqlite::SqlitePool, FromRow, Type};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use tokio::sync::Semaphore;
use tracing::{debug, info, instrument, warn};

// ─── Models ──────────────────────────────────────────────────────────────────

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Type)]
#[sqlx(rename_all = "lowercase")]
pub enum AssetType {
    Image,
    Audio,
    Video,
    Unknown,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, FromRow)]
pub struct AssetMetadata {
    pub id: String,
    pub asset_type: AssetType,
    pub filename: String,
    pub extension: String,

    #[sqlx(rename = "path")]
    pub dest_path: String,

    /// Populated during the import pipeline only — not stored in the database
    /// and not serialized to the frontend. Kept on this struct to avoid a
    /// separate staging type.
    #[serde(skip)]
    #[sqlx(skip)]
    pub source_path: String,

    pub imported_date: String,
    #[sqlx(rename = "creation_date")]
    pub creation_date: String,
    #[sqlx(rename = "modified_date")]
    pub modified_date: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Folder {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,
    pub order_by: String,
    pub is_ascending: String,
    pub original_path: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ImportResult {
    pub folders: Vec<Folder>,
    pub assets: Vec<AssetMetadata>,
    pub path_links: HashMap<String, String>,
}

// ─── Progress reporting ───────────────────────────────────────────────────────

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
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

/// Decouples the import pipeline from Tauri's event system.
///
/// The pipeline depends only on this trait, which allows it to be driven by a
/// `TauriProgressReporter` in production or a no-op mock in unit tests — no
/// conditional compilation or feature flags required.
pub trait ProgressReporter: Send + Sync {
    fn report(&self, progress: ImportProgress);
}

// ─── File-type detection ──────────────────────────────────────────────────────

// These arrays must remain sorted — `binary_search` requires it.
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

// ─── Internal helpers ─────────────────────────────────────────────────────────

/// Resolves the library's root directory from the active pool.
///
/// Uses `PRAGMA database_list` to read the physical file path at runtime, which
/// avoids storing the path redundantly in `DbState` and keeps it always in sync
/// with the actual connection.
#[instrument(skip(pool))]
async fn resolve_library_root(pool: &SqlitePool) -> Result<PathBuf> {
    let db_info: (i32, String, String) = sqlx::query_as("PRAGMA database_list")
        .fetch_one(pool)
        .await
        .context("Failed to read library path via PRAGMA database_list")?;

    PathBuf::from(db_info.2)
        .parent()
        .map(|p| p.to_path_buf())
        .context("Library database has an invalid path structure")
}

/// Inserts all staged assets in a single transaction.
///
/// Atomicity is intentional: either the entire batch is saved or nothing is.
/// A partial write would leave files on disk with no corresponding database
/// record, making them invisible to the library without a manual repair.
#[instrument(skip(pool, assets), fields(count = assets.len()))]
async fn persist_assets(pool: &SqlitePool, assets: &[AssetMetadata]) -> Result<()> {
    let start = std::time::Instant::now();

    let mut tx = pool
        .begin()
        .await
        .context("Failed to begin database transaction")?;

    for asset in assets {
        sqlx::query(
            "INSERT INTO assets (id, asset_type, filename, extension, path,
                                 imported_date, creation_date, modified_date)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&asset.id)
        .bind("image") // TODO: bind asset.asset_type directly once the migration uses the enum column
        .bind(&asset.filename)
        .bind(&asset.extension)
        .bind(&asset.dest_path)
        .bind(&asset.imported_date)
        .bind(&asset.creation_date)
        .bind(&asset.modified_date)
        .execute(&mut *tx)
        .await
        .with_context(|| format!("Failed to insert asset '{}'", asset.filename))?;
    }

    tx.commit()
        .await
        .context("Failed to commit asset transaction")?;

    info!(
        count = assets.len(),
        elapsed_ms = start.elapsed().as_millis(),
        "Assets persisted to database"
    );
    Ok(())
}

/// Attempts to construct an `AssetMetadata` from a source path.
///
/// Returns `None` on any I/O failure rather than propagating an error — a
/// single unreadable file should not abort the entire import batch.
fn build_asset_metadata(src: PathBuf, dest_dir: &Path) -> Option<AssetMetadata> {
    let asset_type = detect_asset_type(&src);
    let meta = std::fs::metadata(&src)
        .inspect_err(|e| warn!(path = ?src, error = %e, "Could not read file metadata, skipping"))
        .ok()?;

    let created: DateTime<Utc> = meta
        .created()
        .inspect_err(|e| debug!(path = ?src, error = %e, "btime unavailable, falling back"))
        .ok()?
        .into();
    let modified: DateTime<Utc> = meta.modified().ok()?.into();

    let ext = src.extension()?.to_str()?;
    let id = uuid::Uuid::new_v4().to_string();
    let dest_path = dest_dir.join(format!("{}.{}", id, ext));

    Some(AssetMetadata {
        id,
        asset_type,
        filename: src.file_name()?.to_string_lossy().into_owned(),
        extension: ext.to_string(),
        dest_path: dest_path.to_string_lossy().into_owned(),
        source_path: src.to_string_lossy().into_owned(),
        imported_date: Utc::now().to_rfc3339(),
        creation_date: created.to_rfc3339(),
        modified_date: modified.to_rfc3339(),
    })
}

/// Copies assets to the library directory with bounded concurrency.
///
/// The semaphore is capped at 10 to avoid exhausting OS file descriptors on
/// large imports. Individual failures are counted and logged but do not abort
/// the batch — a partial import is preferable to losing all progress.
#[instrument(skip(reporter, assets), fields(total = assets.len()))]
async fn copy_assets(reporter: Arc<dyn ProgressReporter>, assets: &[AssetMetadata]) -> Result<()> {
    let start = std::time::Instant::now();
    let semaphore = Arc::new(Semaphore::new(10));
    let completed = Arc::new(AtomicUsize::new(0));
    let failed = Arc::new(AtomicUsize::new(0));
    let total = assets.len();
    let mut handles = Vec::with_capacity(total);

    for asset in assets {
        let permit = Arc::clone(&semaphore)
            .acquire_owned()
            .await
            .context("Failed to acquire semaphore permit for file copy")?;

        let src = PathBuf::from(&asset.source_path);
        let dst = PathBuf::from(&asset.dest_path);
        let reporter = Arc::clone(&reporter);
        let completed = Arc::clone(&completed);
        let failed = Arc::clone(&failed);
        let filename = asset.filename.clone();

        handles.push(tokio::spawn(async move {
            let _permit = permit;

            match tokio::fs::copy(&src, &dst).await {
                Ok(bytes) => {
                    let current = completed.fetch_add(1, Ordering::SeqCst) + 1;
                    debug!(file = %filename, bytes, "File copied");

                    reporter.report(ImportProgress {
                        stage: ImportStage::CopyingFiles,
                        current,
                        total,
                        message: format!("Importing: {}", filename),
                    });
                }
                Err(e) => {
                    failed.fetch_add(1, Ordering::SeqCst);
                    warn!(src = ?src, error = %e, "Failed to copy file, skipping");
                }
            }
        }));
    }

    for handle in handles {
        handle.await.context("File copy task panicked")?;
    }

    let failed_count = failed.load(Ordering::SeqCst);
    if failed_count > 0 {
        warn!(
            failed = failed_count,
            total, "Import completed with copy failures"
        );
    }

    info!(
        total,
        failed = failed_count,
        elapsed_ms = start.elapsed().as_millis(),
        "File copy stage complete"
    );

    Ok(())
}

// ─── Public API ───────────────────────────────────────────────────────────────

/// Returns all assets currently stored in the library database.
#[instrument(skip(pool))]
pub async fn fetch_assets(pool: &SqlitePool) -> Result<Vec<AssetMetadata>> {
    let assets = sqlx::query_as::<_, AssetMetadata>(
        r#"
        SELECT id, asset_type, filename, extension, path,
               imported_date, creation_date, modified_date
        FROM assets
        "#,
    )
    .fetch_all(pool)
    .await
    .context("Failed to fetch assets from database")?;

    debug!(count = assets.len(), "Assets fetched from database");
    Ok(assets)
}

/// Inserts a synthetic asset record for development and testing.
#[instrument(skip(pool))]
pub async fn insert_test_asset(pool: &SqlitePool, name: &str) -> Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    sqlx::query(
        "INSERT INTO assets (id, asset_type, filename, extension, path,
                             imported_date, creation_date, modified_date)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind("image")
    .bind(name)
    .bind("png")
    .bind(format!("assets/{}", name))
    .bind(&now)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await
    .with_context(|| format!("Failed to insert test asset '{}'", name))?;

    debug!(id = %id, name = name, "Test asset inserted");
    Ok(id)
}

/// Runs the full import pipeline for a source directory.
///
/// Progress is emitted through `reporter` so this function stays independent
/// of Tauri and can be unit-tested with a mock reporter.
#[instrument(skip(reporter, pool), fields(source = %source_dir.display()))]
pub async fn import_assets(
    reporter: Arc<dyn ProgressReporter>,
    source_dir: PathBuf,
    pool: SqlitePool,
) -> Result<ImportResult> {
    let pipeline_start = std::time::Instant::now();

    reporter.report(ImportProgress {
        stage: ImportStage::Scanning,
        current: 0,
        total: 0,
        message: "Scanning folder structure...".into(),
    });

    // Stage 1: Resolve destination directory.
    let library_root = resolve_library_root(&pool).await?;
    let assets_dir = library_root.join("assets");
    fs::ensure_dir(&assets_dir).await?;

    // Stage 2: Walk directory tree.
    let scan_start = std::time::Instant::now();
    let (folders, folder_id_by_path) = fs::scan_directories(&source_dir);
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

    let staged_assets: Vec<AssetMetadata> = discovered_files
        .into_par_iter()
        .filter(|p| matches!(detect_asset_type(p), AssetType::Image))
        .filter_map(|src| build_asset_metadata(src, &assets_dir))
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

    // Stage 4: Copy files with bounded concurrency (I/O-bound via Tokio).
    copy_assets(reporter.clone(), &staged_assets).await?;

    // Stage 5: Persist all metadata atomically.
    reporter.report(ImportProgress {
        stage: ImportStage::Finalizing,
        current: 0,
        total: staged_assets.len(),
        message: "Saving to database...".into(),
    });

    persist_assets(&pool, &staged_assets).await?;

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
