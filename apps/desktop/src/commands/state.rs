//! Managed Tauri application state bridging control-plane commands and the zero-copy data plane.

use std::sync::Arc;

use ddscope_core::transport::list_serial_ports as core_list_serial_ports;
use ddscope_core::types::{ScopeId, SourceId};
use ddscope_core::CoreWorkspace;
use parking_lot::RwLock;

use crate::data_bridge::{DataBridgeModeDto, DataBridgeStatsDto, ZeroCopyDataBridge};

use super::convert;
use super::dto::{
    ApiErrorDto, ChannelTreeSnapshotDto, CreateSourceRequestDto, DownlinkReceiptDto,
    EngineStatusDto, RawRecorderRequestDto, ScopeViewportConfigDto, ScopeViewportSummaryDto,
    SeekReplayRequestDto, SendDownlinkRequestDto, SerialPortInfoDto, SourceSummaryDto,
    SyncGroupUpdateDto,
};

const DEFAULT_FALLBACK_QUOTA_BYTES: usize = 2 * 1024 * 1024 * 1024;

/// Backend engine binding mode stored inside [`DesktopBridgeState`].
#[derive(Clone, Debug, Default)]
pub enum ControlPlaneBackend {
    /// Pre-M4 (`T06`) stub backend returning `NOT_IMPLEMENTED` so frontend `BridgeClient` keeps
    /// its automatic `MockDdsfController` browser/headless fallback intact.
    #[default]
    Stub,
    /// Live [`CoreWorkspace`] engine backend wired to real multi-source acquisition and `DDSF v1`.
    Live(Arc<CoreWorkspace>),
}

/// Tauri managed state housing both the control-plane engine handle and [`ZeroCopyDataBridge`].
#[derive(Clone)]
pub struct DesktopBridgeState {
    backend: Arc<RwLock<ControlPlaneBackend>>,
    data_bridge: Arc<ZeroCopyDataBridge>,
}

impl std::fmt::Debug for DesktopBridgeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DesktopBridgeState")
            .field("is_live", &self.is_live())
            .field("data_bridge", &self.data_bridge)
            .finish()
    }
}

impl Default for DesktopBridgeState {
    fn default() -> Self {
        Self::new_stub()
    }
}

impl DesktopBridgeState {
    /// Creates a pre-M4 stub state (`ControlPlaneBackend::Stub`) with an empty [`ZeroCopyDataBridge`].
    pub fn new_stub() -> Self {
        Self {
            backend: Arc::new(RwLock::new(ControlPlaneBackend::Stub)),
            data_bridge: Arc::new(ZeroCopyDataBridge::new()),
        }
    }

    /// Creates a state directly bound to a live [`CoreWorkspace`].
    pub fn with_workspace(workspace: Arc<CoreWorkspace>) -> Self {
        Self {
            backend: Arc::new(RwLock::new(ControlPlaneBackend::Live(workspace))),
            data_bridge: Arc::new(ZeroCopyDataBridge::new()),
        }
    }

    /// Attaches or replaces the live [`CoreWorkspace`] (or switches back to `Stub` if `None`).
    pub fn set_workspace(&self, workspace: Option<Arc<CoreWorkspace>>) {
        *self.backend.write() = match workspace {
            Some(ws) => ControlPlaneBackend::Live(ws),
            None => ControlPlaneBackend::Stub,
        };
    }

    /// Returns `true` when backed by a live [`CoreWorkspace`].
    #[inline]
    pub fn is_live(&self) -> bool {
        matches!(&*self.backend.read(), ControlPlaneBackend::Live(_))
    }

    /// Returns a cloned handle to the live [`CoreWorkspace`], if bound.
    pub fn workspace(&self) -> Option<Arc<CoreWorkspace>> {
        match &*self.backend.read() {
            ControlPlaneBackend::Stub => None,
            ControlPlaneBackend::Live(ws) => Some(Arc::clone(ws)),
        }
    }

