use crate::fs;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rayon::prelude::{IntoParallelIterator, ParallelIterator};
use serde::{Deserialize, Serialize};
use sqlx::{sqlite::SqlitePool, FromRow, QueryBuilder, Type};
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

    #[serde(skip)]
    #[sqlx(skip)]
    pub source_path: String,

    pub width: u32,
    pub height: u32,

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
    pub order_by: String,     // TODO: Use an enum
    pub is_ascending: String, // TODO: Use a bool
    pub original_path: String,
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

#[instrument(skip(pool, assets), fields(count = assets.len()))]
async fn persist_assets(pool: &SqlitePool, assets: &[AssetMetadata]) -> Result<()> {
    let start = std::time::Instant::now();

    // SQLite caps bound parameters at 32766. With 10 columns per row, keep each
    // multi-row INSERT well under that (10 * 2000 = 20000 params per statement)
    const ROWS_PER_INSERT: usize = 2000;

    let mut tx = pool
        .begin()
        .await
        .context("Failed to begin database transaction")?;

    for chunk in assets.chunks(ROWS_PER_INSERT) {
        let mut qb = QueryBuilder::new(
            "INSERT INTO assets (id, asset_type, filename, extension, path, \
                width, height, imported_date, creation_date, modified_date) ",
        );

        qb.push_values(chunk, |mut b, asset| {
            b.push_bind(&asset.id)
                .push_bind(asset.asset_type)
                .push_bind(&asset.filename)
                .push_bind(&asset.extension)
                .push_bind(&asset.dest_path)
                .push_bind(asset.width)
                .push_bind(asset.height)
                .push_bind(&asset.imported_date)
                .push_bind(&asset.creation_date)
                .push_bind(&asset.modified_date);
        });

        qb.build()
            .execute(&mut *tx)
            .await
            .context("Failed to batch insert asset chunk")?;
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
        count = assets.len(),
        elapsed_ms = start.elapsed().as_millis(),
        "Assets persisted to database"
    );
    Ok(())
}

fn build_asset_metadata(src: PathBuf, dest_dir: &Path) -> Option<AssetMetadata> {
    let asset_type = detect_asset_type(&src);
    let (width, height) = image::image_dimensions(&src)
        .inspect_err(|e| warn!(path = ?src, error = %e, "Could not read image dimensions"))
        .ok()?;
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
    let dest_path = dest_dir.join(format!("{}.{}", id, ext));

    Some(AssetMetadata {
        id,
        asset_type,
        filename: src.file_name()?.to_string_lossy().into_owned(),
        extension: ext.to_string(),
        dest_path: dest_path.to_string_lossy().into_owned(),
        source_path: src.to_string_lossy().into_owned(),
        width,
        height,
        imported_date: Utc::now().to_rfc3339(),
        creation_date: created.to_rfc3339(),
        modified_date: modified.to_rfc3339(),
    })
}

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

#[instrument(skip(pool))]
pub async fn fetch_assets(pool: &SqlitePool) -> Result<Vec<AssetMetadata>> {
    let assets = sqlx::query_as::<_, AssetMetadata>(
        r#"
        SELECT id, asset_type, filename, extension, path, width, height,
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

#[instrument(skip(pool))]
pub async fn insert_test_asset(pool: &SqlitePool, name: &str) -> Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    sqlx::query(
        "INSERT INTO assets (id, asset_type, filename, extension, path, width, height,
                             imported_date, creation_date, modified_date)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind("image")
    .bind(name)
    .bind("png")
    .bind(format!("assets/{}", name))
    .bind(0_i64) // width — placeholder for the test row
    .bind(0_i64) // height
    .bind(&now)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await
    .with_context(|| format!("Failed to insert test asset '{}'", name))?;

    debug!(id = %id, name = name, "Test asset inserted");
    Ok(id)
}

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
