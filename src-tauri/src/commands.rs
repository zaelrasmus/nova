use std::path::Path;
use tauri::AppHandle;
use tauri_plugin_fs::FsExt;

#[tauri::command]
pub async fn create_library(app: AppHandle, library_path: String) -> Result<String, String> {
    let root = Path::new(&library_path);
    if root.exists() {
        return Err("Library path already exists".into());
    }
    tokio::fs::create_dir_all(root)
        .await
        .map_err(|e| format!("create dir failed: {e}"))?;
    tokio::fs::create_dir_all(root.join("assets"))
        .await
        .map_err(|e| format!("create assets dir failed: {e}"))?;

    app.fs_scope()
        .allow_directory(root, true)
        .map_err(|e| format!("scope allow failed: {e}"))?;

    Ok(root.join("library.db").to_string_lossy().replace('\\', "/"))
}