    /// Returns the shared zero-copy [`ZeroCopyDataBridge`].
    #[inline]
    pub fn data_bridge(&self) -> &Arc<ZeroCopyDataBridge> {
        &self.data_bridge
    }

    fn require_live_workspace(&self, command: &str) -> Result<Arc<CoreWorkspace>, ApiErrorDto> {
        self.workspace()
            .ok_or_else(|| ApiErrorDto::not_implemented(command))
    }

    /// Implements `get_engine_info`.
    pub fn engine_info(&self) -> EngineStatusDto {
        let info = ddscope_core::build_info();
        EngineStatusDto {
            core_name: info.name.to_string(),
            core_version: info.version.to_string(),
            bridge_ready: self.is_live(),
            ddsf_version: ddscope_core::ddsf::DDSF_VERSION_V1,
        }
    }

    /// Implements `list_serial_ports`.
    pub fn list_serial_ports(&self) -> Result<Vec<SerialPortInfoDto>, ApiErrorDto> {
        let ports = match self.workspace() {
            Some(ws) => ws.list_serial_ports().map_err(ApiErrorDto::from)?,
            None => core_list_serial_ports().unwrap_or_default(),
        };
        Ok(ports
            .into_iter()
            .map(convert::serial_port_to_dto)
            .collect())
    }

    /// Implements `create_source`.
    pub fn create_source(
        &self,
        config: CreateSourceRequestDto,
    ) -> Result<SourceSummaryDto, ApiErrorDto> {
        let ws = self.require_live_workspace("create_source")?;
        let core_cfg = convert::create_source_dto_to_core(config)?;
        let summary = ws.create_source(core_cfg).map_err(ApiErrorDto::from)?;
        Ok(convert::source_summary_to_dto(summary))
    }

    /// Implements `start_source`.
    pub fn start_source(&self, source_id: String) -> Result<SourceSummaryDto, ApiErrorDto> {
        let ws = self.require_live_workspace("start_source")?;
        let summary = ws
            .start_source(&SourceId::new(source_id))
            .map_err(ApiErrorDto::from)?;
        Ok(convert::source_summary_to_dto(summary))
    }

    /// Implements `stop_source`.
    pub async fn stop_source(&self, source_id: String) -> Result<SourceSummaryDto, ApiErrorDto> {
        let ws = self.require_live_workspace("stop_source")?;
        let summary = ws
            .stop_source(&SourceId::new(source_id))
            .await
            .map_err(ApiErrorDto::from)?;
        Ok(convert::source_summary_to_dto(summary))
    }

    /// Implements `remove_source`.
    pub async fn remove_source(&self, source_id: String) -> Result<(), ApiErrorDto> {
        let ws = self.require_live_workspace("remove_source")?;
        ws.remove_source(&SourceId::new(source_id))
            .await
            .map_err(ApiErrorDto::from)
    }

    /// Implements `seek_replay_source`.
    pub fn seek_replay_source(&self, req: SeekReplayRequestDto) -> Result<(), ApiErrorDto> {
        let ws = self.require_live_workspace("seek_replay_source")?;
        if !req.offset_ratio.is_finite() {
            return Err(ApiErrorDto::invalid_config(
                "offset_ratio must be finite",
            ));
        }
        ws.seek_replay_source(
            &SourceId::new(req.source_id),
            Some(req.offset_ratio),
            req.paused,
            req.playback_speed,
        )
        .map_err(ApiErrorDto::from)
    }

    /// Implements `set_raw_recorder`.
    pub fn set_raw_recorder(
        &self,
        req: RawRecorderRequestDto,
    ) -> Result<SourceSummaryDto, ApiErrorDto> {
        let ws = self.require_live_workspace("set_raw_recorder")?;
        let summary = ws
            .set_raw_recorder(&SourceId::new(req.source_id), req.enabled, req.output_path)
            .map_err(ApiErrorDto::from)?;
        Ok(convert::source_summary_to_dto(summary))
    }

