use crate::assets::{self, AssetMetadata, ImportProgress, ImportResult, ProgressReporter};
use crate::db::DbState;
use crate::error::AppError;
use crate::library::{perform_create_library, LibraryInfo};
use anyhow::Result;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Runtime};
use tauri_plugin_fs::FsExt;

/// Bridges the generic `ProgressReporter` trait to Tauri's window event system.
///
/// Throttled to ~60fps (16ms) to avoid flooding the frontend event queue when
/// importing tens of thousands of files.
struct TauriProgressReporter {
    window: tauri::Window,
    last_emit: std::sync::Mutex<std::time::Instant>,
}

impl ProgressReporter for TauriProgressReporter {
    fn report(&self, progress: ImportProgress) {
        let mut last = self.last_emit.lock().unwrap();
        let is_stage_complete = progress.current == progress.total && progress.total > 0;
        let throttle_passed = last.elapsed().as_millis() >= 16;

        if throttle_passed || is_stage_complete {
            if let Err(e) = self.window.emit("import-progress", &progress) {
                // Non-fatal: the window may have been closed mid-import.
                // warn!(error = %e, "Failed to emit import-progress event");
            }
            *last = std::time::Instant::now();
        }
    }
}

// #[instrument(skip_all, fields(library_path = %library_path))]
#[tauri::command]
pub async fn connect_library(
    library_path: String,
    state: tauri::State<'_, DbState>,
) -> Result<String, AppError> {
    // state
    //     .connect(&library_path)
    //     .await
    //     .inspect_err(|e| tracing::error!(error = %e, "connect_library failed"))?;

    state.connect(&library_path).await?;

    // info!(library_path = %library_path, "Library connected via command");
    Ok("Library connected successfully".into())
}

// #[instrument(skip_all, fields(asset_name = %name))]
#[tauri::command]
pub async fn inject_test_asset(
    name: String,
    state: tauri::State<'_, DbState>,
) -> Result<String, AppError> {
    let pool = state.get_pool().await?;
    // assets::add_test_asset(&pool, &name)
    //     .await
    //     .map_err(|e| e.to_string())
    // assets::add_test_asset(&pool, &name)
    //     .await
    //     .inspect_err(
    //         |e| tracing::error!(error = %e, asset_name = %name, "inject_test_asset failed"),
    //     )
    //     .map_err(AppError::from)

    assets::add_test_asset(&pool, &name)
        .await
        .map_err(AppError::from)
}

// #[instrument(skip_all)]
#[tauri::command]
pub async fn fetch_assets(
    state: tauri::State<'_, DbState>,
) -> Result<Vec<AssetMetadata>, AppError> {
    let pool = state.get_pool().await?;
    // list_assets(&pool).await.map_err(|e| e.to_string())
    // assets::list_assets(&pool)
    //     .await
    //     .inspect_err(|e| tracing::error!(error = %e, "fetch_assets failed"))
    //     .map_err(AppError::from)
    //

    assets::list_assets(&pool).await.map_err(AppError::from)
}

// #[instrument(skip_all, fields(library_name = %name, location = %location))]
#[tauri::command]
pub async fn create_library<R: Runtime>(
    app: AppHandle<R>,
    location: String,
    name: String,
) -> Result<LibraryInfo, AppError> {
    // let root = perform_create_library(&location, &name)
    //     .await
    //     .map_err(|e| format!("{:?}", e))?;

    // app.fs_scope()
    //     .allow_directory(&root, true)
    //     .map_err(|e| format!("Error allowing directory: {}", e))?;

    // Ok(LibraryInfo {
    //     db_path: root.join("library.db"),
    //     root_path: root,
    // })
    //

    // let root = library::perform_create_library(&location, &name)
    //     .await
    //     .inspect_err(|e| tracing::error!(error = %e, "create_library failed"))?;

    let root = perform_create_library(&location, &name).await?;

    app.fs_scope().allow_directory(&root, true).map_err(|e| {
        // tracing::error!(error = %e, path = ?root, "Failed to grant fs scope for library");
        AppError::Io(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            e.to_string(),
        ))
    })?;

    // info!(root = ?root, "Library created successfully");

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
) -> Result<ImportResult, AppError> {
    let pool = state.get_pool().await?;
    let src = std::path::PathBuf::from(source_path);

    let reporter = Arc::new(TauriProgressReporter {
        window,
        last_emit: std::sync::Mutex::new(std::time::Instant::now()),
    });

    // assets::perform_import_assets(reporter, src, pool)
    //     .await
    //     .map_err(|e| format!("{:?}", e))

    // assets::perform_import_assets(reporter, src, pool)
    //     .await
    //     .inspect_err(|e| tracing::error!(error = %e, source = %source_path, "import_assets failed"))
    //     .map_err(AppError::from)

    assets::perform_import_assets(reporter, src, pool)
        .await
        .map_err(AppError::from)
}
