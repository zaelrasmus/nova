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

    tauri::Builder::default()
        .plugin(tauri_plugin_store::Builder::new().build())
        .manage(db::DbState::new())
        .plugin(tauri_plugin_dialog::init())
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
            commands::add_assets_to_folder,
            commands::move_assets_to_folder,
            commands::remove_assets_from_folder,
            commands::fetch_tags,
            commands::ensure_tag,
            commands::rename_tag,
            commands::delete_tag,
            commands::assign_tag,
            commands::unassign_tag,
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
