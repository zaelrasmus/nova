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
use tauri::Emitter;
use tokio::sync::Semaphore;
use uuid::Uuid;
use walkdir::WalkDir;

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Type)]
#[sqlx(transparent)]
pub struct AssetTypeWrapper(pub AssetType);

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

#[derive(Clone, Serialize)]
struct ImportProgress {
    current: usize,
    total: usize,
    percentage: f64,
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

pub async fn list_assets(pool: &SqlitePool) -> anyhow::Result<Vec<AssetMetadata>> {
    let assets = sqlx::query_as::<_, AssetMetadata>(
        r#"
        SELECT id, asset_type, filename, extension, path,
               imported_date, creation_date, modified_date
        FROM assets
        "#,
    )
    .fetch_all(pool)
    .await?;

    Ok(assets)
}

pub async fn add_test_asset(pool: &SqlitePool, name: &str) -> anyhow::Result<String> {
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
    .await?;

    Ok(id)
}
pub async fn perform_import_assets(
    window: tauri::Window,
    source_dir: PathBuf,
    pool: SqlitePool,
) -> Result<ImportResult> {
    // Database
    let db_info: (i32, String, String) = sqlx::query_as("PRAGMA database_list")
        .fetch_one(&pool)
        .await
        .context("Failed to get database path")?;
    let library_root = PathBuf::from(db_info.2).parent().unwrap().to_path_buf();

    let assets_dir = library_root.join("assets");

    // Guarantee that the assets folder exists before proceeding
    fs::ensure_dir(&assets_dir).await?;

    // 1. Scan Folder Structure
    let (folders, folder_map) = fs::scan_folder_structure(&source_dir);

    // 2. Collect file paths
    let all_files = fs::collect_file_paths(&source_dir);

    // 3. Proccess assets metadata parallel
    let asset_tasks: Vec<AssetMetadata> = all_files
        .into_par_iter()
        .filter_map(|src| {
            let asset_type = get_asset_type(&src);
            let meta = std::fs::metadata(&src).ok()?;
            let created: DateTime<Utc> = meta.created().ok()?.into();
            let modified: DateTime<Utc> = meta.modified().ok()?.into();

            let mut width = None;
            let mut height = None;

            if let AssetType::Image = asset_type {
                if let Ok(dim) = image::image_dimensions(&src) {
                    width = Some(dim.0);
                    height = Some(dim.1);
                }
            }

            let ext = src.extension()?.to_str()?;
            let id = Uuid::new_v4().to_string();

            let full_dest_path = assets_dir.join(format!("{}.{}", id, ext));
            let dest_path_string = full_dest_path.to_string_lossy().into_owned();

            Some(AssetMetadata {
                id,
                asset_type,
                filename: src.file_name()?.to_string_lossy().into_owned(),
                extension: ext.to_string(),
                dest_path: dest_path_string,
                source_path: src.to_string_lossy().into_owned(),
                imported_date: created.to_rfc3339(),
                creation_date: created.to_rfc3339(),
                modified_date: modified.to_rfc3339(),
            })
        })
        .collect();

    // 4. Copy files
    let semaphore = Arc::new(Semaphore::new(10));
    let mut handles = Vec::with_capacity(asset_tasks.len());

    let total_assets = asset_tasks.len();
    let completed_count = Arc::new(AtomicUsize::new(0));

    let last_emit = Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
    let emit_interval = std::time::Duration::from_millis(100);

    for task in &asset_tasks {
        let permit = Arc::clone(&semaphore)
            .acquire_owned()
            .await
            .context("Error adquiring semaphore")?;

        let src = PathBuf::from(&task.source_path);
        let dst = PathBuf::from(&task.dest_path);

        let window_clone = window.clone();
        let counter = Arc::clone(&completed_count);
        let last_emit_clone = Arc::clone(&last_emit);

        handles.push(tokio::spawn(async move {
            let _permit = permit;
            let _ = tokio::fs::copy(&src, &dst).await;

            let current = counter.fetch_add(1, Ordering::SeqCst) + 1;

            if let Ok(mut last_time) = last_emit_clone.lock() {
                if last_time.elapsed() >= emit_interval || current == total_assets {
                    let percentage = (current as f64 / total_assets as f64) * 100.0;

                    let _ = window_clone.emit(
                        "import-progress",
                        ImportProgress {
                            current,
                            total: total_assets,
                            percentage,
                        },
                    );
                    *last_time = std::time::Instant::now();
                }
            }

            // let current = counter.fetch_add(1, Ordering::SeqCst) + 1;
            // let percentage = (current as f64 / total_assets as f64) * 100.0;

            // let _ = window_clone.emit(
            //     "import-progress",
            //     ImportProgress {
            //         current,
            //         total: total_assets,
            //         percentage,
            //     },
            // );
        }));
    }

    // Wait for all copies to finish
    for handle in handles {
        handle.await.context("error copying file")?;
    }

    // 5. Insert into database
    let mut tx = pool.begin().await.context("Error starting transaction")?;

    for asset in &asset_tasks {
        sqlx::query("INSERT INTO assets (id, asset_type, filename, extension, path, imported_date, creation_date, modified_date) VALUES (?, ?, ?, ?, ?, ? , ? , ?)")
            .bind(&asset.id)
            .bind("image")
            .bind(&asset.filename)
            .bind(&asset.extension)
            .bind(&asset.dest_path)
            .bind(&asset.imported_date)
            .bind(&asset.creation_date)
            .bind(&asset.modified_date)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await.context("Error committing transaction")?;

    Ok(ImportResult {
        folders,
        assets: asset_tasks,
        path_links: folder_map
            .into_iter()
            .map(|(k, v)| (k.to_string_lossy().into_owned(), v))
            .collect(),
    })
}
