//! Strongly-typed Tauri control-plane commands and DTOs exported via `tauri-specta`.

pub mod convert;
pub mod dto;
pub mod state;

use std::sync::Arc;
use tauri::ipc::Channel;

use crate::data_bridge::{FrameSink, RawDdsfFrame, RawIpcFrameSink};
pub use dto::*;
pub use state::{ControlPlaneBackend, DesktopBridgeState};

/// Returns `ddscope-core` build metadata and `DDSF v1` protocol version.
#[tauri::command]
#[specta::specta]
pub fn get_engine_info(state: tauri::State<'_, DesktopBridgeState>) -> EngineStatusDto {
    state.engine_info()
}

/// Enumerates host serial ports currently visible to the desktop engine.
#[tauri::command]
#[specta::specta]
pub fn list_serial_ports(
    state: tauri::State<'_, DesktopBridgeState>,
) -> Result<Vec<SerialPortInfoDto>, ApiErrorDto> {
    state.list_serial_ports()
}

/// Registers a new physical or simulated data source.
#[tauri::command]
#[specta::specta]
pub fn create_source(
    state: tauri::State<'_, DesktopBridgeState>,
    config: CreateSourceRequestDto,
) -> Result<SourceSummaryDto, ApiErrorDto> {
    state.create_source(config)
}

/// Validates and updates an existing physical or simulated data source configuration.
#[tauri::command]
#[specta::specta]
pub async fn update_source(
    state: tauri::State<'_, DesktopBridgeState>,
    source_id: String,
    config: CreateSourceRequestDto,
) -> Result<SourceSummaryDto, ApiErrorDto> {
    state.update_source(source_id, config).await
}

/// Returns workspace run-divider boundary timestamps in nanoseconds (`u64` represented as `f64`).
#[tauri::command]
#[specta::specta]
pub fn get_run_boundaries(
    state: tauri::State<'_, DesktopBridgeState>,
) -> Result<Vec<f64>, ApiErrorDto> {
    state.get_run_boundaries()
}

/// Starts acquisition on an existing source.
#[tauri::command]
#[specta::specta]
pub fn start_source(
    state: tauri::State<'_, DesktopBridgeState>,
    source_id: String,
) -> Result<SourceSummaryDto, ApiErrorDto> {
    state.start_source(source_id)
}

/// Stops acquisition on an active source without deleting its configuration.
#[tauri::command]
#[specta::specta]
pub async fn stop_source(
    state: tauri::State<'_, DesktopBridgeState>,
    source_id: String,
) -> Result<SourceSummaryDto, ApiErrorDto> {
    state.stop_source(source_id).await
}

/// Stops (if running) and removes a source from the workspace.
#[tauri::command]
#[specta::specta]
pub async fn remove_source(
    state: tauri::State<'_, DesktopBridgeState>,
    source_id: String,
) -> Result<(), ApiErrorDto> {
    state.remove_source(source_id).await
}

/// Seeks a replay source to a byte offset or microsecond timestamp.
#[tauri::command]
#[specta::specta]
pub fn seek_replay_source(
    state: tauri::State<'_, DesktopBridgeState>,
    req: SeekReplayRequestDto,
) -> Result<(), ApiErrorDto> {
    state.seek_replay_source(req)
}

/// Enables or disables raw `.ddraw` recording on a source.
#[tauri::command]
#[specta::specta]
pub fn set_raw_recorder(
    state: tauri::State<'_, DesktopBridgeState>,
    req: RawRecorderRequestDto,
) -> Result<SourceSummaryDto, ApiErrorDto> {
    state.set_raw_recorder(req)
}

/// Returns the complete channel tree snapshot across all workspace sources.
#[tauri::command]
#[specta::specta]
pub fn get_channel_tree(
    state: tauri::State<'_, DesktopBridgeState>,
) -> Result<ChannelTreeSnapshotDto, ApiErrorDto> {
    state.get_channel_tree()
}

/// Returns exportable control-plane configurations for all registered workspace sources.
#[tauri::command]
#[specta::specta]
pub fn get_source_configs(
    state: tauri::State<'_, DesktopBridgeState>,
) -> Result<Vec<CreateSourceRequestDto>, ApiErrorDto> {
    state.get_source_configs()
}

/// Creates or updates a scope viewport subscription, tracks, trigger, and quota.
#[tauri::command]
#[specta::specta]
pub fn upsert_scope_viewport(
    state: tauri::State<'_, DesktopBridgeState>,
    config: ScopeViewportConfigDto,
) -> Result<ScopeViewportSummaryDto, ApiErrorDto> {
    state.upsert_scope_viewport(config)
}

