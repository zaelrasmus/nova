//! Nova — media asset manager built for 100k+ asset libraries.
//!
//! A library is a `<name>.library` folder holding `library.db` (SQLite/WAL),
//! `assets/` (the actual bytes, named `{uuid}.{ext}`) and `thumbnails/`.
//!
//! Module map, roughly in dependency order:
//!   * `error`     — `AppError`, the single error type crossing the IPC boundary.
//!   * `db`        — connection lifecycle + the open-library handle.
//!   * `library`   — creating a new `.library` folder on disk.
//!   * `fs` / `extract` / `color` / `thumbnail` — file, metadata, palette and
//!     thumbnail primitives. No knowledge of the app's concepts.
//!   * `assets`    — the big one: import, and `build_manifest_query`, the single
//!     place a scope+filters+sort becomes SQL. Also folders, pins, ordering.
//!   * `tags`      — tags and tag groups.
//!   * `search`    — the denormalised FTS5 `search_index` and its sync primitive.
//!   * `rules`     — smart-folder rule trees compiled to SQL predicates.
//!   * `actions`   — `run_steps`, the mutation runner every write routes through,
//!     plus undo and the trash.
//!   * `commands`  — the Tauri command surface; thin wrappers, no logic.

mod actions;
mod assets;
mod color;
mod commands;
mod db;
mod error;
mod extract;
mod fs;
mod library;
mod rules;
mod search;
mod tags;
mod thumbnail;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // TODO: Write structured logs to a rotating file to disk for crash reports.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nova=debug,sqlx=warn,tauri=warn,tao=warn".into()),
        )
        .with_target(false)
        .compact()
        .init();

    tracing::info!("Starting Nova");

    // Undo is bounded to this session — see `actions::SESSION_START`.
    actions::begin_session();

    tauri::Builder::default()
        .plugin(tauri_plugin_store::Builder::new().build())
        .manage(db::DbState::new())
        .plugin(tauri_plugin_dialog::init())
        // Looks unused — the capability grants no `fs:*` permission and nothing
        // calls `fs_scope()` — but do NOT remove it. `tauri-plugin-dialog`
        // extends the fs scope after a file pick, resolving managed state that
        // only this `init()` installs; dropping it panics the import folder
        // picker the moment it opens. No compiler or test would catch that.
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_drag::init())
        .invoke_handler(tauri::generate_handler![
            commands::create_library,
            commands::import_assets,
            commands::import_dropped_paths,
            commands::start_asset_drag,
            commands::clear_drag_staging,
            commands::rebuild_search_index,
            commands::search_index_degraded,
            commands::connect_library,
            commands::stream_manifest,
            commands::fetch_folders,
            commands::fetch_smart_folders,
            commands::create_smart_folder,
            commands::update_smart_folder,
            commands::delete_smart_folder,
            commands::count_matching,
            commands::fetch_smart_folder_groups,
            commands::create_smart_folder_group,
            commands::rename_smart_folder_group,
            commands::delete_smart_folder_group,
            commands::set_smart_folder_group,
            commands::create_folder,
            commands::update_folder,
            commands::folder_stats,
            commands::selection_summary,
            commands::folder_membership,
            commands::update_asset,
            commands::delete_folders,
            commands::move_folder,
            commands::reorder_folder,
            commands::fetch_pins,
            commands::set_pinned,
            commands::set_pin_color,
            commands::reorder_pin,
            commands::preview_matches,
            commands::reorder_assets,
            commands::fetch_tags,
            commands::ensure_tag,
            commands::rename_tag,
            commands::delete_tag,
            commands::tag_usage_for_assets,
            commands::set_tag_color,
            commands::set_tag_starred,
            commands::set_tag_group,
            commands::merge_tags,
            commands::fetch_tag_groups,
            commands::create_tag_group,
            commands::rename_tag_group,
            commands::set_tag_group_color,
            commands::delete_tag_group,
            commands::fetch_assets_by_ids,
            commands::generate_thumbnails_for_ids,
            commands::store_media_thumbnail,
            commands::rebuild_thumbnails,
            commands::fetch_sort,
            commands::set_sort,
            commands::color_coverage,
            commands::fetch_palette,
            commands::analyze_colors,
            commands::fetch_saved_filters,
            commands::create_saved_filter,
            commands::rename_saved_filter,
            commands::update_saved_filter,
            commands::delete_saved_filter,
            commands::fetch_quick_actions,
            commands::create_quick_action,
            commands::update_quick_action,
            commands::delete_quick_action,
            commands::preview_action_run,
            commands::run_quick_action,
            commands::undo_action_run,
            commands::fetch_action_runs,
            commands::preview_rename,
            commands::run_steps,
            commands::undo_latest_run,
            commands::fetch_folder_auto_tags,
            commands::set_folder_auto_tags,
            commands::apply_folder_auto_tags,
            commands::trash_count,
            commands::set_assets_trashed,
            commands::purge_assets,
            commands::empty_trash,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
