use anyhow::{bail, Context, Result};
use chrono::Utc;
use sqlx::{sqlite::SqliteConnectOptions, SqlitePool};
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Runtime};
use tauri_plugin_fs::FsExt;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::assets::{perform_import_assets, AssetMetadata, ImportResult};
use crate::library::{perform_create_library, LibraryInfo};

// #[derive(Debug, Serialize)]
// pub struct LibraryInfo {
//     db_path: PathBuf,
//     root_path: PathBuf,
// }

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

// async fn perform_create_library(location: &str, name: &str) -> Result<PathBuf> {
//     let root = PathBuf::from(location).join(format!("{}.library", name));

//     if root.exists() {
//         anyhow::bail!("Library path already exists")
//     }

//     let workspace_res: anyhow::Result<()> = async {
//         tokio::fs::create_dir_all(root.join("assets"))
//             .await
//             .context("Cannot create assets dir")?;

//         let db_path = root.join("library.db");
//         let options = SqliteConnectOptions::new()
//             .filename(&db_path)
//             .create_if_missing(true)
//             .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
//             .synchronous(sqlx::sqlite::SqliteSynchronous::Normal);

//         let pool = SqlitePool::connect_with(options)
//             .await
//             .context("Cannot open pool Database SQlite")?;

//         sqlx::migrate!()
//             .run(&pool)
//             .await
//             .context("Error executing migrations in database")?;

//         pool.close().await;

//         Ok(())
//     }
//     .await;

//     if let Err(e) = workspace_res {
//         // Rollback: remove the root directory if creation failed
//         if root.exists() {
//             let _ = tokio::fs::remove_dir_all(&root).await;
//         }
//         return Err(e).context("Error creating library");
//     }

//     Ok(root)
// }

#[tauri::command]
pub async fn import_assets(
    window: tauri::Window,
    source_path: String,
    state: tauri::State<'_, DbState>,
) -> Result<ImportResult, String> {
    let src = PathBuf::from(source_path);
    // let lib = PathBuf::from(library_path);

    let pool_lock = state.pool.lock().await;
    let pool = pool_lock.as_ref().ok_or("Not active library connected")?;

    perform_import_assets(window, src, pool.clone())
        .await
        .map_err(|e| format!("{:?}", e))
}
