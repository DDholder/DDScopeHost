//! Strongly-typed Tauri control-plane commands exported via `tauri-specta`.

use serde::{Deserialize, Serialize};
use specta::Type;

/// Health and build metadata returned to the frontend workspace shell.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct EngineStatusDto {
    pub core_name: String,
    pub core_version: String,
    pub bridge_ready: bool,
}

/// Returns `ddscope-core` build metadata for M0 bridge verification.
#[tauri::command]
#[specta::specta]
pub fn get_engine_info() -> EngineStatusDto {
    let info = ddscope_core::build_info();
    EngineStatusDto {
        core_name: info.name.to_string(),
        core_version: info.version.to_string(),
        bridge_ready: true,
    }
}
