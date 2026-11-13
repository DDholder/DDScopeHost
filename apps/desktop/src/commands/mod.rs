//! Strongly-typed Tauri control-plane commands and DTOs exported via `tauri-specta`.

pub mod dto;

pub use dto::*;

/// Returns `ddscope-core` build metadata and `DDSF v1` protocol version.
#[tauri::command]
#[specta::specta]
pub fn get_engine_info() -> EngineStatusDto {
    let info = ddscope_core::build_info();
    EngineStatusDto {
        core_name: info.name.to_string(),
        core_version: info.version.to_string(),
        bridge_ready: true,
        ddsf_version: ddscope_core::ddsf::DDSF_VERSION_V1,
    }
}

/// Enumerates available host serial ports.
#[tauri::command]
#[specta::specta]
pub fn list_serial_ports() -> Result<Vec<SerialPortInfoDto>, ApiErrorDto> {
    Ok(Vec::new())
}

/// Creates and registers a new multi-source pipeline (`Sim`, `Serial`, `UDP`, `TCP`, `RawFileReplay`).
#[tauri::command]
#[specta::specta]
pub fn create_source(_config: CreateSourceRequestDto) -> Result<SourceSummaryDto, ApiErrorDto> {
    Err(ApiErrorDto::not_implemented("create_source"))
}

/// Starts acquisition on a registered data source.
#[tauri::command]
#[specta::specta]
pub fn start_source(_source_id: String) -> Result<SourceSummaryDto, ApiErrorDto> {
    Err(ApiErrorDto::not_implemented("start_source"))
}

/// Stops acquisition on a registered data source.
#[tauri::command]
#[specta::specta]
pub fn stop_source(_source_id: String) -> Result<SourceSummaryDto, ApiErrorDto> {
    Err(ApiErrorDto::not_implemented("stop_source"))
}

/// Removes a stopped data source and releases its resources.
#[tauri::command]
#[specta::specta]
pub fn remove_source(_source_id: String) -> Result<(), ApiErrorDto> {
    Err(ApiErrorDto::not_implemented("remove_source"))
}

/// Seeks or updates playback rate/pause state on a `RawFileReplay` source.
#[tauri::command]
#[specta::specta]
pub fn seek_replay_source(_req: SeekReplayRequestDto) -> Result<(), ApiErrorDto> {
    Err(ApiErrorDto::not_implemented("seek_replay_source"))
}

/// Enables or disables bypass raw byte recording (`.ddraw`/`.bin`) on a source.
#[tauri::command]
#[specta::specta]
pub fn set_raw_recorder(_req: RawRecorderRequestDto) -> Result<SourceSummaryDto, ApiErrorDto> {
    Err(ApiErrorDto::not_implemented("set_raw_recorder"))
}

/// Returns the current global `<source_id>/<channel>` tree and memory quota usage.
#[tauri::command]
#[specta::specta]
pub fn get_channel_tree() -> Result<ChannelTreeSnapshotDto, ApiErrorDto> {
    Ok(ChannelTreeSnapshotDto {
        sources: Vec::new(),
        quota_limit_bytes: (2_u64 * 1024 * 1024 * 1024) as f64,
        quota_used_bytes: 0.0,
        pinned_bytes: 0.0,
    })
}

/// Creates or updates an independent oscilloscope viewport controller configuration.
#[tauri::command]
#[specta::specta]
pub fn upsert_scope_viewport(
    _config: ScopeViewportConfigDto,
) -> Result<ScopeViewportSummaryDto, ApiErrorDto> {
    Err(ApiErrorDto::not_implemented("upsert_scope_viewport"))
}

/// Removes an oscilloscope viewport controller and releases any pinned chunks.
#[tauri::command]
#[specta::specta]
pub fn remove_scope_viewport(_scope_id: u32) -> Result<(), ApiErrorDto> {
    Err(ApiErrorDto::not_implemented("remove_scope_viewport"))
}

/// Re-arms a latched `Single` trigger oscilloscope and unpins its retained chunks.
#[tauri::command]
#[specta::specta]
pub fn rearm_scope(_scope_id: u32) -> Result<(), ApiErrorDto> {
    Err(ApiErrorDto::not_implemented("rearm_scope"))
}

/// Propagates timebase/pan/cursor synchronization across a `SyncGroup`.
#[tauri::command]
#[specta::specta]
pub fn update_sync_group(_update: SyncGroupUpdateDto) -> Result<(), ApiErrorDto> {
    Err(ApiErrorDto::not_implemented("update_sync_group"))
}

/// Dispatches an outbound command or coalesced parameter value to a data source.
#[tauri::command]
#[specta::specta]
pub fn send_downlink(_req: SendDownlinkRequestDto) -> Result<DownlinkReceiptDto, ApiErrorDto> {
    Err(ApiErrorDto::not_implemented("send_downlink"))
}
