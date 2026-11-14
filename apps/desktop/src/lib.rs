//! Tauri v2 desktop bridge host for DDScopeHost.

pub mod commands;
pub mod data_bridge;
pub mod runtime;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use specta_typescript::Typescript;
use tauri::Manager;
use tauri_specta::{collect_commands, collect_events, Builder};

/// Builds the `tauri-specta` command and event router.
pub fn create_specta_builder() -> Builder<tauri::Wry> {
    Builder::<tauri::Wry>::new()
        .commands(collect_commands![
            commands::get_engine_info,
            commands::list_serial_ports,
            commands::create_source,
            commands::start_source,
            commands::stop_source,
            commands::remove_source,
            commands::seek_replay_source,
            commands::set_raw_recorder,
            commands::get_channel_tree,
            commands::get_source_configs,
            commands::upsert_scope_viewport,
            commands::remove_scope_viewport,
            commands::rearm_scope,
            commands::update_sync_group,
            commands::send_downlink,
            commands::attach_data_bridge,
            commands::detach_data_bridge,
            commands::get_data_bridge_stats,
            commands::set_data_bridge_rate,
            commands::load_default_project_config,
            commands::save_default_project_config,
            commands::clear_default_project_config,
        ])
        .events(collect_events![commands::WorkspaceTelemetryEvent])
}

/// Exports TypeScript bindings to `path`.
pub fn export_ts_bindings(path: &Path) -> Result<(), specta_typescript::Error> {
    let builder = create_specta_builder();
    let ts = Typescript::default().header("// @ts-nocheck\n");
    builder.export(ts, path)
}

/// Returns the default TypeScript bindings output path in `apps/web-ui`.
pub fn default_bindings_output_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../web-ui/src/bridge/bindings.ts")
}

/// Launches the Tauri v2 desktop application.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = create_specta_builder();

    #[cfg(debug_assertions)]
    {
        if let Err(err) = export_ts_bindings(&default_bindings_output_path()) {
            eprintln!("failed to export typescript bindings: {err}");
        }
    }

    let tokio_handle = tauri::async_runtime::block_on(async { tokio::runtime::Handle::current() });
    let auto_start_default = std::env::var("DDSCOPE_AUTO_START_DEFAULT_SOURCE")
        .map(|raw| {
            let trimmed = raw.trim();
            trimmed == "1" || trimmed.eq_ignore_ascii_case("true")
        })
        .unwrap_or(false);
    let bridge_state = runtime::build_live_desktop_state_with_auto_start(
        tokio_handle,
        runtime::DesktopRuntimeConfig::from_env(),
        auto_start_default,
    )
    .expect("failed to initialize DDScopeHost live CoreWorkspace");

    let setup_state = bridge_state.clone();
    tauri::Builder::default()
        .manage(bridge_state)
        .invoke_handler(builder.invoke_handler())
        .on_window_event(|window, event| {
            if matches!(event, tauri::WindowEvent::Destroyed) {
                if window.label() == "main" {
                    if let Some(ctrl) = window.try_state::<Arc<runtime::DesktopRuntimeController>>() {
                        ctrl.shutdown_blocking();
                    }
                }
                #[cfg(windows)]
                {
                    data_bridge::shm_windows::release_webview2_shm_on_sta(window.label());
                }
            }
        })
        .setup(move |app| {
            builder.mount_events(app);
            let rt_ctrl = runtime::DesktopRuntimeController::spawn(
                app.handle().clone(),
                setup_state.clone(),
            );
            app.manage(rt_ctrl.clone());

            if let Ok(raw_ms) = std::env::var("DDSCOPE_SMOKE_EXIT_MS") {
                if let Ok(ms) = raw_ms.parse::<u64>() {
                    if ms > 0 {
                        let handle = app.handle().clone();
                        let smoke_ctrl = Arc::clone(&rt_ctrl);
                        std::thread::spawn(move || {
                            std::thread::sleep(std::time::Duration::from_millis(ms));
                            smoke_ctrl.shutdown_blocking();
                            handle.exit(0);
                        });
                    }
                }
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
