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
//!   * `remote`    — probing and downloading a URL: every HTTP request Nova
//!     makes lives here, and nowhere else.
//!   * `rules`     — smart-folder rule trees compiled to SQL predicates.
//!   * `actions`   — `run_steps`, the mutation runner every write routes through,
//!     plus undo and the trash.
//!   * `commands`  — the Tauri command surface; thin wrappers, no logic.

mod actions;
mod assets;
mod bridge;
mod color;
mod commands;
mod db;
mod error;
mod extract;
mod fs;
mod library;
mod remote;
mod rules;
mod search;
mod tags;
mod thumbnail;

use std::sync::atomic::{AtomicBool, Ordering};

/// Set just before a DELIBERATE quit, so the exit guard below knows to let it
/// through. Everything else — closing the window in particular — is a request to
/// go to the background, not to die.
static QUITTING: AtomicBool = AtomicBool::new(false);

/// Open (or focus) the main window.
///
/// Built in code rather than declared in `tauri.conf.json`, because a declared
/// window is created unconditionally at startup — which is exactly what
/// `--tray` exists to avoid. No window means no WebView2 processes at all, and
/// those are essentially the whole of Nova's memory footprint.
fn open_main_window<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
        return;
    }

    let built = WebviewWindowBuilder::new(app, "main", WebviewUrl::default())
        .title("nova")
        .inner_size(1280.0, 832.0)
        .min_inner_size(800.0, 600.0)
        .decorations(false)
        .build();

    if let Err(e) = built {
        tracing::error!(error = %e, "Could not open the main window");
    }
}

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

    // `--tray` starts with no window: the background mode the browser extension
    // wakes into. Everything else about the process is identical.
    let tray_mode = std::env::args().any(|arg| arg == "--tray");
    let lease = bridge::Lease::new();

    tauri::Builder::default()
        // A second launch must never become a second instance: two processes on
        // one library means two write pools, two undo sessions and two thumbnail
        // pipelines. Instead the newcomer's argv is handed to the instance that
        // is already up — so `nova.exe` from the Start menu raises the window,
        // while `nova.exe --tray` from the launcher stub is a no-op because the
        // bridge it wanted is already listening.
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            if !argv.iter().any(|arg| arg == "--tray") {
                open_main_window(app);
            }
        }))
        .plugin(tauri_plugin_store::Builder::new().build())
        .manage(db::DbState::new())
        .manage(lease.clone())
        .plugin(tauri_plugin_dialog::init())
        // Looks unused — the capability grants no `fs:*` permission and nothing
        // calls `fs_scope()` — but do NOT remove it. `tauri-plugin-dialog`
        // extends the fs scope after a file pick, resolving managed state that
        // only this `init()` installs; dropping it panics the import folder
        // picker the moment it opens. No compiler or test would catch that.
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_drag::init())
        // `nova-remote://<asset id>` — how the webview reads an asset whose bytes
        // are not on disk. Registered as a protocol rather than pointing an
        // element straight at the origin URL, because only Rust can set the
        // Referer and User-Agent that many CDNs demand, and because the page's
        // CSP then never has to allow arbitrary remote origins.
        //
        // Asynchronous: the responder is handed to a task, so a slow origin
        // blocks nothing. See `remote::fetch_slice` for why one response is one
        // bounded chunk.
        .register_asynchronous_uri_scheme_protocol("nova-remote", |ctx, request, responder| {
            let app = ctx.app_handle().clone();
            let id = request.uri().path().trim_start_matches('/').to_string();
            let range = request
                .headers()
                .get("range")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);

            tauri::async_runtime::spawn(async move {
                responder.respond(commands::serve_remote_asset(&app, &id, range.as_deref()).await);
            });
        })
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
            commands::probe_url,
            commands::import_from_url,
            commands::add_remote_asset,
            commands::keep_remote_offline,
            commands::verify_remote_asset,
            commands::set_media_duration,
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
            commands::bridge_state,
            commands::bridge_approve,
            commands::bridge_deny,
            commands::bridge_revoke,
            commands::bridge_set_idle_exit,
        ])
        .setup(move |app| {
            use tauri::Manager;
            let handle = app.handle().clone();

            // The tray is the only affordance a windowless Nova has. Without it
            // a background process would be invisible and unkillable except
            // through Task Manager, which is not an acceptable thing to ship.
            build_tray(&handle)?;

            if tray_mode {
                tracing::info!("Started in tray mode; no window");
                // Nothing else will do it. Connecting the library is normally
                // driven by the WEBVIEW (`settings.svelte.ts` calls
                // `connect_library` once its store has hydrated) — and in tray
                // mode there is no webview, so a Nova woken by the extension
                // would sit there with no library and queue every capture.
                let reconnect = handle.clone();
                tauri::async_runtime::spawn(async move {
                    reconnect_last_library(&reconnect).await;
                });
            } else {
                open_main_window(&handle);
            }

            // Point the browsers at the connector sitting beside us. Done here,
            // on every start, because Nova is the only thing that knows where it
            // actually is — an installer-time registration is wrong the moment
            // the folder moves. Idempotent and best-effort; see `bridge::host`.
            if let Ok(config_dir) = handle.path().app_config_dir() {
                bridge::host::ensure_registered(&config_dir);
            }

            // The bridge outlives individual windows, so it is owned by the app,
            // not by the UI.
            let bridge_handle = handle.clone();
            let bridge_lease = lease.clone();
            tauri::async_runtime::spawn(async move {
                let state = match bridge::BridgeState::new(bridge_handle.clone(), bridge_lease).await
                {
                    Ok(state) => state,
                    Err(e) => {
                        tracing::error!(error = %e, "Extension bridge could not start");
                        return;
                    }
                };
                bridge_handle.manage(state.clone());

                // Serve, drain, and either exit or come back. `serve_once`
                // returns false when work arrived in the gap between the idle
                // countdown finishing and the listener actually closing — the
                // abort-the-shutdown path, where the right move is to re-bind
                // and carry on rather than die with something outstanding.
                loop {
                    match bridge::serve_once(state.clone()).await {
                        Ok(true) => {
                            QUITTING.store(true, Ordering::SeqCst);
                            bridge_handle.exit(0);
                            return;
                        }
                        Ok(false) => continue,
                        Err(e) => {
                            tracing::error!(error = %e, "Extension bridge stopped");
                            return;
                        }
                    }
                }
            });

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(move |app, event| match event {
            // Closing the window releases the lease rather than ending the
            // process: Nova drops back to the background, the WebView2
            // processes go with the window, and the idle countdown starts. That
            // is what makes "close it and forget it" cost nothing.
            tauri::RunEvent::WindowEvent {
                event: tauri::WindowEvent::Destroyed,
                ..
            } => {
                use tauri::Manager;
                if let Some(lease) = app.try_state::<bridge::Lease>() {
                    lease.set_windowed(false);
                }
            }
            tauri::RunEvent::WindowEvent {
                event: tauri::WindowEvent::Focused(_),
                ..
            } => {
                use tauri::Manager;
                if let Some(lease) = app.try_state::<bridge::Lease>() {
                    lease.set_windowed(true);
                }
            }
            // Tauri wants to exit whenever the last window goes. Refuse, unless
            // the quit was deliberate — the tray's Quit item, or the bridge
            // deciding it has been idle long enough.
            tauri::RunEvent::ExitRequested { api, .. } => {
                if !QUITTING.load(Ordering::SeqCst) {
                    api.prevent_exit();
                }
            }
            _ => {}
        });
}

/// Open the library the user last had open, reading the same `settings.json`
/// the frontend writes.
///
/// Only used in tray mode. The frontend deliberately keeps `activeLibrary` null
/// until `connect_library` resolves — consumers treat it as "library ready" —
/// but here there is no frontend to do any of that, so this reads the stored
/// path directly and connects.
///
/// A failure is not fatal and not surfaced: the library may have been moved or
/// deleted, in which case `/health` reports `library: false`, the extension says
/// "no library open", and captures queue. Which is the correct outcome — better
/// than a background process popping up a dialog nobody asked for.
async fn reconnect_last_library<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    use tauri::Manager;
    use tauri_plugin_store::StoreExt;

    let Ok(store) = app.store("settings.json") else {
        tracing::debug!("No settings store; nothing to reconnect");
        return;
    };

    // Shape written by `LibraryManager`: { activeLibrary, history }.
    let path = store
        .get("library")
        .and_then(|value| value.get("activeLibrary").cloned())
        .and_then(|value| value.as_str().map(str::to_owned));

    let Some(path) = path.filter(|p| !p.is_empty()) else {
        tracing::info!("No previously-open library to reconnect");
        return;
    };

    match app.state::<db::DbState>().connect(&path).await {
        Ok(()) => tracing::info!(library = %path, "Reconnected the last library"),
        Err(e) => tracing::warn!(error = %e, library = %path, "Could not reconnect the last library"),
    }
}

/// Tray icon and menu. Two items, because there are exactly two things a
/// background process needs to offer: come back, or go away.
fn build_tray<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::TrayIconBuilder;

    let open = MenuItem::with_id(app, "open", "Open Nova", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;

    let mut tray = TrayIconBuilder::with_id("nova")
        .menu(&menu)
        .tooltip("Nova")
        // Left-click is the shortest path back to the window, and is what people
        // try first regardless of what the menu says.
        .on_tray_icon_event(|tray, event| {
            if let tauri::tray::TrayIconEvent::Click {
                button: tauri::tray::MouseButton::Left,
                button_state: tauri::tray::MouseButtonState::Up,
                ..
            } = event
            {
                open_main_window(tray.app_handle());
            }
        })
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => open_main_window(app),
            "quit" => {
                QUITTING.store(true, Ordering::SeqCst);
                app.exit(0);
            }
            _ => {}
        });

    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}
