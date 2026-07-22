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
    import_folders: bool,
    state: tauri::State<'_, DbState>,
) -> Result<ImportResult, AppError> {
    let handle = state.acquire().await?;
    let source_dir = std::path::PathBuf::from(&source_path);

    let reporter = Arc::new(TauriProgressReporter {
        window,
        last_emit: std::sync::Mutex::new(std::time::Instant::now()),
    });

    assets::import_assets(
        reporter,
        source_dir,
        handle.pool,
        handle.root,
        import_folders,
    )
    .await
    .inspect_err(|e| tracing::error!(error = %e, source = %source_path, "import_assets failed"))
    .map_err(AppError::from)
}

/// Progress emitter for background thumbnail generation. Emits once per chunk
/// (already coarse — a chunk is 128 images), carrying the just-completed
/// `(id, thumb_hash)` pairs so the UI patches those rows in place.
struct ThumbProgressEmitter {
    window: tauri::Window,
}

impl assets::ThumbProgress for ThumbProgressEmitter {
    fn report(&self, done: usize, total: usize, ready: &[assets::ThumbReady]) {
        if let Err(e) = self.window.emit(
            "thumbnail-progress",
            serde_json::json!({ "current": done, "total": total, "ready": ready }),
        ) {
            warn!(error = %e, "Failed to emit thumbnail-progress event");
        }
    }
}

/// Generate thumbnails for all images still missing one (freshly imported, or
/// interrupted on a previous run). Safe to call on import completion and on
/// library open — a run already in flight makes this a no-op.
#[instrument(skip_all)]
#[tauri::command]
pub async fn generate_thumbnails(
    window: tauri::Window,
    thumb_mode: String,
    quality: f32,
    state: tauri::State<'_, DbState>,
) -> Result<usize, AppError> {
    let handle = state.acquire().await?;

    // Concurrency guard: if a run is already active, skip silently.
    let _guard = match state.thumb_gen.try_lock() {
        Ok(g) => g,
        Err(_) => {
            info!("Thumbnail generation already running; ignoring duplicate request");
            return Ok(0);
        }
    };

    let config = crate::thumbnail::ThumbConfig::from_setting(&thumb_mode, quality);
    let reporter = Arc::new(ThumbProgressEmitter { window });

    assets::generate_pending_thumbnails(&handle.pool, &handle.root, config, reporter)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "generate_thumbnails failed"))
        .map_err(AppError::from)
}

/// Rebuild ALL thumbnails from scratch: clear the cache (files + DB columns),
/// then regenerate with `thumb_mode`. Used to apply a changed quality setting to
/// an existing library. Guarded like `generate_thumbnails` — if a run is already
/// active this is a no-op. Resolves once every thumbnail has been regenerated.
#[instrument(skip_all)]
#[tauri::command]
pub async fn rebuild_thumbnails(
    window: tauri::Window,
    thumb_mode: String,
    quality: f32,
    state: tauri::State<'_, DbState>,
) -> Result<usize, AppError> {
    let handle = state.acquire().await?;

    let _guard = match state.thumb_gen.try_lock() {
        Ok(g) => g,
        Err(_) => {
            info!("Thumbnail generation already running; ignoring rebuild request");
            return Ok(0);
        }
    };

    assets::reset_thumbnails(&handle.pool, &handle.root)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "reset_thumbnails failed"))?;

    let config = crate::thumbnail::ThumbConfig::from_setting(&thumb_mode, quality);
    let reporter = Arc::new(ThumbProgressEmitter { window });

    assets::generate_pending_thumbnails(&handle.pool, &handle.root, config, reporter)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "rebuild_thumbnails failed"))
        .map_err(AppError::from)
}

/// Generate thumbnails only for the given asset ids that are still missing one —
/// the on-view (lazy) path. Called per visible window as the user scrolls, so it
/// runs unlocked (the frontend de-dupes in-flight ids); ids already generated are
/// filtered out by the query, making repeated calls cheap and idempotent.
#[instrument(skip_all, fields(requested = ids.len()))]
#[tauri::command]
pub async fn generate_thumbnails_for_ids(
    window: tauri::Window,
    ids: Vec<String>,
    thumb_mode: String,
    quality: f32,
    state: tauri::State<'_, DbState>,
) -> Result<usize, AppError> {
    let handle = state.acquire().await?;
    let config = crate::thumbnail::ThumbConfig::from_setting(&thumb_mode, quality);
    let reporter = Arc::new(ThumbProgressEmitter { window });

    assets::generate_thumbnails_for_ids(&handle.pool, &handle.root, config, &ids, reporter)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "generate_thumbnails_for_ids failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all)]
#[tauri::command]
pub async fn stream_manifest(
    filter: assets::ManifestFilter,
    on_chunk: tauri::ipc::Channel<Vec<AssetLightRow>>,
    state: tauri::State<'_, DbState>,
) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    let rows = assets::fetch_manifest(&pool, &filter).await?;

    // Chunk so first paint starts before the whole manifest is deserialized.
    for chunk in rows.chunks(5000) {
        on_chunk
            .send(chunk.to_vec())
            .map_err(|e| AppError::Internal(e.into()))?;
    }
    Ok(())
}

#[instrument(skip_all)]
#[tauri::command]
pub async fn fetch_folders(
    state: tauri::State<'_, DbState>,
) -> Result<Vec<assets::Folder>, AppError> {
    let pool = state.acquire_pool().await?;
    assets::fetch_folders(&pool)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "fetch_folders failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all, fields(name = %name))]
#[tauri::command]
pub async fn create_folder(
    name: String,
    parent_id: Option<String>,
    state: tauri::State<'_, DbState>,
) -> Result<assets::Folder, AppError> {
    let pool = state.acquire_pool().await?;
    assets::create_folder(&pool, &name, parent_id.as_deref())
        .await
        .inspect_err(|e| tracing::error!(error = %e, "create_folder failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all)]
#[tauri::command]
pub async fn rename_folder(
    id: String,
    name: String,
    state: tauri::State<'_, DbState>,
) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    assets::rename_folder(&pool, &id, &name)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "rename_folder failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all)]
#[tauri::command]
pub async fn delete_folder(
    id: String,
    state: tauri::State<'_, DbState>,
) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    assets::delete_folder(&pool, &id)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "delete_folder failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all)]
#[tauri::command]
pub async fn move_folder(
    id: String,
    new_parent_id: Option<String>,
    state: tauri::State<'_, DbState>,
) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    assets::move_folder(&pool, &id, new_parent_id.as_deref())
        .await
        .inspect_err(|e| tracing::error!(error = %e, "move_folder failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all, fields(count = asset_ids.len()))]
#[tauri::command]
pub async fn add_assets_to_folder(
    folder_id: String,
    asset_ids: Vec<String>,
    state: tauri::State<'_, DbState>,
) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    assets::add_assets_to_folder(&pool, &folder_id, &asset_ids)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "add_assets_to_folder failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all, fields(count = asset_ids.len()))]
#[tauri::command]
pub async fn remove_assets_from_folder(
    folder_id: String,
    asset_ids: Vec<String>,
    state: tauri::State<'_, DbState>,
) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    assets::remove_assets_from_folder(&pool, &folder_id, &asset_ids)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "remove_assets_from_folder failed"))
        .map_err(AppError::from)
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
