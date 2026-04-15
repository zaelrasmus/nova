use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::{sqlite::SqliteConnectOptions, ConnectOptions, SqlitePool};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
use tauri::{AppHandle, Runtime};
use tauri_plugin_fs::FsExt;

use chrono::{DateTime, Utc};
use rayon::prelude::*;

use std::sync::Arc;
use tokio::sync::Semaphore;
use uuid::Uuid;
use walkdir::WalkDir;

#[derive(Debug, Serialize)]
pub struct LibraryInfo {
    db_path: PathBuf,
    root_path: PathBuf,
}

#[derive(serde::Serialize, Deserialize, Clone, Copy, Debug)]
pub enum AssetType {
    Image,
    Audio,
    Video,
    Unknown,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AssetMetadata {
    pub id: String,
    pub asset_type: AssetType,
    pub filename: String,
    pub dest_path: String,  
    pub source_path: String, 
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration: Option<f64>,
    pub creation_date: String,
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

#[tauri::command]
pub async fn create_library<R: Runtime>(
    app: AppHandle<R>,
    location: String,
    name: String,
) -> Result<LibraryInfo, String> {
    let root = perform_create_library(&location, &name)
        .await
        .map_err(|e| format!("{:?}", e))?;

    app.fs_scope()
        .allow_directory(&root, true)
        .map_err(|e| format!("Error allowing directory: {}", e))?;

    Ok(LibraryInfo {
        db_path: root.join("library.db"),
        root_path: root,
    })
}

async fn perform_create_library(location: &str, name: &str) -> Result<PathBuf> {
    let root = PathBuf::from(location).join(format!("{}.library", name));

    if root.exists() {
        anyhow::bail!("Library path already exists")
    }

    let workspace_res: anyhow::Result<()> = async {
        tokio::fs::create_dir_all(root.join("assets"))
            .await
            .context("Cannot create assets dir")?;

        let db_path = root.join("library.db");
        let options = SqliteConnectOptions::new()
            .filename(&db_path)
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .synchronous(sqlx::sqlite::SqliteSynchronous::Normal);

        let pool = SqlitePool::connect_with(options)
            .await
            .context("Cannot open pool Database SQlite")?;

        sqlx::migrate!()
            .run(&pool)
            .await
            .context("Error executing migrations in database")?;

        pool.close().await;

        Ok(())
    }
    .await;

    if let Err(e) = workspace_res {
        // Rollback: remove the root directory if creation failed
        if root.exists() {
            let _ = tokio::fs::remove_dir_all(&root).await;
        }
        return Err(e).context("Error creating library");
    }

    Ok(root)
}

#[tauri::command]
pub async fn import_assets(
    source_path: String,
    library_path: String,
) -> Result<ImportResult, String> {
    let src = PathBuf::from(source_path);
    let lib = PathBuf::from(library_path);

    perform_import_assets(src, lib)
        .await
        .map_err(|e| format!("{:?}", e))
}

async fn perform_import_assets(source_dir: PathBuf, library_root: PathBuf) -> Result<ImportResult> {
    let assets_dir = library_root.join("assets");

    // Guarantee that the assets folder exists before proceeding
    if !assets_dir.exists() {
        tokio::fs::create_dir_all(&assets_dir)
            .await
            .context("Assets folder could not be created")?;
    }

    let mut folders = Vec::new();
    let mut folder_map = HashMap::new();

    // 1. Scan folder structure
    for entry in WalkDir::new(&source_dir)
        .min_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_dir() {
            let path = entry.path().to_path_buf();
            let id = Uuid::new_v4().to_string();

            // Search for the parent ID in our map
            let parent_id = path.parent().and_then(|p| folder_map.get(p).cloned());

            let folder_obj = Folder {
                id: id.clone(),
                name: entry.file_name().to_string_lossy().into_owned(),
                parent_id,
                order_by: "name".to_string(),
                is_ascending: "1".to_string(),
                original_path: path.to_string_lossy().into_owned(),
            };

            folder_map.insert(path, id);
            folders.push(folder_obj);
        }
    }

    // 2. Collect file paths
    let file_paths: Vec<PathBuf> = WalkDir::new(&source_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| matches!(get_asset_type(p), AssetType::Image)) // Only images right now
        .collect();

    // 3. Proccess assets metadata parallel
    let asset_tasks: Vec<AssetMetadata> = file_paths
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
            let dest_path = assets_dir.join(format!("{}.{}", id, ext));

            Some(AssetMetadata {
                id,
                asset_type,
                filename: src.file_name()?.to_string_lossy().into_owned(),
                dest_path: dest_path.to_string_lossy().into_owned(),
                source_path: src.to_string_lossy().into_owned(),
                width,
                height,
                duration: None,
                creation_date: created.to_rfc3339(),
                modified_date: modified.to_rfc3339(),
            })
        })
        .collect();

    // 4. Copy files
    let semaphore = Arc::new(Semaphore::new(10));
    let mut handles = Vec::with_capacity(asset_tasks.len());

    for task in &asset_tasks {
        let permit = Arc::clone(&semaphore)
            .acquire_owned()
            .await
            .context("Error adquiring semaphore")?;

        let src = PathBuf::from(&task.source_path);
        let dst = PathBuf::from(&task.dest_path);

        handles.push(tokio::spawn(async move {
            let _permit = permit;
            tokio::fs::copy(&src, &dst).await
        }));
    }

    // Wait for all copies to finish
    for handle in handles {
        handle.await.context("panic in copy thread")??;
    }

    Ok(ImportResult {
        folders,
        assets: asset_tasks,
        path_links: folder_map
            .into_iter()
            .map(|(k, v)| (k.to_string_lossy().into_owned(), v))
            .collect(),
    })
}