/// Removes a scope viewport subscription from the workspace.
#[tauri::command]
#[specta::specta]
pub fn remove_scope_viewport(
    state: tauri::State<'_, DesktopBridgeState>,
    scope_id: u32,
) -> Result<(), ApiErrorDto> {
    state.remove_scope_viewport(scope_id)
}

/// Re-arms a single-shot or normal trigger on the given scope viewport.
#[tauri::command]
#[specta::specta]
pub fn rearm_scope(
    state: tauri::State<'_, DesktopBridgeState>,
    scope_id: u32,
) -> Result<(), ApiErrorDto> {
    state.rearm_scope(scope_id)
}

/// Updates shared cursor / zoom / pause synchronization for a sync group.
#[tauri::command]
#[specta::specta]
pub fn update_sync_group(
    state: tauri::State<'_, DesktopBridgeState>,
    update: SyncGroupUpdateDto,
) -> Result<(), ApiErrorDto> {
    state.update_sync_group(update)
}

/// Sends a downlink control or command payload to a source transport.
#[tauri::command]
#[specta::specta]
pub fn send_downlink(
    state: tauri::State<'_, DesktopBridgeState>,
    req: SendDownlinkRequestDto,
) -> Result<DownlinkReceiptDto, ApiErrorDto> {
    state.send_downlink(req)
}

/// Attaches the high-frequency zero-copy `DDSF v1` data bridge to the calling webview window.
///
/// On Windows, negotiates a `3 x 8 MiB` WebView2 `SharedBuffer` ring by default and falls back
/// to raw binary Tauri IPC channels (`InvokeResponseBody::Raw`) when `RawIpcChannel` is
/// requested or when `SharedBuffer` initialization is unavailable in auto mode.
#[tauri::command]
#[specta::specta]
pub async fn attach_data_bridge(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, DesktopBridgeState>,
    preferred_mode: Option<DataBridgeModeDto>,
    slot_notify_channel: Channel<DdsfSlotReadyDto>,
    raw_frame_channel: Channel<RawDdsfFrame>,
) -> Result<DataBridgeAttachDto, ApiErrorDto> {
    let (sink, attach_dto): (Arc<dyn FrameSink>, DataBridgeAttachDto) = match preferred_mode {
        Some(DataBridgeModeDto::RawIpcChannel) => (
            Arc::new(RawIpcFrameSink::new(raw_frame_channel)),
            DataBridgeAttachDto {
                active_mode: DataBridgeModeDto::RawIpcChannel,
                slot_count: 0,
                slot_bytes: 0,
            },
        ),
        #[cfg(not(windows))]
        Some(DataBridgeModeDto::WebView2SharedBuffer) => {
            return Err(ApiErrorDto {
                code: "SHM_UNSUPPORTED_PLATFORM".to_string(),
                message: "WebView2 SharedBuffer zero-copy SHM is only supported on Windows"
                    .to_string(),
            });
        }
        #[cfg(not(windows))]
        None => (
            Arc::new(RawIpcFrameSink::new(raw_frame_channel)),
            DataBridgeAttachDto {
                active_mode: DataBridgeModeDto::RawIpcChannel,
                slot_count: 0,
                slot_bytes: 0,
            },
        ),
        #[cfg(windows)]
        mode @ (None | Some(DataBridgeModeDto::WebView2SharedBuffer)) => {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let label = window.label().to_string();
            let dispatch_res = window.with_webview(move |platform_webview| {
                let attach_res =
                    crate::data_bridge::shm_windows::attach_or_repost_webview2_shm_on_sta(
                        &label,
                        platform_webview,
                    );
                let _ = tx.send(attach_res);
            });

            let shm_res = match dispatch_res {
                Ok(()) => match rx.await {
                    Ok(res) => res,
                    Err(e) => Err(crate::data_bridge::DataBridgeError::PlatformShm(format!(
                        "STA attach response channel closed: {e}"
                    ))),
                },
                Err(e) => Err(crate::data_bridge::DataBridgeError::PlatformShm(format!(
                    "with_webview dispatch failed: {e}"
                ))),
            };

            match shm_res {
                Ok(ring) => (
                    Arc::new(
                        crate::data_bridge::shm_windows::WebView2SharedBufferSink::new(
                            ring,
                            slot_notify_channel,
                            raw_frame_channel,
                        ),
                    ),
                    DataBridgeAttachDto {
                        active_mode: DataBridgeModeDto::WebView2SharedBuffer,
                        slot_count: crate::data_bridge::DDSF_SHM_SLOT_COUNT as u8,
                        slot_bytes: crate::data_bridge::DDSF_SHM_SLOT_BYTES as u32,
                    },
                ),
                Err(err) if matches!(mode, Some(DataBridgeModeDto::WebView2SharedBuffer)) => {
                    return Err(err.into());
                }
                Err(_) => (
                    Arc::new(RawIpcFrameSink::new(raw_frame_channel)),
                    DataBridgeAttachDto {
                        active_mode: DataBridgeModeDto::RawIpcChannel,
                        slot_count: 0,
                        slot_bytes: 0,
                    },
                ),
            }
        }
    };

    state.data_bridge().attach_sink(sink);
    Ok(attach_dto)
}

