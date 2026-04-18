use crate::assets::{self, AssetMetadata, ImportProgress, ImportResult, ProgressReporter};
use crate::db::DbState;
use crate::library::{perform_create_library, LibraryInfo};
use anyhow::Result;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Runtime};
use tauri_plugin_fs::FsExt;

struct TauriProgressReporter {
    window: tauri::Window,
    last_emit: std::sync::Mutex<std::time::Instant>,
}

impl ProgressReporter for TauriProgressReporter {
    fn report(&self, progress: ImportProgress) {
        // SENIOR TIP: Throttling para 10,000 imágenes
        // Solo emitimos si ha pasado más de 16ms (60fps) o si es un cambio de etapa
        let mut last_time = self.last_emit.lock().unwrap();
        if last_time.elapsed().as_millis() > 16 || progress.current == progress.total {
            let _ = self.window.emit("import-progress", progress);
            *last_time = std::time::Instant::now();
        }
    }
}

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

    let reporter = Arc::new(TauriProgressReporter {
        window,
        last_emit: std::sync::Mutex::new(std::time::Instant::now()),
    });

    assets::perform_import_assets(reporter, src, pool)
        .await
        .map_err(|e| format!("{:?}", e))
}
