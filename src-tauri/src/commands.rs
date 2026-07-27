use crate::actions;
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

    // Sweep any drag-staging links left by a crash mid-drag. Cheap, and the
    // per-drag cleanup already handles the normal case; this is the safety net.
    if let Ok(handle) = state.acquire().await {
        let _ = assets::clear_drag_staging(&handle.root);
    }

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

    // Exclusive: this is about to delete `thumbnails/` wholesale.
    let _guard = match state.thumb_gen.try_write() {
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
/// the on-view (lazy) path. Called per visible window as the user scrolls, so
/// these overlap freely with each other (the frontend de-dupes in-flight ids, and
/// the query filters out ids already generated, making repeated calls cheap and
/// idempotent).
///
/// Takes the SHARED side of `thumb_gen`, which costs these calls nothing against
/// one another and keeps them out of a rebuild's way — see the lock's comment in
/// `db.rs`. A rejected `try_read` means a rebuild holds the exclusive side and is
/// regenerating every one of these rows anyway, so skipping is the whole answer,
/// not a compromise.
#[instrument(skip_all, fields(requested = ids.len()))]
#[tauri::command]
pub async fn generate_thumbnails_for_ids(
    window: tauri::Window,
    ids: Vec<String>,
    settings: crate::thumbnail::ThumbSettings,
    state: tauri::State<'_, DbState>,
) -> Result<usize, AppError> {
    let handle = state.acquire().await?;

    let Ok(_guard) = state.thumb_gen.try_read() else {
        info!("Rebuild in progress; skipping this on-view thumbnail batch");
        return Ok(0);
    };

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

    // Claim a generation. Any later call bumps this, which is the signal to stop
    // — see `DbState::manifest_gen`. Taken before the first row so a request
    // superseded during setup never sends anything at all.
    let generation = state
        .manifest_gen
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        + 1;
    let manifest_gen = std::sync::Arc::clone(&state.manifest_gen);

    // 5,000 rows a batch: large enough that the per-message IPC overhead
    // disappears against the payload, small enough that the first one lands
    // while SQLite is still walking the index.
    let sent = assets::stream_manifest(&pool, &query, 5000, move |batch| {
        // Checked BETWEEN batches rather than per row: the frontend cannot
        // render faster than a batch anyway, and this keeps the hot loop free
        // of an atomic load per asset.
        if manifest_gen.load(std::sync::atomic::Ordering::SeqCst) != generation {
            return Ok(false);
        }
        // A send failure means the webview is gone (window closed mid-load).
        // Also a reason to stop, and not an error worth surfacing to a UI that
        // no longer exists.
        Ok(on_chunk.send(batch).is_ok())
    })
    .await?;

    tracing::debug!(sent, generation, "Manifest stream finished");
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

    let _guard = match state.thumb_gen.try_write() {
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

// ── Smart folders ─────────────────────────────────────────────────────────────

#[instrument(skip_all)]
#[tauri::command]
pub async fn fetch_smart_folders(
    state: tauri::State<'_, DbState>,
) -> Result<Vec<assets::SmartFolder>, AppError> {
    let pool = state.acquire_pool().await?;
    assets::fetch_smart_folders(&pool)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "fetch_smart_folders failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all, fields(name = %name))]
#[tauri::command]
pub async fn create_smart_folder(
    name: String,
    rules: crate::rules::RuleNode,
    state: tauri::State<'_, DbState>,
) -> Result<assets::SmartFolder, AppError> {
    let pool = state.acquire_pool().await?;
    assets::create_smart_folder(&pool, &name, &rules)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "create_smart_folder failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all)]
#[tauri::command]
pub async fn update_smart_folder(
    id: String,
    patch: assets::SmartFolderPatch,
    state: tauri::State<'_, DbState>,
) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    assets::update_smart_folder(&pool, &id, patch)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "update_smart_folder failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all)]
#[tauri::command]
pub async fn delete_smart_folder(
    id: String,
    state: tauri::State<'_, DbState>,
) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    assets::delete_smart_folder(&pool, &id)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "delete_smart_folder failed"))
        .map_err(AppError::from)
}

/// Live "Found N items" for the rule editor. Debounced by the caller — this runs
/// the real predicate, so it costs a real query.
#[instrument(skip_all)]
#[tauri::command]
pub async fn count_matching(
    rules: crate::rules::RuleNode,
    state: tauri::State<'_, DbState>,
) -> Result<i64, AppError> {
    let pool = state.acquire_pool().await?;
    assets::count_matching(&pool, &rules)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "count_matching failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all)]
#[tauri::command]
pub async fn fetch_smart_folder_groups(
    state: tauri::State<'_, DbState>,
) -> Result<Vec<assets::SmartFolderGroup>, AppError> {
    let pool = state.acquire_pool().await?;
    assets::fetch_smart_folder_groups(&pool)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "fetch_smart_folder_groups failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all, fields(name = %name))]
#[tauri::command]
pub async fn create_smart_folder_group(
    name: String,
    state: tauri::State<'_, DbState>,
) -> Result<assets::SmartFolderGroup, AppError> {
    let pool = state.acquire_pool().await?;
    assets::create_smart_folder_group(&pool, &name)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "create_smart_folder_group failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all)]
#[tauri::command]
pub async fn rename_smart_folder_group(
    id: String,
    name: String,
    state: tauri::State<'_, DbState>,
) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    assets::rename_smart_folder_group(&pool, &id, &name)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "rename_smart_folder_group failed"))
        .map_err(AppError::from)
}

/// Deleting a group UNGROUPS its members; it never deletes saved queries.
#[instrument(skip_all)]
#[tauri::command]
pub async fn delete_smart_folder_group(
    id: String,
    state: tauri::State<'_, DbState>,
) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    assets::delete_smart_folder_group(&pool, &id)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "delete_smart_folder_group failed"))
        .map_err(AppError::from)
}

