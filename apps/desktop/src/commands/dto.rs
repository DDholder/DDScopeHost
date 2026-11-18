//! Frozen `tauri-specta` control-plane DTOs and telemetry event payloads (`T03`).
//!
//! All numeric fields avoid 64-bit integer types (`u64`/`i64`/`usize`/`isize`) so
//! `specta-typescript` exports clean `number` types without precision-loss errors.

use ddscope_core::types::CoreError;
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri_specta::Event;

pub use crate::data_bridge::{
    DataBridgeAttachDto, DataBridgeModeDto, DataBridgeStatsDto, DdsfSlotReadyDto,
};

/// Structured command error returned across the Tauri IPC boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ApiErrorDto {
    pub code: String,
    pub message: String,
}

impl ApiErrorDto {
    /// Creates an error DTO for a command not yet wired to a live `CoreWorkspace` (M1 gate stub).
    pub fn not_implemented(command: &str) -> Self {
        Self {
            code: "NOT_IMPLEMENTED".to_string(),
            message: format!("control command `{command}` is frozen in T03 and wired in T06"),
        }
    }

    /// Creates an error DTO for invalid caller parameters.
    pub fn invalid_config(message: impl Into<String>) -> Self {
        Self {
            code: "INVALID_CONFIG".to_string(),
            message: message.into(),
        }
    }
}

impl From<CoreError> for ApiErrorDto {
    fn from(err: CoreError) -> Self {
        let code = match &err {
            CoreError::Io(_) => "IO_ERROR",
            CoreError::Transport(_) => "TRANSPORT_ERROR",
            CoreError::Protocol { .. } => "PROTOCOL_ERROR",
            CoreError::Clock(_) => "CLOCK_ERROR",
            CoreError::Storage(_) => "STORAGE_ERROR",
            CoreError::QuotaExceeded { .. } => "QUOTA_EXCEEDED",
            CoreError::ChannelNotFound(_) => "CHANNEL_NOT_FOUND",
            CoreError::ScopeNotFound(_) => "SCOPE_NOT_FOUND",
            CoreError::SourceNotFound(_) => "SOURCE_NOT_FOUND",
            CoreError::SourceAlreadyExists(_) => "SOURCE_ALREADY_EXISTS",
            CoreError::InvalidConfig(_) => "INVALID_CONFIG",
            CoreError::ChannelClosed => "CHANNEL_CLOSED",
            CoreError::Unsupported(_) => "UNSUPPORTED",
        };
        Self {
            code: code.to_string(),
            message: err.to_string(),
        }
    }
}

/// Health and build metadata returned to the frontend workspace shell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct EngineStatusDto {
    pub core_name: String,
    pub core_version: String,
    pub bridge_ready: bool,
    pub ddsf_version: u16,
}

/// Serial port metadata returned by `list_serial_ports`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SerialPortInfoDto {
    pub port_name: String,
    pub description: Option<String>,
    pub vid: Option<u16>,
    pub pid: Option<u16>,
}

/// UART parity configuration.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SerialParityDto {
    #[default]
    None,
    Odd,
    Even,
}

/// UART stop-bit configuration.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SerialStopBitsDto {
    #[default]
    One,
    Two,
}

/// UART flow-control configuration.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SerialFlowControlDto {
    #[default]
    None,
    HardwareRtsCts,
    SoftwareXonXoff,
}

/// Transport endpoint configuration for creating a data source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TransportConfigDto {
    Sim {
        sample_rate_hz: f64,
        analog_channels: u8,
        enable_glitch: bool,
        glitch_period_samples: u32,
    },
    Serial {
        port_name: String,
        baud_rate: u32,
        data_bits: u8,
        parity: SerialParityDto,
        stop_bits: SerialStopBitsDto,
        flow_control: SerialFlowControlDto,
        auto_reconnect: bool,
    },
    Udp {
        bind_addr: String,
        multicast_group: Option<String>,
        recv_buffer_bytes: u32,
        remote_tx_addr: Option<String>,
    },
    TcpClient {
        remote_addr: String,
        auto_reconnect: bool,
        tcp_nodelay: bool,
    },
    TcpServer {
        listen_addr: String,
        tcp_nodelay: bool,
    },
    RawFileReplay {
        file_path: String,
        full_speed_offline: bool,
        playback_speed: f64,
        loop_playback: bool,
        nominal_byte_rate: Option<f64>,
    },
}

fn default_true() -> bool {
    true
}

/// Per-source hybrid clock synchronization configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ClockConfigDto {
    Tier1HardwareCounter { tick_hz: f64, counter_bits: u8 },
    Tier2SoftwarePll { nominal_sample_rate_hz: Option<f64> },
    Tier3HostArrival,
    ChannelCounter {
        channel: String,
        ns_per_count: f64,
        rollover_modulus: Option<f64>,
        #[serde(default = "default_true")]
        drift_compensation: bool,
    },
}

