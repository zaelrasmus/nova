use crate::assets::{
    self, AssetLightRow, AssetMetadata, ImportProgress, ImportResult, ProgressReporter,
};
use crate::db::DbState;
use crate::tags;
use crate::error::AppError;
use crate::library::{self, LibraryInfo};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, Runtime};
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

    // Scope the asset protocol to this library so the webview can load its
    // thumbnails/originals — the static scope is empty, granted per-library here.
    // (Additive for the session: switching libraries doesn't revoke prior grants.)
    app.asset_protocol_scope()
        .allow_directory(&library_path, true)
        .map_err(|e| {
            tracing::error!(error = %e, path = %library_path, "Failed to allow asset scope on connect");
            AppError::Io(std::io::Error::new(std::io::ErrorKind::PermissionDenied, e.to_string()))
        })?;

    info!(library_path = %library_path, "Library connected");
    Ok("Library connected successfully".into())
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

    // Scope the asset protocol to the new library (see connect_library).
    app.asset_protocol_scope()
        .allow_directory(&library_root, true)
        .map_err(|e| {
            tracing::error!(error = %e, path = ?library_root, "Failed to grant asset scope");
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
        assets::ImportRequest {
            sources: vec![source_dir],
            // The dialog imports into the library at large, and recreates the
            // picked folder's CONTENTS rather than the folder itself.
            target_folder: None,
            import_folders,
            include_roots: false,
        },
        handle.pool,
        handle.root,
    )
    .await
    .inspect_err(|e| tracing::error!(error = %e, source = %source_path, "import_assets failed"))
    .map_err(AppError::from)
}

/// Import files and folders dropped onto the window from the OS.
///
/// Separate from `import_assets` because a drop means something different: the
/// paths are whatever was grabbed (files, directories, or both), each dropped
/// directory becomes a folder in its own right, and the whole lot nests under
/// whichever folder row the cursor was over.
///
/// `paths` comes straight from Tauri's native drag-drop event, so these are real
/// OS paths the user chose by dragging — the same trust level as the file dialog.
#[instrument(skip_all, fields(paths = paths.len(), target = ?target_folder))]
#[tauri::command]
pub async fn import_dropped_paths(
    window: tauri::Window,
    paths: Vec<String>,
    target_folder: Option<String>,
    import_folders: bool,
    state: tauri::State<'_, DbState>,
) -> Result<assets::ImportResult, AppError> {
    let handle = state.acquire().await?;

    let reporter = Arc::new(TauriProgressReporter {
        window,
        last_emit: std::sync::Mutex::new(std::time::Instant::now()),
    });

    assets::import_assets(
        reporter,
        assets::ImportRequest {
            sources: paths.into_iter().map(std::path::PathBuf::from).collect(),
            target_folder,
            import_folders,
            include_roots: true,
        },
        handle.pool,
        handle.root,
    )
    .await
    .inspect_err(|e| tracing::error!(error = %e, "import_dropped_paths failed"))
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

/// Rebuild ALL thumbnails from scratch: clear the cache (files + DB columns),
/// then regenerate with `thumb_mode`. Used to apply a changed quality setting to
/// an existing library. Guarded by `thumb_gen` — if a run is already active this
/// is a no-op. Resolves once every thumbnail has been regenerated.
#[instrument(skip_all)]
#[tauri::command]
pub async fn rebuild_thumbnails(
    window: tauri::Window,
    settings: crate::thumbnail::ThumbSettings,
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

    let config = crate::thumbnail::ThumbConfig::from_settings(&settings);
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
    settings: crate::thumbnail::ThumbSettings,
    state: tauri::State<'_, DbState>,
) -> Result<usize, AppError> {
    let handle = state.acquire().await?;
    let config = crate::thumbnail::ThumbConfig::from_settings(&settings);
    let reporter = Arc::new(ThumbProgressEmitter { window });

    assets::generate_thumbnails_for_ids(&handle.pool, &handle.root, config, &ids, reporter)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "generate_thumbnails_for_ids failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all)]
#[tauri::command]
pub async fn stream_manifest(
    query: assets::ManifestQuery,
    on_chunk: tauri::ipc::Channel<Vec<AssetLightRow>>,
    state: tauri::State<'_, DbState>,
) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    let rows = assets::fetch_manifest(&pool, &query).await?;

    // Chunk so first paint starts before the whole manifest is deserialized.
    for chunk in rows.chunks(5000) {
        on_chunk
            .send(chunk.to_vec())
            .map_err(|e| AppError::Internal(e.into()))?;
    }
    Ok(())
}

/// The persisted sort for a scope. The frontend reads this when switching views
/// so the sort control always reflects what the query actually did, rather than
/// guessing and correcting (which flickers).
#[instrument(skip_all)]
#[tauri::command]
pub async fn fetch_sort(
    scope: assets::Scope,
    state: tauri::State<'_, DbState>,
) -> Result<assets::Sort, AppError> {
    let pool = state.acquire_pool().await?;
    assets::resolve_sort(&pool, &scope)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "fetch_sort failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all)]
#[tauri::command]
pub async fn set_sort(
    scope: assets::Scope,
    sort: assets::Sort,
    state: tauri::State<'_, DbState>,
) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    assets::set_sort(&pool, &scope, sort)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "set_sort failed"))
        .map_err(AppError::from)
}