/// Move a smart folder into a group; `groupId: null` ungroups it.
#[instrument(skip_all)]
#[tauri::command]
pub async fn set_smart_folder_group(
    id: String,
    group_id: Option<String>,
    state: tauri::State<'_, DbState>,
) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    assets::set_smart_folder_group(&pool, &id, group_id.as_deref())
        .await
        .inspect_err(|e| tracing::error!(error = %e, "set_smart_folder_group failed"))
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

/// Drop-between-rows: place a folder under `newParentId`, after `afterId`.
/// `afterId: null` means first among its new siblings.
#[instrument(skip_all)]
#[tauri::command]
pub async fn reorder_folder(
    id: String,
    new_parent_id: Option<String>,
    after_id: Option<String>,
    state: tauri::State<'_, DbState>,
) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    assets::reorder_folder(&pool, &id, new_parent_id.as_deref(), after_id.as_deref())
        .await
        .inspect_err(|e| tracing::error!(error = %e, "reorder_folder failed"))
        .map_err(AppError::from)
}

/// Drop a block of assets at a new spot in the current scope's manual order.
/// `afterId: null` drops at the head. Only meaningful when the scope is sorted
/// manually — the frontend enforces that before calling.
#[instrument(skip_all, fields(scope = ?scope, moved = moved_ids.len()))]
#[tauri::command]
pub async fn reorder_assets(
    scope: assets::Scope,
    moved_ids: Vec<String>,
    after_id: Option<String>,
    state: tauri::State<'_, DbState>,
) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    assets::reorder_assets(&pool, &scope, &moved_ids, after_id.as_deref())
        .await
        .inspect_err(|e| tracing::error!(error = %e, "reorder_assets failed"))
        .map_err(AppError::from)
}
// ── Pins ──────────────────────────────────────────────────────────────────────
//
// One list, two kinds. Every command takes a `kind` so the sidebar's shortlist
// can hold folders and smart folders in a single order the user arranges.

/// The pinned list, in order, across both kinds.
#[instrument(skip_all)]
#[tauri::command]
pub async fn fetch_pins(
    state: tauri::State<'_, DbState>,
) -> Result<Vec<assets::PinnedItem>, AppError> {
    let pool = state.acquire_pool().await?;
    assets::fetch_pins(&pool)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "fetch_pins failed"))
        .map_err(AppError::from)
}

/// Pin or unpin. Pinning appends to the end of the shared list.
#[instrument(skip_all)]
#[tauri::command]
pub async fn set_pinned(
    kind: assets::PinKind,
    id: String,
    pinned: bool,
    state: tauri::State<'_, DbState>,
) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    assets::set_pinned(&pool, kind, &id, pinned)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "set_pinned failed"))
        .map_err(AppError::from)
}

/// Set or clear a pin's accent. `color: null` clears it.
#[instrument(skip_all)]
#[tauri::command]
pub async fn set_pin_color(
    kind: assets::PinKind,
    id: String,
    color: Option<String>,
    state: tauri::State<'_, DbState>,
) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    assets::set_pin_color(&pool, kind, &id, color.as_deref())
        .await
        .inspect_err(|e| tracing::error!(error = %e, "set_pin_color failed"))
        .map_err(AppError::from)
}