/// Field specification used by `FixedSchemaBinary` protocol mode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct FixedFieldSpecDto {
    pub name: String,
    pub scalar_type: String,
    pub scale: f64,
    pub offset: f64,
    pub unit: Option<String>,
    pub is_state_enum: bool,
}

/// Per-channel 32-bit parse type used by `JustTyped` protocol mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum JustTypedKindDto {
    #[default]
    Float,
    Int,
    Uint,
}

/// Channel specification used by `JustTyped` protocol mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct JustTypedChannelDto {
    pub name: String,
    #[serde(default)]
    pub data_type: JustTypedKindDto,
}

/// Protocol codec configuration attached to a data source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(tag = "codec", rename_all = "snake_case")]
pub enum ProtocolConfigDto {
    SimPreset,
    JustFloat {
        channel_names: Vec<String>,
    },
    JustTyped {
        channels: Vec<JustTypedChannelDto>,
    },
    FixedSchemaBinary {
        header_magic: Vec<u8>,
        fields: Vec<FixedFieldSpecDto>,
        has_hw_tick_first_col: bool,
    },
    FireWater {
        channel_names: Vec<String>,
    },
    Plugin {
        plugin_id: String,
        config_json: String,
    },
}

/// Request payload for `create_source`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct CreateSourceRequestDto {
    pub source_id: String,
    pub display_name: Option<String>,
    pub transport: TransportConfigDto,
    pub clock: ClockConfigDto,
    pub protocol: ProtocolConfigDto,
    pub auto_start: bool,
}

/// Lifecycle state of a registered data source.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SourceRuntimeStateDto {
    #[default]
    Stopped,
    Connecting,
    Running,
    Reconnecting,
    Completed,
    Error,
}

/// Summary descriptor for an active or configured data source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SourceSummaryDto {
    pub source_id: String,
    pub display_name: String,
    pub transport_kind: String,
    pub state: SourceRuntimeStateDto,
    pub raw_recording_path: Option<String>,
}

/// Request payload for seeking or pacing a `RawFileReplay` source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SeekReplayRequestDto {
    pub source_id: String,
    pub offset_ratio: f64,
    pub paused: Option<bool>,
    pub playback_speed: Option<f64>,
}

/// Request payload for enabling/disabling bypass raw byte recording on a source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct RawRecorderRequestDto {
    pub source_id: String,
    pub enabled: bool,
    pub output_path: Option<String>,
}

/// Channel modality classification exposed to the frontend channel tree.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ChannelModalityDto {
    Scalar,
    DigitalBool,
    Frame,
    StateMachine,
    EventLog,
}

/// Enumerated state label entry for MSO state-machine ribbon tracks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct StateLabelEntryDto {
    pub code: i32,
    pub name: String,
    pub color_rgba: Option<u32>,
}

/// Single channel node in the global `<source_id>/<local_path>` tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ChannelNodeDto {
    pub channel_id: u32,
    pub source_id: String,
    pub local_path: String,
    pub full_path: String,
    pub display_name: String,
    pub unit: Option<String>,
    pub modality: ChannelModalityDto,
    pub scalar_type: Option<String>,
    pub state_labels: Vec<StateLabelEntryDto>,
}

/// Channels grouped under one registered data source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SourceChannelGroupDto {
    pub source: SourceSummaryDto,
    pub channels: Vec<ChannelNodeDto>,
}

/// Full snapshot of the global channel tree and RAM quota accounting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct ChannelTreeSnapshotDto {
    pub sources: Vec<SourceChannelGroupDto>,
    pub quota_limit_bytes: f64,
    pub quota_used_bytes: f64,
    pub pinned_bytes: f64,
}

/// Oscilloscope trigger/sweep mode.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum TriggerSweepModeDto {
    #[default]
    Roll,
    Auto,
    Normal,
    Single,
}

/// Edge polarity for trigger configuration.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum EdgeSlopeDto {
    #[default]
    Rising,
    Falling,
    Either,
}

/// Trigger rule attached to an oscilloscope viewport.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TriggerConditionDto {
    Edge {
        channel_id: u32,
        level: f64,
        hysteresis: f64,
        slope: EdgeSlopeDto,
    },
    StateEquals {
        channel_id: u32,
        state_code: i32,
    },
}

/// Visual MSO track lane inside an oscilloscope viewport.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ScopeLaneKindDto {
    Analog,
    Digital,
    StateRibbon,
}

