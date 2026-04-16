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

use sqlx::{decode::Decode, encode::Encode, sqlite::Sqlite, FromRow, Type};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::Semaphore;
use uuid::Uuid;
use walkdir::WalkDir;

#[derive(Debug, Serialize)]
pub struct LibraryInfo {
    db_path: PathBuf,
    root_path: PathBuf,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Type)]
#[sqlx(transparent)] // Esto permite que se trate como el tipo subyacente (Texto)
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

pub struct DbState {
    pub pool: Arc<Mutex<Option<SqlitePool>>>,
}

impl DbState {
    pub fn new() -> Self {
        Self {
            pool: Arc::new(Mutex::new(None)),
        }
    }
}

#[tauri::command]
pub async fn connect_library(
    library_path: String,
    state: tauri::State<'_, DbState>,
) -> Result<String, String> {
    let base_path = std::path::PathBuf::from(&library_path);

    let path = std::path::PathBuf::from(&library_path).join("library.db");

    // let db_path = if base_path.is_file() {
    //     base_path // Si el usuario eligió el archivo .db directamente
    // } else {
    //     base_path.join("library.db") // Si eligió la carpeta
    // };

    println!("Intentando abrir base de datos en: {:?}", path);

    if !path.exists() {
        return Err(format!("El archivo no existe en la ruta: {:?}", path));
    }

    let options = SqliteConnectOptions::new()
        .filename(&path)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .synchronous(sqlx::sqlite::SqliteSynchronous::Normal);

    let pool = SqlitePool::connect_with(options)
        .await
        .map_err(|e| format!("Error real de SQLite: {}", e))?;

    let mut current_pool = state.pool.lock().await;

    // If a pool is already connected, disconnect it first
    if let Some(old_pool) = current_pool.take() {
        old_pool.close().await;
    }

    *current_pool = Some(pool);

    Ok("Connection established successfully".to_string())
}

#[tauri::command]
pub async fn inject_test_asset(
    name: String,
    state: tauri::State<'_, DbState>,
) -> Result<String, String> {
    let pool_lock = state.pool.lock().await;
    let pool = pool_lock.as_ref().ok_or("No hay librería conectada")?;

    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();

    sqlx::query(
        "INSERT INTO assets (id, asset_type, filename, extension, path, imported_date, creation_date, modified_date)
         VALUES (?, ?, ?, ?, ?, ?,?,? )",
    )
    .bind(&id)
    .bind("image")
    .bind(&name)
    .bind(format!("assets/{}", name))
    .bind("png")
    .bind(&now)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await
    .map_err(|e| e.to_string())?;

    Ok(format!("Inyectado con éxito: {}", name))
}

#[tauri::command]
pub async fn fetch_assets(state: tauri::State<'_, DbState>) -> Result<Vec<AssetMetadata>, String> {
    let pool_lock = state.pool.lock().await;
    let pool = pool_lock.as_ref().ok_or("Not library connected")?;

    let assets = sqlx::query_as::<_, AssetMetadata>(
        r#"
        SELECT
            id,
            asset_type,
            filename,
            extension,
            path,
            imported_date,
            creation_date,
            modified_date
        FROM assets
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Error en Fetch: {}", e))?;

    Ok(assets)
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
    state: tauri::State<'_, DbState>,
) -> Result<ImportResult, String> {
    let src = PathBuf::from(source_path);
    // let lib = PathBuf::from(library_path);

    let pool_lock = state.pool.lock().await;
    let pool = pool_lock.as_ref().ok_or("Not active library connected")?;

    perform_import_assets(src, pool.clone())
        .await
        .map_err(|e| format!("{:?}", e))
}

async fn perform_import_assets(source_dir: PathBuf, pool: SqlitePool) -> Result<ImportResult> {
    // Database
    let db_info: (i32, String, String) = sqlx::query_as("PRAGMA database_list")
        .fetch_one(&pool)
        .await
        .context("Failed to get database path")?;
    let library_root = PathBuf::from(db_info.2).parent().unwrap().to_path_buf();

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

            let dest_path = format!("assets/{}.{}", id, ext);
            let full_dest_path = assets_dir.join(format!("{}.{}", id, ext));

            Some(AssetMetadata {
                id,
                asset_type,
                filename: src.file_name()?.to_string_lossy().into_owned(),
                extension: ext.to_string(),
                dest_path,
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

    for task in &asset_tasks {
        let permit = Arc::clone(&semaphore)
            .acquire_owned()
            .await
            .context("Error adquiring semaphore")?;

        let src = PathBuf::from(&task.source_path);
        let dst = library_root.join(&task.dest_path);
        handles.push(tokio::spawn(async move {
            let _permit = permit;
            tokio::fs::copy(&src, &dst).await
        }));
    }

    // Wait for all copies to finish
    for handle in handles {
        handle.await.context("error copying file")??;
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