/// Drag-to-reorder inside the pinned list. A null `afterId` means first.
#[instrument(skip_all)]
#[tauri::command]
pub async fn reorder_pin(
    kind: assets::PinKind,
    id: String,
    after_kind: Option<assets::PinKind>,
    after_id: Option<String>,
    state: tauri::State<'_, DbState>,
) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    assets::reorder_pin(&pool, kind, &id, after_kind, after_id.as_deref())
        .await
        .inspect_err(|e| tracing::error!(error = %e, "reorder_pin failed"))
        .map_err(AppError::from)
}

/// A few of a rule set's current matches, for the sidebar preview.
#[instrument(skip_all)]
#[tauri::command]
pub async fn preview_matches(
    rules: crate::rules::RuleNode,
    limit: i64,
    state: tauri::State<'_, DbState>,
) -> Result<Vec<AssetLightRow>, AppError> {
    let pool = state.acquire_pool().await?;
    assets::preview_matches(&pool, &rules, limit)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "preview_matches failed"))
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

/// Rebuild the full-text search index from scratch. The recovery path if the
/// derived index ever drifts from the source tables — the maintenance tool.
#[instrument(skip_all)]
#[tauri::command]
pub async fn rebuild_search_index(state: tauri::State<'_, DbState>) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    crate::search::rebuild_search_index(&pool)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "rebuild_search_index failed"))
        .map_err(AppError::from)
}

/// Stage the given assets for an outbound OS drag and return the absolute staged
/// paths. The frontend hands these to the drag plugin's `startDrag`.
///
/// Ids in, paths out: the webview never names a source location, so this can't be
/// turned into an arbitrary-file read. See `stage_assets_for_drag`.
#[instrument(skip_all, fields(count = asset_ids.len()))]
#[tauri::command]
pub async fn start_asset_drag(
    asset_ids: Vec<String>,
    state: tauri::State<'_, DbState>,
) -> Result<Vec<String>, AppError> {
    let handle = state.acquire().await?;
    assets::stage_assets_for_drag(&handle.pool, &handle.root, &asset_ids)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "start_asset_drag failed"))
        .map_err(AppError::from)
}

/// Remove staged drag links. Called after a drag ends. Non-fatal: staged links
/// are near-zero-byte and swept on next library open regardless.
#[instrument(skip_all)]
#[tauri::command]
pub async fn clear_drag_staging(state: tauri::State<'_, DbState>) -> Result<(), AppError> {
    let handle = state.acquire().await?;
    assets::clear_drag_staging(&handle.root)
        .inspect_err(|e| tracing::warn!(error = %e, "clear_drag_staging failed"))
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

// REMOVED: add_assets_to_folder, move_assets_to_folder, remove_assets_from_folder,
// assign_tag, unassign_tag.
//
// Unreferenced since membership and tagging were rerouted through `run_steps`,
// but still registered in `generate_handler!` — which made them live IPC surface
// the webview could call, doing the same work with NO undo record and no run
// history. A second way to do the same thing, minus the guarantees the first one
// exists to provide. The `_in` primitives they wrapped are still there and still
// used by the step engine.

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

// ── Quick actions ─────────────────────────────────────────────────────────────
//
// Note what these commands do NOT do: none of them derives the selection. The
// asset ids come from the frontend, snapshotted at the moment the user
// triggered the run. An action that changes what matches the current scope makes
// assets disappear from the grid while it runs, so re-reading the selection
// anywhere in here would apply the pipeline to a set the user never chose.

#[instrument(skip_all)]
#[tauri::command]
pub async fn fetch_quick_actions(
    state: tauri::State<'_, DbState>,
) -> Result<Vec<actions::QuickAction>, AppError> {
    let pool = state.acquire_pool().await?;
    actions::fetch_quick_actions(&pool)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "fetch_quick_actions failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all, fields(name = %draft.name))]
#[tauri::command]
pub async fn create_quick_action(
    draft: actions::QuickActionDraft,
    state: tauri::State<'_, DbState>,
) -> Result<actions::QuickAction, AppError> {
    let pool = state.acquire_pool().await?;
    actions::create_quick_action(&pool, draft)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "create_quick_action failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all)]
#[tauri::command]
pub async fn update_quick_action(
    id: String,
    draft: actions::QuickActionDraft,
    state: tauri::State<'_, DbState>,
) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    actions::update_quick_action(&pool, &id, draft)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "update_quick_action failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all)]
#[tauri::command]
pub async fn delete_quick_action(
    id: String,
    state: tauri::State<'_, DbState>,
) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    actions::delete_quick_action(&pool, &id)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "delete_quick_action failed"))
        .map_err(AppError::from)
}

