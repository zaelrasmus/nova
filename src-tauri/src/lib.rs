mod assets;
mod commands;
mod db;
mod error;
mod fs;
mod library;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Tracing filter priority:
    //   1. `RUST_LOG` environment variable — overrides everything (useful in CI).
    //   2. Fallback: this crate at DEBUG, noisy dependencies silenced to WARN.
    //
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
            commands::inject_test_asset,
            commands::fetch_assets,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