// ── Color analysis ────────────────────────────────────────────────────────────

/// How many images have a color palette. Drives the "color data: N of M" notice,
/// so a color filter never quietly under-reports on an unanalyzed library.
#[instrument(skip_all)]
#[tauri::command]
pub async fn color_coverage(
    state: tauri::State<'_, DbState>,
) -> Result<assets::ColorCoverage, AppError> {
    let pool = state.acquire_pool().await?;
    assets::color_coverage(&pool)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "color_coverage failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all)]
#[tauri::command]
pub async fn fetch_palette(
    asset_id: String,
    state: tauri::State<'_, DbState>,
) -> Result<Vec<assets::PaletteSwatch>, AppError> {
    let pool = state.acquire_pool().await?;
    assets::fetch_palette(&pool, &asset_id)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "fetch_palette failed"))
        .map_err(AppError::from)
}

/// Backfill palettes for images that don't have one. Shares the thumbnail
/// generation lock: both passes decode images, and running them together would
/// just contend for the same cores.
#[instrument(skip_all)]
#[tauri::command]
pub async fn analyze_colors(
    window: tauri::Window,
    state: tauri::State<'_, DbState>,
) -> Result<usize, AppError> {
    let handle = state.acquire().await?;

    let _guard = match state.thumb_gen.try_lock() {
        Ok(g) => g,
        Err(_) => {
            info!("Generation already running; ignoring analyze request");
            return Ok(0);
        }
    };

    let reporter = Arc::new(ThumbProgressEmitter { window });
    assets::analyze_colors(&handle.pool, &handle.root, reporter)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "analyze_colors failed"))
        .map_err(AppError::from)
}

// ── Saved filters ─────────────────────────────────────────────────────────────

#[instrument(skip_all)]
#[tauri::command]
pub async fn fetch_saved_filters(
    state: tauri::State<'_, DbState>,
) -> Result<Vec<assets::SavedFilter>, AppError> {
    let pool = state.acquire_pool().await?;
    assets::fetch_saved_filters(&pool)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "fetch_saved_filters failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all, fields(name = %name))]
#[tauri::command]
pub async fn create_saved_filter(
    name: String,
    filters: assets::FilterSet,
    state: tauri::State<'_, DbState>,
) -> Result<assets::SavedFilter, AppError> {
    let pool = state.acquire_pool().await?;
    assets::create_saved_filter(&pool, &name, &filters)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "create_saved_filter failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all)]
#[tauri::command]
pub async fn rename_saved_filter(
    id: String,
    name: String,
    state: tauri::State<'_, DbState>,
) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    assets::rename_saved_filter(&pool, &id, &name)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "rename_saved_filter failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all)]
#[tauri::command]
pub async fn update_saved_filter(
    id: String,
    filters: assets::FilterSet,
    state: tauri::State<'_, DbState>,
) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    assets::update_saved_filter(&pool, &id, &filters)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "update_saved_filter failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all)]
#[tauri::command]
pub async fn delete_saved_filter(
    id: String,
    state: tauri::State<'_, DbState>,
) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    assets::delete_saved_filter(&pool, &id)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "delete_saved_filter failed"))
        .map_err(AppError::from)
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