/// The dry run behind the confirmation dialog. Read-only.
#[instrument(skip_all, fields(count = asset_ids.len()))]
#[tauri::command]
pub async fn preview_action_run(
    action_id: String,
    asset_ids: Vec<String>,
    state: tauri::State<'_, DbState>,
) -> Result<actions::RunPreview, AppError> {
    let pool = state.acquire_pool().await?;
    actions::preview_run(&pool, &action_id, &asset_ids)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "preview_action_run failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all, fields(count = asset_ids.len()))]
#[tauri::command]
pub async fn run_quick_action(
    action_id: String,
    asset_ids: Vec<String>,
    state: tauri::State<'_, DbState>,
) -> Result<actions::RunSummary, AppError> {
    let pool = state.acquire_pool().await?;
    actions::run_action(&pool, &action_id, &asset_ids)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "run_quick_action failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all)]
#[tauri::command]
pub async fn undo_action_run(
    run_id: String,
    state: tauri::State<'_, DbState>,
) -> Result<actions::UndoSummary, AppError> {
    let pool = state.acquire_pool().await?;
    actions::undo_run(&pool, &run_id)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "undo_action_run failed"))
        .map_err(AppError::from)
}

/// Run history, newest first. Read on connect so an undo offer survives a reload
/// rather than living only in the toast that announced it.
#[instrument(skip_all)]
#[tauri::command]
pub async fn fetch_action_runs(
    state: tauri::State<'_, DbState>,
) -> Result<Vec<actions::ActionRun>, AppError> {
    let pool = state.acquire_pool().await?;
    actions::fetch_recent_runs(&pool)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "fetch_action_runs failed"))
        .map_err(AppError::from)
}

/// Live rename preview for the pattern box. Read-only, and safe to call on
/// every keystroke — a bad pattern comes back as `error`, not as a failure.
#[instrument(skip_all, fields(count = asset_ids.len()))]
#[tauri::command]
pub async fn preview_rename(
    step: actions::Step,
    asset_ids: Vec<String>,
    limit: usize,
    state: tauri::State<'_, DbState>,
) -> Result<actions::RenamePreview, AppError> {
    let pool = state.acquire_pool().await?;
    actions::preview_rename(&pool, &step, &asset_ids, limit)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "preview_rename failed"))
        .map_err(AppError::from)
}

/// Run a pipeline that isn't a saved action.
///
/// This is how direct manipulation gets an inverse: dragging a selection into a
/// folder is `[AddToFolder]`, a bulk tag toggle is `[AddTags]`, and both go
/// through the same transaction-and-undo machinery a quick action does rather
/// than reimplementing it. Small edits still skip the history — see
/// `UNDO_MIN_ASSETS`.
#[instrument(skip_all, fields(name = %name, steps = steps.len(), count = asset_ids.len()))]
#[tauri::command]
pub async fn run_steps(
    name: String,
    steps: Vec<actions::Step>,
    asset_ids: Vec<String>,
    state: tauri::State<'_, DbState>,
) -> Result<actions::RunSummary, AppError> {
    let pool = state.acquire_pool().await?;
    actions::run_steps(
        &pool,
        actions::RunSource::Direct { name: &name },
        &steps,
        &asset_ids,
    )
    .await
    .inspect_err(|e| tracing::error!(error = %e, "run_steps failed"))
    .map_err(AppError::from)
}

/// Undo the most recent undoable run, whatever produced it. Backs Ctrl+Z.
///
/// `None` means there was nothing to undo — not an error, just an empty history,
/// which is the normal state after a fresh library open.
#[instrument(skip_all)]
#[tauri::command]
pub async fn undo_latest_run(
    state: tauri::State<'_, DbState>,
) -> Result<Option<actions::UndoSummary>, AppError> {
    let pool = state.acquire_pool().await?;
    let Some(run_id) = actions::latest_undoable_run(&pool)
        .await
        .map_err(AppError::from)?
    else {
        return Ok(None);
    };
    actions::undo_run(&pool, &run_id)
        .await
        .map(Some)
        .inspect_err(|e| tracing::error!(error = %e, "undo_latest_run failed"))
        .map_err(AppError::from)
}

// ── Folder auto-tags ──────────────────────────────────────────────────────────

