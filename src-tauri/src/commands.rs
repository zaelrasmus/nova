use serde::{Deserialize, Serialize};
use sqlx::{sqlite::SqliteConnectOptions, ConnectOptions, SqlitePool};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
use tauri::{AppHandle, Runtime};
use tauri_plugin_fs::FsExt;

#[derive(Debug, Serialize)]
pub struct LibraryInfo {
    db_path: PathBuf,
    root_path: PathBuf,
}

#[tauri::command]
pub async fn create_library<R: Runtime>(
    app: AppHandle<R>,
    location: String,
    name: String,
) -> Result<LibraryInfo, String> {
    let root = PathBuf::from(location).join(format!("{}.library", name));
    if root.exists() {
        return Err("Library path already exists".into());
    }
    tokio::fs::create_dir_all(&root)
        .await
        .map_err(|e| format!("create dir failed: {e}"))?;
    tokio::fs::create_dir_all(root.join("assets"))
        .await
        .map_err(|e| format!("create assets dir failed: {e}"))?;

    let db_path = root.join("library.db");

    // TODO: Connect to the database and create
    let db_url = format!("sqlite:{}", db_path.to_string_lossy());
    println!("db_url: {}", db_url);
    println!("db_path: {:?}", &db_path);

    let options = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .synchronous(sqlx::sqlite::SqliteSynchronous::Normal);

    let pool = SqlitePool::connect_with(options)
        .await
        .map_err(|e| format!("cannot open pool DB: {e}"))?;
    sqlx::migrate!()
        .run(&pool)
        .await
        .map_err(|e| format!("migration failed: {e}"))?;

    pool.close().await;

    app.fs_scope()
        .allow_directory(&root, true)
        .map_err(|e| format!("scope allow failed: {e}"))?;

    Ok(LibraryInfo {
        db_path,
        root_path: root,
    })
}
