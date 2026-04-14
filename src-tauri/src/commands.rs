use serde::{Deserialize, Serialize};
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

    app.fs_scope()
        .allow_directory(&root, true)
        .map_err(|e| format!("scope allow failed: {e}"))?;

    let db_path = root.join("library.db");

    // TODO: Connect to the database and create

    Ok(LibraryInfo {
        db_path,
        root_path: root,
    })
}