#[instrument(skip_all, fields(count = asset_ids.len()))]
#[tauri::command]
pub async fn selection_summary(
    asset_ids: Vec<String>,
    state: tauri::State<'_, DbState>,
) -> Result<assets::SelectionSummary, AppError> {
    let pool = state.acquire_pool().await?;
    assets::selection_summary(&pool, &asset_ids)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "selection_summary failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all, fields(count = asset_ids.len()))]
#[tauri::command]
pub async fn folder_membership(
    asset_ids: Vec<String>,
    state: tauri::State<'_, DbState>,
) -> Result<Vec<assets::FolderMembership>, AppError> {
    let pool = state.acquire_pool().await?;
    assets::folder_membership(&pool, &asset_ids)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "folder_membership failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all)]
#[tauri::command]
pub async fn folder_stats(
    folder_id: String,
    state: tauri::State<'_, DbState>,
) -> Result<assets::FolderStats, AppError> {
    let pool = state.acquire_pool().await?;
    assets::folder_stats(&pool, &folder_id)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "folder_stats failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all)]
#[tauri::command]
pub async fn update_folder(
    id: String,
    patch: assets::FolderPatch,
    state: tauri::State<'_, DbState>,
) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    assets::update_folder(&pool, &id, patch)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "update_folder failed"))
        .map_err(AppError::from)
}

/// Returns the stored row so the frontend refreshes its cache from what the DB
/// actually holds — including the recomposed `filename`, which the client never
/// builds itself.
#[instrument(skip_all)]
#[tauri::command]
pub async fn update_asset(
    id: String,
    patch: assets::AssetPatch,
    state: tauri::State<'_, DbState>,
) -> Result<assets::AssetMetadata, AppError> {
    let handle = state.acquire().await?;
    assets::update_asset(&handle.pool, &handle.root, &id, patch)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "update_asset failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all)]
#[tauri::command]
pub async fn delete_folders(
    ids: Vec<String>,
    state: tauri::State<'_, DbState>,
) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    assets::delete_folders(&pool, &ids)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "delete_folders failed"))
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

// ── Tags ────────────────────────────────────────────────────────────────────

#[instrument(skip_all)]
#[tauri::command]
pub async fn fetch_tags(state: tauri::State<'_, DbState>) -> Result<Vec<tags::Tag>, AppError> {
    let pool = state.acquire_pool().await?;
    tags::fetch_tags(&pool)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "fetch_tags failed"))
        .map_err(AppError::from)
}

/// Find-or-create by name, returning the tag id. This is the create-on-the-fly
/// primitive the inspector calls before assigning.
#[instrument(skip_all)]
#[tauri::command]
pub async fn ensure_tag(name: String, state: tauri::State<'_, DbState>) -> Result<String, AppError> {
    let pool = state.acquire_pool().await?;
    tags::ensure_tag(&pool, &name)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "ensure_tag failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all)]
#[tauri::command]
pub async fn rename_tag(
    id: String,
    name: String,
    state: tauri::State<'_, DbState>,
) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    tags::rename_tag(&pool, &id, &name)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "rename_tag failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all)]
#[tauri::command]
pub async fn delete_tag(id: String, state: tauri::State<'_, DbState>) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    tags::delete_tag(&pool, &id)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "delete_tag failed"))
        .map_err(AppError::from)
}

/// Assign an existing tag to a set of assets. Idempotent per asset.
#[instrument(skip_all, fields(count = asset_ids.len()))]
#[tauri::command]
pub async fn assign_tag(
    tag_id: String,
    asset_ids: Vec<String>,
    state: tauri::State<'_, DbState>,
) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    tags::assign_tag(&pool, &tag_id, &asset_ids)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "assign_tag failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all, fields(count = asset_ids.len()))]
#[tauri::command]
pub async fn unassign_tag(
    tag_id: String,
    asset_ids: Vec<String>,
    state: tauri::State<'_, DbState>,
) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    tags::unassign_tag(&pool, &tag_id, &asset_ids)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "unassign_tag failed"))
        .map_err(AppError::from)
}

