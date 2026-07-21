use crate::assets::{
    self, AssetLightRow, AssetMetadata, ImportProgress, ImportResult, ProgressReporter,
};
use crate::db::DbState;
use crate::error::AppError;
use crate::library::{self, LibraryInfo};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Runtime};
use tauri_plugin_fs::FsExt;
use tracing::{info, instrument, warn};

struct TauriProgressReporter {
    window: tauri::Window,
    last_emit: std::sync::Mutex<std::time::Instant>,
}

impl ProgressReporter for TauriProgressReporter {
    fn report(&self, progress: ImportProgress) {
        let is_high_frequency = matches!(progress.stage, assets::ImportStage::CopyingFiles);
        let stage_finished = progress.total > 0 && progress.current == progress.total;

        if is_high_frequency && !stage_finished {
            let mut last = self.last_emit.lock().unwrap();
            if last.elapsed().as_millis() < 16 {
                return; // drop this intermediate frame
            }
            *last = std::time::Instant::now();
            // guard drops here - we never hold the mutex across emit()
        }

        if let Err(e) = self.window.emit("import-progress", &progress) {
            // Non-fatal: the window may have been closed mid-import
            warn!(error = %e, "Failed to emit import-progress event");
        }

        // let mut last = self.last_emit.lock().unwrap();
        // let stage_finished = progress.current == progress.total && progress.total > 0;
        // let throttle_passed = last.elapsed().as_millis() >= 16;

        // if throttle_passed || stage_finished {
        //     if let Err(e) = self.window.emit("import-progress", &progress) {
        //         // Non-fatal: the window may have been closed mid-import.
        //         warn!(error = %e, "Failed to emit import-progress event");
        //     }
        //     *last = std::time::Instant::now();
        // }
    }
}

#[instrument(skip_all, fields(library_path = %library_path))]
#[tauri::command]
pub async fn connect_library<R: Runtime>(
    app: AppHandle<R>,
    library_path: String,
    state: tauri::State<'_, DbState>,
) -> Result<String, AppError> {
    state
        .connect(&library_path)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "connect_library failed"))?;

    app.fs_scope().allow_directory(&library_path, true).map_err(|e| {
        tracing::error!(error = %e, path = %library_path, "Failed to allow directory on connect");
        AppError::Io(std::io::Error::new(std::io::ErrorKind::PermissionDenied, e.to_string(),))
    })?;

    info!(library_path = %library_path, "Library connected");
    Ok("Library connected successfully".into())
}

#[instrument(skip_all, fields(asset_name = %name))]
#[tauri::command]
pub async fn inject_test_asset(
    name: String,
    state: tauri::State<'_, DbState>,
) -> Result<String, AppError> {
    let pool = state.acquire_pool().await?;

    assets::insert_test_asset(&pool, &name)
        .await
        .inspect_err(
            |e| tracing::error!(error = %e, asset_name = %name, "inject_test_asset failed"),
        )
        .map_err(AppError::from)
}

#[instrument(skip_all)]
#[tauri::command]
pub async fn fetch_assets(
    state: tauri::State<'_, DbState>,
) -> Result<Vec<AssetMetadata>, AppError> {
    let pool = state.acquire_pool().await?;

    assets::fetch_assets(&pool)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "fetch_assets failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all, fields(library_name = %name, location = %location))]
#[tauri::command]
pub async fn create_library<R: Runtime>(
    app: AppHandle<R>,
    location: String,
    name: String,
) -> Result<LibraryInfo, AppError> {
    let library_root = library::create_library(&location, &name)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "create_library failed"))?;

    app.fs_scope()
        .allow_directory(&library_root, true)
        .map_err(|e| {
            tracing::error!(error = %e, path = ?library_root, "Failed to grant fs scope");
            AppError::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                e.to_string(),
            ))
        })?;

    info!(root = ?library_root, "Library created successfully");

    Ok(LibraryInfo {
        db_path: library_root.join("library.db"),
        root_path: library_root,
    })
}

#[instrument(skip_all, fields(source_path = %source_path))]
#[tauri::command]
pub async fn import_assets(
    window: tauri::Window,
    source_path: String,
    state: tauri::State<'_, DbState>,
) -> Result<ImportResult, AppError> {
    let handle = state.acquire().await?;
    let source_dir = std::path::PathBuf::from(&source_path);

    let reporter = Arc::new(TauriProgressReporter {
        window,
        last_emit: std::sync::Mutex::new(std::time::Instant::now()),
    });

    assets::import_assets(reporter, source_dir, handle.pool, handle.root)
        .await
        .inspect_err(|e| tracing::error!(error = %e, source = %source_path, "import_assets failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all)]
#[tauri::command]
pub async fn stream_manifest(
    on_chunk: tauri::ipc::Channel<Vec<AssetLightRow>>,
    state: tauri::State<'_, DbState>,
) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    let rows = assets::fetch_manifest(&pool).await?;

    // Chunk so first paint starts before the whole manifest is deserialized.
    for chunk in rows.chunks(5000) {
        on_chunk
            .send(chunk.to_vec())
            .map_err(|e| AppError::Internal(e.into()))?;
    }
    Ok(())
}

#[instrument(skip_all, fields(count = ids.len()))]
#[tauri::command]
pub async fn fetch_assets_by_ids(
    ids: Vec<String>,
    state: tauri::State<'_, DbState>,
) -> Result<Vec<AssetMetadata>, AppError> {
    let handle = state.acquire().await?;
    assets::fetch_assets_by_ids(&handle.pool, &handle.root, &ids)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "fetch_assets_by_ids failed"))
        .map_err(AppError::from)
}

// ANTICIPATED: Backend sync
// Apply a preference change to the backend
#[tauri::command]
pub async fn apply_preference(key: String, value: serde_json::Value) -> Result<(), AppError> {
    match key.as_str() {
        // "max_import_size_mb" => { ... }
        // "thumbnail_quality"  => { ... }
        unknown => {
            tracing::warn!(key = unknown, "Unknown preference key, ignoring");
        }
    }
    Ok(())
}
