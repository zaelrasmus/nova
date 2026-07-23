mod assets;
mod commands;
mod db;
mod error;
mod extract;
mod fs;
mod library;
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
        .invoke_handler(tauri::generate_handler![
            commands::create_library,
            commands::import_assets,
            commands::connect_library,
            commands::stream_manifest,
            commands::fetch_folders,
            commands::create_folder,
            commands::rename_folder,
            commands::delete_folder,
            commands::move_folder,
            commands::add_assets_to_folder,
            commands::remove_assets_from_folder,
            commands::fetch_assets_by_ids,
            commands::generate_thumbnails_for_ids,
            commands::rebuild_thumbnails,
            commands::fetch_sort,
            commands::set_sort,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
