use crate::assets::{self, AssetMetadata, ImportResult};
use crate::db::DbState;
use crate::library::{perform_create_library, LibraryInfo};
use anyhow::Result;
use tauri::{AppHandle, Runtime};
use tauri_plugin_fs::FsExt;

#[tauri::command]
pub async fn connect_library(
    library_path: String,
    state: tauri::State<'_, DbState>,
) -> Result<String, String> {
    state.connect(library_path).await?;
    Ok("Library connected successfully".into())
}

#[tauri::command]
pub async fn inject_test_asset(
    name: String,
    state: tauri::State<'_, DbState>,
) -> Result<String, String> {
    let pool = state.get_pool().await?;
    assets::add_test_asset(&pool, &name)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn fetch_assets(state: tauri::State<'_, DbState>) -> Result<Vec<AssetMetadata>, String> {
    let pool = state.get_pool().await?;
    assets::list_assets(&pool).await.map_err(|e| e.to_string())
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

#[tauri::command]
pub async fn import_assets(
    window: tauri::Window,
    source_path: String,
    state: tauri::State<'_, DbState>,
) -> Result<ImportResult, String> {
    let pool = state.get_pool().await?;
    let src = std::path::PathBuf::from(source_path);

    assets::perform_import_assets(window, src, pool.clone())
        .await
        .map_err(|e| format!("{:?}", e))
}
