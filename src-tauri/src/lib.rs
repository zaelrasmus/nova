use crate::commands::DbState;

mod commands;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(DbState::new())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            commands::create_library,
            commands::import_assets,
            commands::connect_library,
            commands::inject_test_asset,
            commands::fetch_assets
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