/// Detaches the active data-bridge sink so frame production pauses cleanly.
#[tauri::command]
#[specta::specta]
pub fn detach_data_bridge(state: tauri::State<'_, DesktopBridgeState>) -> Result<(), ApiErrorDto> {
    state.detach_data_bridge()
}

/// Returns a snapshot of data-bridge frame counters, throughput, and slot fallback metrics.
#[tauri::command]
#[specta::specta]
pub fn get_data_bridge_stats(state: tauri::State<'_, DesktopBridgeState>) -> DataBridgeStatsDto {
    state.data_bridge_stats()
}

/// Updates the target frame-push rate (clamped internally to `[15, 240]` Hz).
#[tauri::command]
#[specta::specta]
pub fn set_data_bridge_rate(
    state: tauri::State<'_, DesktopBridgeState>,
    rate_hz: u32,
) -> Result<u32, ApiErrorDto> {
    state.set_data_bridge_rate(rate_hz)
}

/// Loads the portable default project configuration stored alongside the executable.
#[tauri::command]
#[specta::specta]
pub fn load_default_project_config(
    state: tauri::State<'_, DesktopBridgeState>,
) -> Result<Option<String>, ApiErrorDto> {
    state.load_default_project_config()
}

/// Saves the portable default project configuration alongside the executable and returns the saved path.
#[tauri::command]
#[specta::specta]
pub fn save_default_project_config(
    state: tauri::State<'_, DesktopBridgeState>,
    config_json: String,
) -> Result<String, ApiErrorDto> {
    state.save_default_project_config(&config_json)
}

/// Clears the portable default project configuration stored alongside the executable.
#[tauri::command]
#[specta::specta]
pub fn clear_default_project_config(
    state: tauri::State<'_, DesktopBridgeState>,
) -> Result<bool, ApiErrorDto> {
    state.clear_default_project_config()
}

/// Lists all configured virtual channel DAG nodes.
#[tauri::command]
#[specta::specta]
pub fn list_virtual_channels(
    state: tauri::State<'_, DesktopBridgeState>,
) -> Result<Vec<VirtualNodeConfigDto>, ApiErrorDto> {
    state.list_virtual_channels()
}

/// Creates or updates a virtual channel DAG node and triggers retroactive backfill.
#[tauri::command]
#[specta::specta]
pub fn upsert_virtual_channel(
    state: tauri::State<'_, DesktopBridgeState>,
    config: VirtualNodeConfigDto,
) -> Result<VirtualNodeSummaryDto, ApiErrorDto> {
    state.upsert_virtual_channel(config)
}

/// Removes a virtual channel DAG node and its derived channels.
#[tauri::command]
#[specta::specta]
pub fn remove_virtual_channel(
    state: tauri::State<'_, DesktopBridgeState>,
    node_id: String,
) -> Result<bool, ApiErrorDto> {
    state.remove_virtual_channel(node_id)
}

/// Removes all virtual channel DAG nodes from the workspace.
#[tauri::command]
#[specta::specta]
pub fn clear_virtual_channels(
    state: tauri::State<'_, DesktopBridgeState>,
) -> Result<u32, ApiErrorDto> {
    state.clear_virtual_channels()
}

/// Lists all discovered external protocol and widget plugins.
#[tauri::command]
#[specta::specta]
pub fn list_external_plugins(
    state: tauri::State<'_, DesktopBridgeState>,
) -> Result<ExternalPluginCatalogDto, ApiErrorDto> {
    state.list_external_plugins()
}

/// Rescans external `plugins/` directories and reloads protocol plugin factories.
#[tauri::command]
#[specta::specta]
pub fn reload_external_plugins(
    state: tauri::State<'_, DesktopBridgeState>,
) -> Result<ExternalPluginCatalogDto, ApiErrorDto> {
    state.reload_external_plugins()
}

/// Loads the JavaScript module and optional stylesheet for an external widget plugin.
#[tauri::command]
#[specta::specta]
pub fn load_widget_plugin_bundle(
    state: tauri::State<'_, DesktopBridgeState>,
    plugin_id: String,
) -> Result<WidgetPluginBundleDto, ApiErrorDto> {
    state.load_widget_plugin_bundle(plugin_id)
}
