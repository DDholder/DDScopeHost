//! Tauri v2 desktop bridge host for DDScopeHost.

pub mod commands;
pub mod data_bridge;

use std::path::{Path, PathBuf};
use specta_typescript::Typescript;
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
            commands::upsert_scope_viewport,
            commands::remove_scope_viewport,
            commands::rearm_scope,
            commands::update_sync_group,
            commands::send_downlink,
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

    tauri::Builder::default()
        .invoke_handler(builder.invoke_handler())
        .setup(move |app| {
            builder.mount_events(app);

            #[cfg(debug_assertions)]
            if let Ok(raw_ms) = std::env::var("DDSCOPE_SMOKE_EXIT_MS") {
                if let Ok(ms) = raw_ms.parse::<u64>() {
                    let handle = app.handle().clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(ms));
                        handle.exit(0);
                    });
                }
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
