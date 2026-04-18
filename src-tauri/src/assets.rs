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

pub trait ProgressReporter: Send + Sync {
    fn report(&self, progress: ImportProgress);
}

const IMG_EXTS: &[&str] = &["bmp", "gif", "jfif", "jpeg", "jpg", "png", "webp"];
const VID_EXTS: &[&str] = &["avi", "mkv", "mov", "mp4", "webm"];
const AUD_EXTS: &[&str] = &["flac", "m4a", "mp3", "ogg", "wav"];

fn get_asset_type(path: &Path) -> AssetType {
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

// #{instrument(skip(pool))}
async fn get_library_root(pool: &SqlitePool) -> Result<PathBuf> {
    let db_info: (i32, String, String) = sqlx::query_as("PRAGMA database_list")
        .fetch_one(pool)
        .await
        .context("Failed to read library database path via PRAGMA database_list")?;

    PathBuf::from(db_info.2)
        .parent()
        .map(|p| p.to_path_buf())
        .context("Library database has an invalid path structure")
}

// #[instrument(skip(pool, assets), fields(count = assets.len()))]
async fn save_assets_to_db(pool: &SqlitePool, assets: &[AssetMetadata]) -> Result<()> {
    // let start = std::time::Instant::now();

    let mut tx = pool
        .begin()
        .await
        .context("Failed to begin database transaction")?;

    for asset in assets {
        sqlx::query(
            "INSERT INTO assets (id, asset_type, filename, extension, path, imported_date, creation_date, modified_date)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
        )
        .bind(&asset.id)
        .bind("image") // TODO: bind asset.asset_type once the Type enum is implemented
        .bind(&asset.filename)
        .bind(&asset.extension)
        .bind(&asset.dest_path)
        .bind(&asset.imported_date)
        .bind(&asset.creation_date)
        .bind(&asset.modified_date)
        .execute(&mut *tx)
        .await.with_context(|| format!("Failed to insert asset '{}'", asset.filename))?;
    }

    tx.commit()
        .await
        .context("Failed to commit asset transaction")?;

    // info!(
    //     count = assets.len(),
    //     elapsed_ms = start.elapsed().as_millis(),
    //     "Assets persisted to database"
    // );
    Ok(())
}

// #[instrument(skip(pool))]
pub async fn list_assets(pool: &SqlitePool) -> anyhow::Result<Vec<AssetMetadata>> {
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

    // debug!(count = assets.len(), "Assets fetched from database");
    Ok(assets)
}

// Insert a placeholder asset into the database for testing purposes
pub async fn add_test_asset(pool: &SqlitePool, name: &str) -> Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    sqlx::query(
        "INSERT INTO assets (id, asset_type, filename, extension, path, imported_date, creation_date, modified_date)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
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
    .await.with_context(|| format!("Failed to insert test asset '{}'", name))?;

    // debug!(id = %id, name = name, "Test asset inserted");

    Ok(id)
}
pub async fn perform_import_assets(
    reporter: Arc<dyn ProgressReporter>,
    source_dir: PathBuf,
    pool: SqlitePool,
) -> Result<ImportResult> {
    // let import_start = std::time::Instant::now();

    reporter.report(ImportProgress {
        stage: ImportStage::Scanning,
        current: 0,
        total: 0,
        message: "Scanning folder structure...".into(),
    });

    // Stage 1: Resolve library root and assets directory
    let library_root = get_library_root(&pool).await?;
    let assets_dir = library_root.join("assets");

    // Guarantee that the assets folder exists before proceeding
    fs::ensure_dir(&assets_dir).await?;

    // Stage 2: Scan folder structure and collect file paths

    // let scan_start = std::time::Instant::now();
    let (folders, folder_map) = fs::scan_folder_structure(&source_dir);
    let all_files = fs::collect_file_paths(&source_dir);
    let total_files = all_files.len();

    // info!(
    //         folders = folders.len(),
    //         files = total_files,
    //         elapsed_ms = scan_start.elapsed().as_millis(),
    //         "Scan complete"
    //     );

    reporter.report(ImportProgress {
        stage: ImportStage::ProcessingMetadata,
        current: 0,
        total: total_files,
        message: format!("Processing {} files...", total_files),
    });

    // Stage 3: Process metadata in parallel using rayon
    // let meta_start = std::time::Instant::now();

    let asset_tasks: Vec<AssetMetadata> = all_files
        .into_par_iter()
        .filter(|p| matches!(get_asset_type(p), AssetType::Image))
        .filter_map(|src| process_asset_metadata(src, &assets_dir))
        .collect();

    // info!(
    //     count = asset_tasks.len(),
    //     elapsed_ms = meta_start.elapsed().as_millis(),
    //     "Metadata processing complete"
    // );

    reporter.report(ImportProgress {
        stage: ImportStage::CopyingFiles,
        current: 0,
        total: asset_tasks.len(),
        message: "Copying files...".into(),
    });

    // Stage 4. Copy files concurrently
    copy_files_with_progress(reporter.clone(), &asset_tasks).await?;

    // 5. Persist asset metadata to database
    reporter.report(ImportProgress {
        stage: ImportStage::Finalizing,
        current: 0,
        total: asset_tasks.len(),
        message: "Saving to database...".into(),
    });

    save_assets_to_db(&pool, &asset_tasks).await?;

    // info!(
    //     assets = asset_tasks.len(),
    //     folders = folders.len(),
    //     total_elapsed_ms = import_start.elapsed().as_millis(),
    //     "Import pipeline complete"
    // );

    Ok(ImportResult {
        folders,
        assets: asset_tasks,
        path_links: folder_map
            .into_iter()
            .map(|(k, v)| (k.to_string_lossy().into_owned(), v))
            .collect(),
    })
}

fn process_asset_metadata(src: PathBuf, dest_dir: &Path) -> Option<AssetMetadata> {
    let asset_type = get_asset_type(&src);
    let meta = std::fs::metadata(&src).ok()?;
    // let meta = std::fs::metadata(&src)
    //         .inspect_err(|e| warn!(path = ?src, error = %e, "Could not read file metadata, skipping"))
    //         .ok()?;
    let created: DateTime<Utc> = meta.created().ok()?.into();
    // let created: DateTime<Utc> = meta
    //         .created()
    //         .inspect_err(|e| debug!(path = ?src, error = %e, "btime unavailable, falling back"))
    //         .ok()?
    //         .into();
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

// #{instrument(skip(reporter, assets), fields(total = assets.len()))}
async fn copy_files_with_progress(
    reporter: Arc<dyn ProgressReporter>,
    assets: &[AssetMetadata],
) -> Result<()> {
    // let start = std::time::Instant::now();
    let semaphore = Arc::new(Semaphore::new(10));
    let completed = Arc::new(AtomicUsize::new(0));
    let failed = Arc::new(AtomicUsize::new(0));
    let total = assets.len();
    let mut handles = Vec::with_capacity(total);

    // for asset in assets {
    //     let permit = Arc::clone(&semaphore).acquire_owned().await.context("Failed to acquire semaphore permit for file copy")?;

    //     let src = PathBuf::from(&asset.source_path);
    //     let dst = PathBuf::from(&asset.dest_path);

    //     let r = Arc::clone(&reporter);
    //     let counter = Arc::clone(&completed_count);
    //     let filename = asset.filename.clone();

    //     handles.push(tokio::spawn(async move {
    //         let _permit = permit;

    //         match tokio::fs::copy(&src, &dst).await {
    //             Ok(bytes) => {
    //                 let current = completed.fetch
    //             }
    //         }
    //         // if tokio::fs::copy(&src, &dst).await.is_ok() {
    //         //     let current = counter.fetch_add(1, Ordering::SeqCst) + 1;

    //         //     r.report(ImportProgress {
    //         //         stage: ImportStage::CopyingFiles,
    //         //         current,
    //         //         total,
    //         //         message: format!("Importing: {}", filename),
    //         //     });
    //         // }
    //     }));
    // }

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
                    // debug!(file = %filename, bytes, "File copied");

                    reporter.report(ImportProgress {
                        stage: ImportStage::CopyingFiles,
                        current,
                        total,
                        message: format!("Importing: {}", filename),
                    });
                }
                Err(e) => {
                    // We log and count failures but don't abort the entire import.
                    // A partial import is better than losing all progress on a bad file.
                    failed.fetch_add(1, Ordering::SeqCst);
                    // warn!(src = ?src, error = %e, "Failed to copy file, skipping");
                }
            }
        }));
    }

    for h in handles {
        h.await.context("File copy task panicked")?;
    }

    let failed_count = failed.load(Ordering::SeqCst);
    // if failed_count > 0 {
    //     warn!(
    //         failed = failed_count,
    //         total, "Import completed with failed copies"
    //     );
    // }

    // info!(
    //     total,
    //     failed = failed_count,
    //     elapsed_ms = start.elapsed().as_millis(),
    //     "File copy stage complete"
    // );

    Ok(())
}