/// Channel-to-lane binding for an oscilloscope viewport.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct ScopeTrackBindingDto {
    pub channel_id: u32,
    pub lane: ScopeLaneKindDto,
    pub visible: bool,
    pub y_scale: Option<f64>,
    pub y_offset: Option<f64>,
    pub color_rgba: Option<u32>,
}

/// Complete configuration payload for `upsert_scope_viewport`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct ScopeViewportConfigDto {
    pub scope_id: u32,
    pub title: Option<String>,
    pub canvas_width_px: u32,
    pub time_span_ns: f64,
    pub pre_trigger_ratio: f64,
    pub pan_offset_ns: f64,
    pub sweep_mode: TriggerSweepModeDto,
    pub auto_timeout_ms: u32,
    pub trigger: Option<TriggerConditionDto>,
    pub tracks: Vec<ScopeTrackBindingDto>,
    pub sync_group_id: Option<u32>,
    pub paused: bool,
}

/// Summary returned after creating or updating an oscilloscope viewport.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ScopeViewportSummaryDto {
    pub scope_id: u32,
    pub active_track_count: u32,
    pub sync_group_id: Option<u32>,
}

/// Synchronization update propagated across all scopes in a `SyncGroup`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SyncGroupUpdateDto {
    pub group_id: u32,
    pub origin_scope_id: u32,
    pub time_span_ns: f64,
    pub center_offset_ns: f64,
    pub cursor_a_ratio: Option<f64>,
    pub cursor_b_ratio: Option<f64>,
}

/// Outbound transmission priority lane.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum TxPriorityDto {
    High,
    #[default]
    Normal,
}

/// Downlink payload variants supported by the Phase 1 debug/slider bar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DownlinkPayloadDto {
    HexBytes { hex: String },
    RawBytes { bytes: Vec<u8> },
    AsciiText { text: String, append_newline: bool },
    KeyValue { key: String, value: f64 },
}

/// Request payload for `send_downlink`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SendDownlinkRequestDto {
    pub source_id: String,
    pub payload: DownlinkPayloadDto,
    pub priority: TxPriorityDto,
    pub coalesce_key: Option<String>,
}

/// Receipt returned immediately by `send_downlink`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DownlinkReceiptDto {
    pub accepted: bool,
    pub coalesced_existing: bool,
    pub result_kind: String,
}

/// Per-source throughput and health telemetry emitted at 5-10Hz.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SourceTelemetryDto {
    pub source_id: String,
    pub state: SourceRuntimeStateDto,
    pub samples_per_sec: f64,
    pub bytes_per_sec: f64,
    pub dropped_packets: u32,
}

/// Low-frequency (5-10Hz) workspace telemetry event pushed to the frontend control store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type, Event)]
pub struct WorkspaceTelemetryEvent {
    pub timestamp_ms: f64,
    pub sources: Vec<SourceTelemetryDto>,
    pub quota_used_bytes: f64,
    pub quota_limit_bytes: f64,
    pub pinned_bytes: f64,
}

/// Threshold/window modes for Analog -> Digital conversion (`Bool` output).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum AnalogToDigitalModeDto {
    Schmitt {
        threshold: f64,
        hysteresis: f64,
        invert: bool,
    },
    Window {
        low: f64,
        high: f64,
        invert: bool,
    },
}

/// Display metadata entry for a discrete state code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct StateLabelSpecDto {
    pub code: i32,
    pub name: String,
    pub color_rgba: Option<u32>,
}

/// Numeric interval-to-state mapping rule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct StateRangeRuleDto {
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub code: i32,
    pub name: String,
    pub color_rgba: Option<u32>,
}

/// Conversion modes for Analog / Digital -> State Machine (`state_machine`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ToStateMachineModeDto {
    RangeMap {
        input_channel: String,
        hysteresis: f64,
        rules: Vec<StateRangeRuleDto>,
        default_state: StateLabelSpecDto,
    },
    BitCombine {
        bit_channels: Vec<String>,
        threshold: f64,
        labels: Vec<StateLabelSpecDto>,
    },
}

/// Numeric value assignment for a discrete state code.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct StateValueEntryDto {
    pub code: i32,
    pub value: f64,
}

/// Conversion modes for Digital / State -> Analog (`scalar`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ToAnalogModeDto {
    LevelMap {
        input_channel: String,
        low_value: f64,
        high_value: f64,
        threshold: f64,
        smoothing_tau_ms: f64,
    },
    BitWeightedDac {
        bit_channels: Vec<String>,
        scale: f64,
        offset: f64,
        threshold: f64,
    },
    StateValueMap {
        input_channel: String,
        entries: Vec<StateValueEntryDto>,
        default_value: f64,
    },
}

/// Supported sliding time-window metrics.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum WindowMetricKindDto {
    Rms,
    Mean,
    PeakToPeak,
    Min,
    Max,
    StdDev,
}