/// Per-tag counts across a selection — drives the inspector's tri-state.
#[instrument(skip_all, fields(count = asset_ids.len()))]
#[tauri::command]
pub async fn tag_usage_for_assets(
    asset_ids: Vec<String>,
    state: tauri::State<'_, DbState>,
) -> Result<Vec<tags::TagUsage>, AppError> {
    let pool = state.acquire_pool().await?;
    tags::tag_usage_for_assets(&pool, &asset_ids)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "tag_usage_for_assets failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all)]
#[tauri::command]
pub async fn set_tag_color(
    id: String,
    color: Option<String>,
    state: tauri::State<'_, DbState>,
) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    tags::set_tag_color(&pool, &id, color)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "set_tag_color failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all)]
#[tauri::command]
pub async fn set_tag_starred(
    id: String,
    starred: bool,
    state: tauri::State<'_, DbState>,
) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    tags::set_tag_starred(&pool, &id, starred)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "set_tag_starred failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all)]
#[tauri::command]
pub async fn set_tag_group(
    id: String,
    group_id: Option<String>,
    state: tauri::State<'_, DbState>,
) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    tags::set_tag_group(&pool, &id, group_id)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "set_tag_group failed"))
        .map_err(AppError::from)
}

/// Merge `source` into `target` (reassign then delete source). Irreversible.
#[instrument(skip_all)]
#[tauri::command]
pub async fn merge_tags(
    source: String,
    target: String,
    state: tauri::State<'_, DbState>,
) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    tags::merge_tags(&pool, &source, &target)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "merge_tags failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all)]
#[tauri::command]
pub async fn fetch_tag_groups(
    state: tauri::State<'_, DbState>,
) -> Result<Vec<tags::TagGroup>, AppError> {
    let pool = state.acquire_pool().await?;
    tags::fetch_tag_groups(&pool)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "fetch_tag_groups failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all)]
#[tauri::command]
pub async fn create_tag_group(
    name: String,
    state: tauri::State<'_, DbState>,
) -> Result<String, AppError> {
    let pool = state.acquire_pool().await?;
    tags::create_tag_group(&pool, &name)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "create_tag_group failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all)]
#[tauri::command]
pub async fn rename_tag_group(
    id: String,
    name: String,
    state: tauri::State<'_, DbState>,
) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    tags::rename_tag_group(&pool, &id, &name)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "rename_tag_group failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all)]
#[tauri::command]
pub async fn set_tag_group_color(
    id: String,
    color: Option<String>,
    state: tauri::State<'_, DbState>,
) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    tags::set_tag_group_color(&pool, &id, color)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "set_tag_group_color failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all)]
#[tauri::command]
pub async fn delete_tag_group(
    id: String,
    state: tauri::State<'_, DbState>,
) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    tags::delete_tag_group(&pool, &id)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "delete_tag_group failed"))
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

// ANTICIPATED: Backend sync. Intentional scaffolding for pushing preference
// changes from the frontend to the backend (pairs with BACKEND_SYNCED_KEYS /
// syncBackendPreferences in settings.svelte.ts). Kept on purpose though unused
// and not yet in the invoke_handler; wire it up when a preference first needs to
// affect Rust behavior.
// match kept (not a `let`) so future per-key arms have an obvious home.
#[allow(dead_code, clippy::match_single_binding)]
#[tauri::command]
pub async fn apply_preference(key: String, _value: serde_json::Value) -> Result<(), AppError> {
    match key.as_str() {
        // "max_import_size_mb" => { ... }
        // "thumbnail_quality"  => { ... }
        unknown => {
            tracing::warn!(key = unknown, "Unknown preference key, ignoring");
        }
    }
    Ok(())
}