#[instrument(skip_all)]
#[tauri::command]
pub async fn fetch_folder_auto_tags(
    folder_id: String,
    state: tauri::State<'_, DbState>,
) -> Result<Vec<String>, AppError> {
    let pool = state.acquire_pool().await?;
    assets::fetch_folder_auto_tags(&pool, &folder_id)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "fetch_folder_auto_tags failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all, fields(count = tag_ids.len()))]
#[tauri::command]
pub async fn set_folder_auto_tags(
    folder_id: String,
    tag_ids: Vec<String>,
    state: tauri::State<'_, DbState>,
) -> Result<(), AppError> {
    let pool = state.acquire_pool().await?;
    assets::set_folder_auto_tags(&pool, &folder_id, &tag_ids)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "set_folder_auto_tags failed"))
        .map_err(AppError::from)
}

/// Apply a folder's auto-tags to what's ALREADY in it.
///
/// The only retroactive path, and it's explicit on purpose: turning auto-tags on
/// must not silently rewrite thousands of assets that were filed before the rule
/// existed. Routed through the action pipeline so the backfill is undoable like
/// any other bulk change.
#[instrument(skip_all)]
#[tauri::command]
pub async fn apply_folder_auto_tags(
    folder_id: String,
    state: tauri::State<'_, DbState>,
) -> Result<actions::RunSummary, AppError> {
    let pool = state.acquire_pool().await?;
    let tag_ids = assets::fetch_folder_auto_tags(&pool, &folder_id)
        .await
        .map_err(AppError::from)?;
    let asset_ids = assets::folder_member_ids(&pool, &folder_id)
        .await
        .map_err(AppError::from)?;

    if tag_ids.is_empty() || asset_ids.is_empty() {
        return Ok(actions::RunSummary {
            run_id: None,
            name: "Auto-tag folder".into(),
            asset_count: 0,
            is_undoable: false,
        });
    }

    actions::run_steps(
        &pool,
        actions::RunSource::Direct {
            name: "Auto-tag folder",
        },
        &[actions::Step {
            op: actions::Op::AddTags { tag_ids },
            when: None,
        }],
        &asset_ids,
    )
    .await
    .inspect_err(|e| tracing::error!(error = %e, "apply_folder_auto_tags failed"))
    .map_err(AppError::from)
}

// ── Trash ─────────────────────────────────────────────────────────────────────

#[instrument(skip_all)]
#[tauri::command]
pub async fn trash_count(state: tauri::State<'_, DbState>) -> Result<i64, AppError> {
    let pool = state.acquire_pool().await?;
    assets::trash_count(&pool)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "trash_count failed"))
        .map_err(AppError::from)
}

/// Move to the Trash, and back. Both go through the action pipeline, so both are
/// undoable and both appear in the run history like any other bulk change.
#[instrument(skip_all, fields(count = asset_ids.len(), trashed))]
#[tauri::command]
pub async fn set_assets_trashed(
    asset_ids: Vec<String>,
    trashed: bool,
    state: tauri::State<'_, DbState>,
) -> Result<actions::RunSummary, AppError> {
    let pool = state.acquire_pool().await?;
    let op = if trashed {
        actions::Op::MoveToTrash
    } else {
        actions::Op::RestoreFromTrash
    };
    actions::run_steps(
        &pool,
        actions::RunSource::Direct {
            name: if trashed { "Move to Trash" } else { "Restore" },
        },
        &[actions::Step { op, when: None }],
        &asset_ids,
    )
    .await
    .inspect_err(|e| tracing::error!(error = %e, "set_assets_trashed failed"))
    .map_err(AppError::from)
}

/// Delete for good. Returns how many were actually removed — assets that were
/// not in the Trash are ignored rather than deleted, so a stale selection can
/// never destroy a live asset.
#[instrument(skip_all, fields(count = asset_ids.len()))]
#[tauri::command]
pub async fn purge_assets(
    asset_ids: Vec<String>,
    state: tauri::State<'_, DbState>,
) -> Result<usize, AppError> {
    let handle = state.acquire().await?;
    assets::purge_assets(&handle.pool, &handle.root, &asset_ids)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "purge_assets failed"))
        .map_err(AppError::from)
}

#[instrument(skip_all)]
#[tauri::command]
pub async fn empty_trash(state: tauri::State<'_, DbState>) -> Result<usize, AppError> {
    let handle = state.acquire().await?;
    assets::empty_trash(&handle.pool, &handle.root)
        .await
        .inspect_err(|e| tracing::error!(error = %e, "empty_trash failed"))
        .map_err(AppError::from)
}