/// Calculus operator variants (`d/dt` and trapezoidal integral).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum CalculusModeDto {
    Derivative {
        scale: f64,
        filter_tau_ms: f64,
    },
    Integral {
        scale: f64,
        dc_offset: f64,
        initial_value: f64,
        clamp_min: Option<f64>,
        clamp_max: Option<f64>,
    },
}

/// Output channel selection flags for `SinglePhasePower`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct PowerOutputFlagsDto {
    pub freq: bool,
    pub u_rms: bool,
    pub i_rms: bool,
    pub p: bool,
    pub q1: bool,
    pub q_tot: bool,
    pub s: bool,
    pub pf: bool,
    pub phi_deg: bool,
    pub u_pk: bool,
    pub i_pk: bool,
    pub energy_wh: bool,
}

/// Virtual operator definition exposed across the control plane.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VirtualOperatorDto {
    AnalogToDigital {
        input_channel: String,
        output_path: String,
        mode: AnalogToDigitalModeDto,
    },
    ToStateMachine {
        output_path: String,
        mode: ToStateMachineModeDto,
    },
    ToAnalog {
        output_path: String,
        unit: Option<String>,
        mode: ToAnalogModeDto,
    },
    Formula {
        expression: String,
        output_path: String,
        unit: Option<String>,
        output_digital: bool,
    },
    WindowStats {
        input_channel: String,
        output_path: String,
        unit: Option<String>,
        metric: WindowMetricKindDto,
        window_ms: f64,
    },
    Calculus {
        input_channel: String,
        output_path: String,
        unit: Option<String>,
        mode: CalculusModeDto,
    },
    SinglePhasePower {
        voltage_channel: String,
        current_channel: String,
        output_prefix: String,
        voltage_scale: f64,
        voltage_offset: f64,
        current_scale: f64,
        current_offset: f64,
        nominal_freq_hz: f64,
        zero_cross_hysteresis_v: f64,
        min_freq_hz: f64,
        fallback_window_ms: f64,
        outputs: PowerOutputFlagsDto,
    },
}

/// Virtual channel node configuration DTO.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct VirtualNodeConfigDto {
    pub node_id: String,
    pub name: String,
    pub enabled: bool,
    pub operator: VirtualOperatorDto,
}

/// Virtual channel node registration summary DTO.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct VirtualNodeSummaryDto {
    pub node_id: String,
    pub name: String,
    pub enabled: bool,
    pub output_channels: Vec<ChannelNodeDto>,
}

/// Option entry inside an external plugin's `select` parameter schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct PluginSelectOptionDto {
    pub value: String,
    pub label_en: String,
    pub label_zh: String,
}

/// Declarative parameter definition exposed by an external protocol/widget `plugin.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct PluginParamSpecDto {
    pub key: String,
    pub label_en: String,
    pub label_zh: String,
    pub param_type: String,
    pub default_json: String,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: Option<f64>,
    pub options: Vec<PluginSelectOptionDto>,
    pub description_en: Option<String>,
    pub description_zh: Option<String>,
}

/// Preview channel hint declared by an external protocol plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct PluginChannelPreviewDto {
    pub name: String,
    pub scalar_type: String,
    pub unit: Option<String>,
}

/// Discovered external protocol (data engine) plugin descriptor DTO.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct ProtocolPluginInfoDto {
    pub id: String,
    pub name_en: String,
    pub name_zh: String,
    pub description_en: Option<String>,
    pub description_zh: Option<String>,
    pub version: String,
    pub backend_mode: String,
    pub available: bool,
    pub error: Option<String>,
    pub manifest_path: String,
    pub params: Vec<PluginParamSpecDto>,
    pub channels_preview: Vec<PluginChannelPreviewDto>,
}

/// Discovered external dashboard widget plugin descriptor DTO.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct WidgetPluginInfoDto {
    pub id: String,
    pub name_en: String,
    pub name_zh: String,
    pub description_en: Option<String>,
    pub description_zh: Option<String>,
    pub version: String,
    pub badge: String,
    pub default_col_span: u32,
    pub default_row_span: u32,
    pub binding_mode: String,
    pub available: bool,
    pub error: Option<String>,
    pub manifest_path: String,
    pub params: Vec<PluginParamSpecDto>,
}

/// Catalog of all discovered external protocol and widget plugins.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct ExternalPluginCatalogDto {
    pub search_dirs: Vec<String>,
    pub protocols: Vec<ProtocolPluginInfoDto>,
    pub widgets: Vec<WidgetPluginInfoDto>,
}

/// Loaded ESM module and optional CSS bundle for an external widget plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct WidgetPluginBundleDto {
    pub id: String,
    pub entry_js: String,
    pub style_css: Option<String>,
}