    /// Implements `get_channel_tree`.
    pub fn get_channel_tree(&self) -> Result<ChannelTreeSnapshotDto, ApiErrorDto> {
        match self.workspace() {
            Some(ws) => Ok(convert::build_channel_tree_dto(&ws)),
            None => Ok(ChannelTreeSnapshotDto {
                sources: Vec::new(),
                quota_limit_bytes: DEFAULT_FALLBACK_QUOTA_BYTES as f64,
                quota_used_bytes: 0.0,
                pinned_bytes: 0.0,
            }),
        }
    }

    /// Implements `upsert_scope_viewport`.
    pub fn upsert_scope_viewport(
        &self,
        config: ScopeViewportConfigDto,
    ) -> Result<ScopeViewportSummaryDto, ApiErrorDto> {
        let ws = self.require_live_workspace("upsert_scope_viewport")?;
        let core_cfg = convert::scope_config_dto_to_core(config)?;
        let summary = ws.upsert_scope(core_cfg).map_err(ApiErrorDto::from)?;
        Ok(convert::scope_summary_to_dto(summary))
    }

    /// Implements `remove_scope_viewport`.
    pub fn remove_scope_viewport(&self, scope_id: u32) -> Result<(), ApiErrorDto> {
        let ws = self.require_live_workspace("remove_scope_viewport")?;
        ws.remove_scope(&ScopeId(scope_id))
            .map_err(ApiErrorDto::from)
    }

    /// Implements `rearm_scope`.
    pub fn rearm_scope(&self, scope_id: u32) -> Result<(), ApiErrorDto> {
        let ws = self.require_live_workspace("rearm_scope")?;
        ws.rearm_scope(&ScopeId(scope_id))
            .map(|_| ())
            .map_err(ApiErrorDto::from)
    }

    /// Implements `update_sync_group`.
    pub fn update_sync_group(&self, update: SyncGroupUpdateDto) -> Result<(), ApiErrorDto> {
        let ws = self.require_live_workspace("update_sync_group")?;
        let core_update = convert::sync_group_update_dto_to_core(update)?;
        ws.update_sync_group(&core_update)
            .map(|_| ())
            .map_err(ApiErrorDto::from)
    }

    /// Implements `send_downlink`.
    pub fn send_downlink(
        &self,
        req: SendDownlinkRequestDto,
    ) -> Result<DownlinkReceiptDto, ApiErrorDto> {
        let ws = self.require_live_workspace("send_downlink")?;
        let (source_id, core_req) = convert::downlink_dto_to_core(req)?;
        let receipt = ws
            .send_downlink(&source_id, &core_req)
            .map_err(ApiErrorDto::from)?;
        Ok(convert::tx_receipt_to_dto(receipt))
    }

    /// Implements `detach_data_bridge`.
    pub fn detach_data_bridge(&self) -> Result<(), ApiErrorDto> {
        self.data_bridge.detach();
        Ok(())
    }

    /// Implements `get_data_bridge_stats`.
    pub fn data_bridge_stats(&self) -> DataBridgeStatsDto {
        self.data_bridge.stats_dto()
    }

    /// Implements `set_data_bridge_rate`.
    pub fn set_data_bridge_rate(&self, target_fps: u32) -> Result<u32, ApiErrorDto> {
        Ok(self.data_bridge.set_target_fps(target_fps))
    }

    /// Steps triggers in `CoreWorkspace` at `now_ns` and pushes the packed `DDSF v1` frame
    /// directly into the attached [`ZeroCopyDataBridge`] without intermediate heap copies.
    pub fn step_and_push_ddsf_frame(
        &self,
        now_ns: u64,
    ) -> Result<Option<DataBridgeModeDto>, ApiErrorDto> {
        let Some(ws) = self.workspace() else {
            return Ok(None);
        };
        if !self.data_bridge.is_attached() {
            return Ok(None);
        }
        let push_res = ws
            .step_and_pack_ddsf_with(now_ns, |slice| self.data_bridge.push_frame(slice))
            .map_err(ApiErrorDto::from)?;
        let mode = push_res.map_err(ApiErrorDto::from)?;
        Ok(Some(mode))
    }
}
