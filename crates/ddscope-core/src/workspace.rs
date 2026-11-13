//! Top-level multi-source session pipeline (`SourceSession`) and workspace orchestration.
//!
//! `SourceSession` (Track B Milestone `TB6`) integrates:
//! 1. [`TransportScheduler`] (async IO, slab buffer pool, coalescing TX queue, bypass `.ddraw`/`.bin` recorder),
//! 2. [`StreamClockSynchronizer`] (Tier 1 hardware counter drift anchoring, Tier 2 2nd-order PLL, Tier 3 host clock),
//! 3. [`ProtocolCodec`] (`SimPreset`, `JustFloat`, `FixedSchemaBinary`, `FireWater`, plus downlink encoding),
//! 4. [`ChannelHub`] (lock-free concurrent `<source_id>/<local_path>` registration and native-width columnar ingestion).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle as ThreadJoinHandle;
use std::time::{Duration, Instant};

use bytes::BytesMut;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::clock::{ClockMode, ClockSyncConfig, ClockSyncDiagnostics, StreamClockSynchronizer};
use crate::protocol::{
    encode_hex_string, DownlinkPayload, FireWaterConfig, FireWaterCsvCodec, FixedSchemaBinaryCodec,
    FixedSchemaBinaryConfig, JustFloatCodec, JustFloatConfig, ProtocolCodec, ProtocolRegistry,
    SimPresetCodec, SimPresetConfig, TransactionResult,
};
use crate::storage::{ChannelEntry, ChannelHub};
use crate::transport::{
    OutboundFrame, PacketReceiver, RawByteRecorder, RawByteTap, RawFileReplayConfig,
    RawFileReplayTransport, RawTapDirection, ReplayControlHandle, SerialConfig, SerialTransport,
    SharedRawTap, SimConfig, SimControlHandle, SimSignalTransport, TcpClientConfig,
    TcpClientTransport, TcpServerConfig, TcpServerTransport, Transport, TransportCaps,
    TransportKind, TransportMetrics, TransportMetricsSnapshot, TransportRegistry,
    TransportRuntimeConfig, TransportScheduler, TransportState, TxPriority, UdpConfig,
    UdpTransport,
};
use crate::types::{
    ChannelDescriptor, ChannelId, CoreError, CoreResult, DecodedBatch, ScalarValue, SourceId,
};

/// Transport configuration variants for constructing a [`SourceSession`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceTransportConfig {
    Sim(SimConfig),
    Serial(SerialConfig),
    Udp(UdpConfig),
    TcpClient(TcpClientConfig),
    TcpServer(TcpServerConfig),
    RawFileReplay(RawFileReplayConfig),
    Custom {
        transport_kind: TransportKind,
        json_config: String,
    },
}

impl SourceTransportConfig {
    /// Returns the [`TransportKind`] discriminant for this transport configuration.
    pub fn transport_kind(&self) -> TransportKind {
        match self {
            Self::Sim(_) => TransportKind::Sim,
            Self::Serial(_) => TransportKind::Serial,
            Self::Udp(_) => TransportKind::Udp,
            Self::TcpClient(_) => TransportKind::TcpClient,
            Self::TcpServer(_) => TransportKind::TcpServer,
            Self::RawFileReplay(_) => TransportKind::RawFileReplay,
            Self::Custom { transport_kind, .. } => *transport_kind,
        }
    }
}

/// Protocol codec configuration variants for constructing a [`SourceSession`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "codec", rename_all = "snake_case")]
pub enum SourceProtocolConfig {
    SimPreset(SimPresetConfig),
    JustFloat(JustFloatConfig),
    FixedSchemaBinary(FixedSchemaBinaryConfig),
    FireWater(FireWaterConfig),
    Custom {
        codec_id: String,
        json_config: String,
    },
}

/// Complete configuration for instantiating a [`SourceSession`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceConfig {
    pub source_id: SourceId,
    pub display_name: Option<String>,
    pub transport: SourceTransportConfig,
    pub clock_mode: ClockMode,
    #[serde(default)]
    pub clock_config: ClockSyncConfig,
    pub protocol: SourceProtocolConfig,
    #[serde(default)]
    pub runtime: TransportRuntimeConfig,
    #[serde(default)]
    pub auto_start: bool,
}

impl SourceConfig {
    /// Creates a new [`SourceConfig`] with default clock tuning and runtime parameters.
    pub fn new(
        source_id: impl Into<SourceId>,
        transport: SourceTransportConfig,
        clock_mode: ClockMode,
        protocol: SourceProtocolConfig,
    ) -> Self {
        Self {
            source_id: source_id.into(),
            display_name: None,
            transport,
            clock_mode,
            clock_config: ClockSyncConfig::default(),
            protocol,
            runtime: TransportRuntimeConfig::default(),
            auto_start: false,
        }
    }

    /// Convenience constructor for a `SimSignalTransport` + `SimPresetCodec` + `Tier1HardwareCounter` source.
    pub fn sim(source_id: impl Into<SourceId>, sim_cfg: SimConfig) -> Self {
        let source_id = source_id.into();
        let tick_hz = sim_cfg.sample_rate_hz;
        let analog_channels = sim_cfg.analog_channels;
        Self::new(
            source_id.clone(),
            SourceTransportConfig::Sim(sim_cfg),
            ClockMode::Tier1HardwareCounter {
                tick_hz,
                counter_bits: 64,
            },
            SourceProtocolConfig::SimPreset(SimPresetConfig {
                source_id: source_id.as_str().to_owned(),
                analog_channels,
                ..SimPresetConfig::default()
            }),
        )
    }

    /// Sets a human-readable display name for this source.
    pub fn with_display_name(mut self, name: impl Into<String>) -> Self {
        self.display_name = Some(name.into());
        self
    }

    /// Overrides the [`ClockSyncConfig`] parameters.
    pub fn with_clock_config(mut self, clock_config: ClockSyncConfig) -> Self {
        self.clock_config = clock_config;
        self
    }

    /// Overrides the [`TransportRuntimeConfig`] parameters.
    pub fn with_runtime_config(mut self, runtime: TransportRuntimeConfig) -> Self {
        self.runtime = runtime;
        self
    }

    /// Sets whether the source should automatically start upon registration.
    pub fn with_auto_start(mut self, auto_start: bool) -> Self {
        self.auto_start = auto_start;
        self
    }
}

/// Lifecycle state of a [`SourceSession`], matching control-plane `SourceRuntimeStateDto`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceRuntimeState {
    #[default]
    Stopped,
    Connecting,
    Running,
    Reconnecting,
    Completed,
    Error,
}

/// Summary descriptor of a [`SourceSession`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSummary {
    pub source_id: SourceId,
    pub display_name: String,
    pub transport_kind: TransportKind,
    pub state: SourceRuntimeState,
    pub raw_recording_path: Option<PathBuf>,
}

/// Outbound downlink dispatch request submitted to a [`SourceSession`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DownlinkRequest {
    pub payload: DownlinkPayload,
    pub priority: TxPriority,
    pub coalesce_key: Option<String>,
    pub transaction_id: Option<u64>,
}

impl DownlinkRequest {
    /// Creates a new fire-and-forget [`DownlinkRequest`].
    pub fn new(payload: DownlinkPayload, priority: TxPriority) -> Self {
        Self {
            payload,
            priority,
            coalesce_key: None,
            transaction_id: None,
        }
    }

    /// Parses a hex string into raw bytes and creates a [`DownlinkRequest`].
    pub fn from_hex(hex_str: &str, priority: TxPriority) -> CoreResult<Self> {
        let mut buf = BytesMut::new();
        encode_hex_string(hex_str, &mut buf)?;
        Ok(Self::new(
            DownlinkPayload::RawBytes(buf.to_vec()),
            priority,
        ))
    }

    /// Creates an ASCII text [`DownlinkRequest`], optionally appending `'\n'` if not present.
    pub fn from_ascii(text: impl Into<String>, append_newline: bool, priority: TxPriority) -> Self {
        let mut s = text.into();
        if append_newline && !s.ends_with('\n') {
            s.push('\n');
        }
        Self::new(DownlinkPayload::AsciiLine(s), priority)
    }

    /// Creates a `key=value\n` parameter [`DownlinkRequest`].
    pub fn from_key_value(
        key: impl Into<String>,
        value: ScalarValue,
        priority: TxPriority,
    ) -> Self {
        Self::new(
            DownlinkPayload::KeyValue {
                key: key.into(),
                value,
            },
            priority,
        )
    }

    /// Attaches a `coalesce_key` so unsent frames with the same key are overwritten in-place.
    pub fn with_coalesce_key(mut self, key: impl Into<String>) -> Self {
        self.coalesce_key = Some(key.into());
        self
    }

    /// Attaches an optional Phase 2 ACK transaction ID.
    pub fn with_transaction_id(mut self, transaction_id: u64) -> Self {
        self.transaction_id = Some(transaction_id);
        self
    }
}

/// Receipt returned when a [`DownlinkRequest`] is encoded and queued by a [`SourceSession`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxDispatchReceipt {
    pub accepted: bool,
    pub coalesced_existing: bool,
    pub queue_len: usize,
    pub effective_priority: TxPriority,
    pub encoded_bytes: usize,
    pub result: TransactionResult,
}

/// Point-in-time telemetry snapshot for a [`SourceSession`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceTelemetrySnapshot {
    pub source_id: SourceId,
    pub state: SourceRuntimeState,
    pub samples_per_sec: f64,
    pub bytes_per_sec: f64,
    pub dropped_packets: u64,
    pub total_rx_packets: u64,
    pub total_rx_bytes: u64,
    pub total_decoded_samples: u64,
    pub total_decoded_batches: u64,
    pub decode_errors: u64,
    pub total_tx_submitted: u64,
    pub total_tx_coalesced: u64,
    pub total_tx_dispatched: u64,
    pub clock_diagnostics: ClockSyncDiagnostics,
}

#[derive(Debug)]
struct WorkerTelemetryState {
    stop_requested: AtomicBool,
    seek_reset_requested: AtomicBool,
    worker_active: AtomicBool,
    worker_completed: AtomicBool,
    worker_error: AtomicBool,
    decoded_samples: AtomicU64,
    decoded_batches: AtomicU64,
    decode_errors: AtomicU64,
    ack_events_received: AtomicU64,
    has_prev_raw_recv: AtomicBool,
    prev_raw_recv_ns: AtomicU64,
    last_packet_recv_ns: AtomicU64,
    last_sample_ts_ns: AtomicU64,
    last_error_msg: Mutex<Option<String>>,
    rate_window: Mutex<RateWindowState>,
}

#[derive(Debug)]
struct RateWindowState {
    started_at: Option<Instant>,
    last_sample_at: Option<Instant>,
    last_samples: u64,
    last_bytes: u64,
    smoothed_sps: f64,
    smoothed_bps: f64,
}

impl WorkerTelemetryState {
    fn new() -> Self {
        Self {
            stop_requested: AtomicBool::new(false),
            seek_reset_requested: AtomicBool::new(false),
            worker_active: AtomicBool::new(false),
            worker_completed: AtomicBool::new(false),
            worker_error: AtomicBool::new(false),
            decoded_samples: AtomicU64::new(0),
            decoded_batches: AtomicU64::new(0),
            decode_errors: AtomicU64::new(0),
            ack_events_received: AtomicU64::new(0),
            has_prev_raw_recv: AtomicBool::new(false),
            prev_raw_recv_ns: AtomicU64::new(0),
            last_packet_recv_ns: AtomicU64::new(0),
            last_sample_ts_ns: AtomicU64::new(0),
            last_error_msg: Mutex::new(None),
            rate_window: Mutex::new(RateWindowState {
                started_at: None,
                last_sample_at: None,
                last_samples: 0,
                last_bytes: 0,
                smoothed_sps: 0.0,
                smoothed_bps: 0.0,
            }),
        }
    }

    fn mark_started(&self, current_samples: u64, current_bytes: u64) {
        self.stop_requested.store(false, Ordering::Release);
        self.seek_reset_requested.store(false, Ordering::Release);
        self.worker_completed.store(false, Ordering::Release);
        self.worker_error.store(false, Ordering::Release);
        self.worker_active.store(true, Ordering::Release);
        *self.last_error_msg.lock() = None;

        let now = Instant::now();
        let mut win = self.rate_window.lock();
        win.started_at = Some(now);
        win.last_sample_at = Some(now);
        win.last_samples = current_samples;
        win.last_bytes = current_bytes;
        win.smoothed_sps = 0.0;
        win.smoothed_bps = 0.0;
    }

    fn compute_rates(&self, total_samples: u64, total_bytes: u64, is_active: bool) -> (f64, f64) {
        let mut win = self.rate_window.lock();
        let Some(started_at) = win.started_at else {
            return (0.0, 0.0);
        };
        let now = Instant::now();
        let last_at = win.last_sample_at.unwrap_or(started_at);
        let dt = now.duration_since(last_at).as_secs_f64();

        if dt >= 0.020 {
            let d_samples = total_samples.saturating_sub(win.last_samples);
            let d_bytes = total_bytes.saturating_sub(win.last_bytes);
            let inst_sps = (d_samples as f64) / dt;
            let inst_bps = (d_bytes as f64) / dt;

            win.smoothed_sps = if win.smoothed_sps <= 0.0 {
                inst_sps
            } else {
                0.6 * inst_sps + 0.4 * win.smoothed_sps
            };
            win.smoothed_bps = if win.smoothed_bps <= 0.0 {
                inst_bps
            } else {
                0.6 * inst_bps + 0.4 * win.smoothed_bps
            };
            win.last_sample_at = Some(now);
            win.last_samples = total_samples;
            win.last_bytes = total_bytes;
        }

        if win.smoothed_sps > 0.0 || win.smoothed_bps > 0.0 {
            return (win.smoothed_sps, win.smoothed_bps);
        }

        if is_active || total_samples > 0 || total_bytes > 0 {
            let total_elapsed = now.duration_since(started_at).as_secs_f64().max(1e-6);
            return (
                (total_samples as f64) / total_elapsed,
                (total_bytes as f64) / total_elapsed,
            );
        }

        (0.0, 0.0)
    }
}

/// Pre-built components for constructing a [`SourceSession`] with custom or mock transports/codecs.
pub struct SourceSessionComponents {
    pub source_id: SourceId,
    pub display_name: Option<String>,
    pub transport: Box<dyn Transport>,
    pub clock: StreamClockSynchronizer,
    pub codec: Box<dyn ProtocolCodec>,
    pub runtime_config: TransportRuntimeConfig,
    pub sim_control: Option<SimControlHandle>,
    pub replay_control: Option<ReplayControlHandle>,
}

/// Per-source acquisition, clock synchronization, protocol decoding, and storage ingestion pipeline (`TB6`).
pub struct SourceSession {
    source_id: SourceId,
    display_name: String,
    scheduler: TransportScheduler,
    clock: Arc<Mutex<StreamClockSynchronizer>>,
    codec: Arc<Mutex<Box<dyn ProtocolCodec>>>,
    protocol_name: &'static str,
    declared_descriptors: Vec<ChannelDescriptor>,
    channels: Vec<Arc<ChannelEntry>>,
    hub: Arc<ChannelHub>,
    telemetry: Arc<WorkerTelemetryState>,
    worker_handle: Option<ThreadJoinHandle<()>>,
    sim_control: Option<SimControlHandle>,
    replay_control: Option<ReplayControlHandle>,
    active_recorder: Option<(PathBuf, Arc<RawByteRecorder>)>,
}

impl std::fmt::Debug for SourceSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceSession")
            .field("source_id", &self.source_id)
            .field("display_name", &self.display_name)
            .field("transport_kind", &self.transport_kind())
            .field("protocol_name", &self.protocol_name)
            .field("state", &self.runtime_state())
            .field("channel_count", &self.channels.len())
            .finish()
    }
}

impl SourceSession {
    /// Creates a new [`SourceSession`] from `config` using built-in transport and protocol factories,
    /// registering its declared channels in `hub`.
    pub fn new(config: SourceConfig, hub: Arc<ChannelHub>) -> CoreResult<Self> {
        let transport_reg = TransportRegistry::with_builtin_transports();
        let protocol_reg = ProtocolRegistry::with_builtin_codecs();
        Self::with_registries(config, hub, &transport_reg, &protocol_reg)
    }

    /// Creates a new [`SourceSession`] using explicit [`TransportRegistry`] and [`ProtocolRegistry`] instances.
    pub fn with_registries(
        config: SourceConfig,
        hub: Arc<ChannelHub>,
        transport_registry: &TransportRegistry,
        protocol_registry: &ProtocolRegistry,
    ) -> CoreResult<Self> {
        if config.source_id.as_str().trim().is_empty() {
            return Err(CoreError::InvalidConfig(
                "SourceConfig source_id must not be empty".into(),
            ));
        }

        let (transport, sim_control, replay_control): (
            Box<dyn Transport>,
            Option<SimControlHandle>,
            Option<ReplayControlHandle>,
        ) = match config.transport {
            SourceTransportConfig::Sim(sim_cfg) => {
                let sim = SimSignalTransport::new(sim_cfg)?;
                let ctrl = sim.control_handle();
                (Box::new(sim), Some(ctrl), None)
            }
            SourceTransportConfig::Serial(ser_cfg) => {
                let ser = SerialTransport::new(ser_cfg)?;
                (Box::new(ser), None, None)
            }
            SourceTransportConfig::Udp(udp_cfg) => {
                let udp = UdpTransport::new(udp_cfg)?;
                (Box::new(udp), None, None)
            }
            SourceTransportConfig::TcpClient(tcp_cfg) => {
                let tcp = TcpClientTransport::new(tcp_cfg)?;
                (Box::new(tcp), None, None)
            }
            SourceTransportConfig::TcpServer(srv_cfg) => {
                let srv = TcpServerTransport::new(srv_cfg)?;
                (Box::new(srv), None, None)
            }
            SourceTransportConfig::RawFileReplay(rep_cfg) => {
                let rep = RawFileReplayTransport::new(rep_cfg)?;
                let ctrl = rep.control_handle();
                (Box::new(rep), None, Some(ctrl))
            }
            SourceTransportConfig::Custom {
                transport_kind,
                json_config,
            } => {
                let t = transport_registry.create(transport_kind, &json_config)?;
                (t, None, None)
            }
        };

        let clock = StreamClockSynchronizer::with_config(config.clock_mode, config.clock_config)?;

        let codec: Box<dyn ProtocolCodec> = match config.protocol {
            SourceProtocolConfig::SimPreset(mut p_cfg) => {
                p_cfg.source_id = config.source_id.as_str().to_owned();
                Box::new(SimPresetCodec::new(p_cfg)?)
            }
            SourceProtocolConfig::JustFloat(mut jf_cfg) => {
                jf_cfg.source_id = config.source_id.as_str().to_owned();
                Box::new(JustFloatCodec::new(jf_cfg)?)
            }
            SourceProtocolConfig::FixedSchemaBinary(mut fb_cfg) => {
                fb_cfg.source_id = config.source_id.as_str().to_owned();
                Box::new(FixedSchemaBinaryCodec::new(fb_cfg)?)
            }
            SourceProtocolConfig::FireWater(mut fw_cfg) => {
                fw_cfg.source_id = config.source_id.as_str().to_owned();
                Box::new(FireWaterCsvCodec::new(fw_cfg)?)
            }
            SourceProtocolConfig::Custom {
                codec_id,
                json_config,
            } => protocol_registry.create(&codec_id, &json_config)?,
        };

        Self::from_components(
            SourceSessionComponents {
                source_id: config.source_id,
                display_name: config.display_name,
                transport,
                clock,
                codec,
                runtime_config: config.runtime,
                sim_control,
                replay_control,
            },
            hub,
        )
    }

    /// Constructs a [`SourceSession`] from pre-built transport, clock, and codec instances,
    /// immediately registering its declared channels under `<source_id>/<local_path>` in `hub`.
    pub fn from_components(
        components: SourceSessionComponents,
        hub: Arc<ChannelHub>,
    ) -> CoreResult<Self> {
        let source_id = components.source_id;
        if source_id.as_str().trim().is_empty() {
            return Err(CoreError::InvalidConfig(
                "SourceSession source_id must not be empty".into(),
            ));
        }

        let display_name = components
            .display_name
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| source_id.as_str().to_owned());

        let protocol_name = components.codec.protocol_name();
        let declared_descriptors: Vec<ChannelDescriptor> = components
            .codec
            .declared_channels()
            .iter()
            .cloned()
            .map(|mut d| {
                d.source_id = source_id.clone();
                d
            })
            .collect();

        let channels = hub.register_channels(&declared_descriptors)?;
        let scheduler = TransportScheduler::new(components.transport, components.runtime_config);

        Ok(Self {
            source_id,
            display_name,
            scheduler,
            clock: Arc::new(Mutex::new(components.clock)),
            codec: Arc::new(Mutex::new(components.codec)),
            protocol_name,
            declared_descriptors,
            channels,
            hub,
            telemetry: Arc::new(WorkerTelemetryState::new()),
            worker_handle: None,
            sim_control: components.sim_control,
            replay_control: components.replay_control,
            active_recorder: None,
        })
    }

    /// Returns this session's unique [`SourceId`].
    #[inline]
    pub fn source_id(&self) -> &SourceId {
        &self.source_id
    }

    /// Returns this session's human-readable display name.
    #[inline]
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// Returns the static [`TransportKind`] of the underlying transport.
    #[inline]
    pub fn transport_kind(&self) -> TransportKind {
        self.scheduler.capabilities().kind
    }

    /// Returns static capability flags of the underlying transport.
    #[inline]
    pub fn capabilities(&self) -> TransportCaps {
        self.scheduler.capabilities()
    }

    /// Returns the static protocol name of the attached [`ProtocolCodec`].
    #[inline]
    pub fn protocol_name(&self) -> &'static str {
        self.protocol_name
    }

    /// Returns the configured [`ClockMode`].
    #[inline]
    pub fn clock_mode(&self) -> ClockMode {
        self.clock.lock().mode()
    }

    /// Returns the registered [`ChannelEntry`] handles owned by this source session.
    #[inline]
    pub fn channels(&self) -> &[Arc<ChannelEntry>] {
        &self.channels
    }

    /// Returns the numeric [`ChannelId`]s owned by this source session.
    pub fn channel_ids(&self) -> Vec<ChannelId> {
        self.channels.iter().map(|c| c.id()).collect()
    }

    /// Returns a reference to the shared [`ChannelHub`].
    #[inline]
    pub fn hub(&self) -> &Arc<ChannelHub> {
        &self.hub
    }

    /// Returns the [`SimControlHandle`] if this session uses a `SimSignalTransport`.
    #[inline]
    pub fn sim_control(&self) -> Option<&SimControlHandle> {
        self.sim_control.as_ref()
    }

    /// Returns the [`ReplayControlHandle`] if this session uses a `RawFileReplayTransport`.
    #[inline]
    pub fn replay_control(&self) -> Option<&ReplayControlHandle> {
        self.replay_control.as_ref()
    }

    /// Returns the currently active bypass raw recording file path, if enabled.
    #[inline]
    pub fn raw_recording_path(&self) -> Option<&Path> {
        self.active_recorder.as_ref().map(|(p, _)| p.as_path())
    }

    /// Returns the current [`SourceRuntimeState`] of this session.
    pub fn runtime_state(&self) -> SourceRuntimeState {
        if self.telemetry.worker_error.load(Ordering::Acquire) {
            return SourceRuntimeState::Error;
        }
        let sched_state = self.scheduler.state();
        if sched_state == TransportState::Error {
            return SourceRuntimeState::Error;
        }
        if self.telemetry.worker_active.load(Ordering::Acquire) {
            return match sched_state {
                TransportState::Connecting => SourceRuntimeState::Connecting,
                TransportState::Reconnecting => SourceRuntimeState::Reconnecting,
                _ => SourceRuntimeState::Running,
            };
        }
        if self.telemetry.worker_completed.load(Ordering::Acquire) {
            return SourceRuntimeState::Completed;
        }
        match sched_state {
            TransportState::Stopped => SourceRuntimeState::Stopped,
            TransportState::Connecting => SourceRuntimeState::Connecting,
            TransportState::Running => SourceRuntimeState::Running,
            TransportState::Reconnecting => SourceRuntimeState::Reconnecting,
            TransportState::Error => SourceRuntimeState::Error,
        }
    }

    /// Returns `true` if either the transport IO task or protocol worker thread is currently active.
    #[inline]
    pub fn is_running(&self) -> bool {
        self.scheduler.is_running() || self.telemetry.worker_active.load(Ordering::Acquire)
    }

    /// Builds a [`SourceSummary`] snapshot of this session.
    pub fn summary(&self) -> SourceSummary {
        SourceSummary {
            source_id: self.source_id.clone(),
            display_name: self.display_name.clone(),
            transport_kind: self.transport_kind(),
            state: self.runtime_state(),
            raw_recording_path: self.active_recorder.as_ref().map(|(p, _)| p.clone()),
        }
    }

    /// Starts the transport IO task on the current Tokio runtime and spawns the dedicated
    /// per-source protocol & clock worker thread.
    pub fn start(&mut self) -> CoreResult<SourceSummary> {
        let handle = tokio::runtime::Handle::try_current().map_err(|_| {
            CoreError::Transport(
                "SourceSession::start requires an active Tokio runtime (use start_on outside async context)"
                    .into(),
            )
        })?;
        self.start_on(&handle)
    }

    /// Starts the transport IO task on `handle` and spawns the dedicated per-source worker thread.
    pub fn start_on(&mut self, handle: &tokio::runtime::Handle) -> CoreResult<SourceSummary> {
        if self.is_running() {
            return Err(CoreError::Transport(format!(
                "source `{}` is already running",
                self.source_id
            )));
        }

        if let Some(prev_join) = self.worker_handle.take() {
            let _ = prev_join.join();
        }

        // Ensure channels remain registered in ChannelHub (in case they were removed while stopped)
        self.channels = self.hub.register_channels(&self.declared_descriptors)?;

        {
            let mut codec = self.codec.lock();
            codec.reset_state();
        }
        {
            let mut clock = self.clock.lock();
            clock.reanchor_preserve_monotonic();
        }

        let current_samples = self.telemetry.decoded_samples.load(Ordering::Relaxed);
        let current_bytes = self.scheduler.metrics_snapshot().rx_bytes;
        self.telemetry.mark_started(current_samples, current_bytes);

        let rx = match self.scheduler.start_on(handle) {
            Ok(rx) => rx,
            Err(err) => {
                self.telemetry.worker_active.store(false, Ordering::Release);
                self.telemetry.worker_error.store(true, Ordering::Release);
                *self.telemetry.last_error_msg.lock() = Some(err.to_string());
                return Err(err);
            }
        };

        let worker_join = self.spawn_worker_thread(rx)?;
        self.worker_handle = Some(worker_join);

        Ok(self.summary())
    }

    fn spawn_worker_thread(&self, rx: PacketReceiver) -> CoreResult<ThreadJoinHandle<()>> {
        let source_id = self.source_id.clone();
        let ctx = WorkerPipelineCtx {
            codec: Arc::clone(&self.codec),
            clock: Arc::clone(&self.clock),
            hub: Arc::clone(&self.hub),
            entries: self.channels.clone(),
            descs: self.declared_descriptors.clone(),
            telemetry: Arc::clone(&self.telemetry),
            raw_tap: Arc::clone(self.scheduler.raw_tap()),
            sched_metrics: Arc::clone(self.scheduler.metrics()),
        };

        std::thread::Builder::new()
            .name(format!("ddscope-worker-{source_id}"))
            .spawn(move || {
                run_source_worker_loop(rx, ctx);
            })
            .map_err(|e| {
                CoreError::Transport(format!(
                    "failed to spawn worker thread for source `{source_id}`: {e}"
                ))
            })
    }

    /// Gracefully stops the transport IO task, drains all remaining packets through the
    /// protocol worker into `ChannelHub`, and flushes any active raw byte recorder.
    pub async fn stop(&mut self) -> CoreResult<SourceSummary> {
        self.telemetry.stop_requested.store(true, Ordering::Release);
        self.telemetry
            .worker_completed
            .store(false, Ordering::Release);

        let io_res = self.scheduler.stop().await;

        if let Some(join_handle) = self.worker_handle.take() {
            let _ = tokio::task::spawn_blocking(move || join_handle.join()).await;
        }
        self.telemetry.worker_active.store(false, Ordering::Release);

        io_res?;
        Ok(self.summary())
    }

    /// Synchronously stops the session using the provided Tokio runtime `handle`.
    pub fn stop_blocking(&mut self, handle: &tokio::runtime::Handle) -> CoreResult<SourceSummary> {
        self.telemetry.stop_requested.store(true, Ordering::Release);
        self.telemetry
            .worker_completed
            .store(false, Ordering::Release);

        let io_res = handle.block_on(self.scheduler.stop());
        if let Some(join_handle) = self.worker_handle.take() {
            let _ = join_handle.join();
        }
        self.telemetry.worker_active.store(false, Ordering::Release);

        io_res?;
        Ok(self.summary())
    }

    /// Waits until a finite source (e.g. offline `RawFileReplay` or bounded `Sim`) finishes
    /// streaming and its worker commits all packets to `ChannelHub`.
    pub async fn wait_for_completion(&self, timeout: Duration) -> CoreResult<()> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match self.runtime_state() {
                SourceRuntimeState::Completed | SourceRuntimeState::Stopped => return Ok(()),
                SourceRuntimeState::Error => {
                    let msg = self
                        .telemetry
                        .last_error_msg
                        .lock()
                        .clone()
                        .unwrap_or_else(|| "source session entered Error state".into());
                    return Err(CoreError::Transport(msg));
                }
                _ => {
                    if tokio::time::Instant::now() >= deadline {
                        return Err(CoreError::Transport(format!(
                            "timed out waiting for source `{}` completion",
                            self.source_id
                        )));
                    }
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            }
        }
    }

    /// Enables or disables bypass raw byte recording (`.ddraw` / `.bin`) on this session.
    pub fn set_raw_recorder(
        &mut self,
        enabled: bool,
        output_path: Option<impl AsRef<Path>>,
    ) -> CoreResult<SourceSummary> {
        if enabled {
            let path_ref = output_path.ok_or_else(|| {
                CoreError::InvalidConfig(
                    "output_path is required when enabling raw byte recorder".into(),
                )
            })?;
            let path_buf = path_ref.as_ref().to_path_buf();
            if path_buf.as_os_str().is_empty() {
                return Err(CoreError::InvalidConfig(
                    "raw recorder output_path must not be empty".into(),
                ));
            }

            // Flush any previous recorder before attaching the new one
            if let Some((_, old_rec)) = self.active_recorder.take() {
                let _ = self.scheduler.set_raw_tap(None);
                let _ = old_rec.stop_and_flush();
            }

            let recorder = Arc::new(RawByteRecorder::open(&path_buf)?);
            self.scheduler
                .set_raw_tap(Some(Arc::clone(&recorder) as Arc<dyn RawByteTap>))?;
            self.active_recorder = Some((path_buf, recorder));
        } else {
            let _ = self.scheduler.set_raw_tap(None);
            if let Some((_, old_rec)) = self.active_recorder.take() {
                old_rec.stop_and_flush()?;
            }
        }

        Ok(self.summary())
    }

    /// Disables raw byte bypass recording and flushes/closes any open recording file.
    pub fn disable_raw_recorder(&mut self) -> CoreResult<SourceSummary> {
        self.set_raw_recorder(false, None::<&Path>)
    }

    /// Seeks or updates playback pause/speed state on a `RawFileReplay` source,
    /// resetting protocol framing and re-anchoring clock state without regressing timestamps.
    pub fn seek_replay(
        &self,
        offset_ratio: Option<f64>,
        paused: Option<bool>,
        playback_speed: Option<f64>,
    ) -> CoreResult<()> {
        let ctrl = self.replay_control.as_ref().ok_or_else(|| {
            CoreError::Unsupported(format!(
                "source `{}` ({:?}) does not support replay seek/pacing",
                self.source_id,
                self.transport_kind()
            ))
        })?;

        if let Some(speed) = playback_speed {
            ctrl.set_playback_speed(speed)?;
        }
        if let Some(p) = paused {
            ctrl.set_paused(p);
        }
        if let Some(ratio) = offset_ratio {
            if !ratio.is_finite() {
                return Err(CoreError::InvalidConfig(
                    "seek offset_ratio must be finite".into(),
                ));
            }
            self.telemetry
                .seek_reset_requested
                .store(true, Ordering::Release);
            ctrl.seek(ratio);
        }
        Ok(())
    }

    /// Encodes a logical [`DownlinkRequest`] via the session's [`ProtocolCodec`] and enqueues it
    /// into the coalescing TX queue.
    pub fn send_downlink(&self, req: &DownlinkRequest) -> CoreResult<TxDispatchReceipt> {
        if !self.capabilities().supports_tx {
            return Err(CoreError::Unsupported(format!(
                "source `{}` ({:?}) does not support outbound TX",
                self.source_id,
                self.transport_kind()
            )));
        }

        let mut encoded = BytesMut::with_capacity(64);
        {
            let mut codec = self.codec.lock();
            codec.encode_downlink(&req.payload, &mut encoded)?;
        }

        let encoded_bytes = encoded.len();
        let mut frame = match &req.coalesce_key {
            Some(key) => {
                OutboundFrame::coalesced_with_priority(encoded.freeze(), key.clone(), req.priority)
            }
            None => OutboundFrame::fire_and_forget(encoded.freeze(), req.priority),
        };
        if let Some(tx_id) = req.transaction_id {
            frame = frame.with_transaction_id(tx_id);
        }

        let receipt = self.scheduler.submit_tx(frame)?;
        Ok(TxDispatchReceipt {
            accepted: true,
            coalesced_existing: receipt.coalesced_existing,
            queue_len: receipt.queue_len,
            effective_priority: receipt.effective_priority,
            encoded_bytes,
            result: TransactionResult::SentFireAndForget,
        })
    }

    /// Submits a pre-encoded [`OutboundFrame`] directly into the session's coalescing TX queue.
    pub fn submit_raw_tx(&self, frame: OutboundFrame) -> CoreResult<TxDispatchReceipt> {
        let encoded_bytes = frame.payload.len();
        let receipt = self.scheduler.submit_tx(frame)?;
        Ok(TxDispatchReceipt {
            accepted: true,
            coalesced_existing: receipt.coalesced_existing,
            queue_len: receipt.queue_len,
            effective_priority: receipt.effective_priority,
            encoded_bytes,
            result: TransactionResult::SentFireAndForget,
        })
    }

    /// Returns raw transport IO metrics (`rx_packets`, `rx_bytes`, `dropped_packets`, `tx_*`).
    #[inline]
    pub fn transport_metrics(&self) -> TransportMetricsSnapshot {
        self.scheduler.metrics_snapshot()
    }

    /// Returns a snapshot of the hybrid clock/PLL synchronizer diagnostics.
    #[inline]
    pub fn clock_diagnostics(&self) -> ClockSyncDiagnostics {
        self.clock.lock().diagnostics()
    }

    /// Returns the cumulative number of decoded sample rows committed to `ChannelHub` by this session.
    #[inline]
    pub fn total_decoded_samples(&self) -> u64 {
        self.telemetry.decoded_samples.load(Ordering::Relaxed)
    }

    /// Returns a complete [`SourceTelemetrySnapshot`] combining transport, protocol, and clock metrics.
    pub fn telemetry_snapshot(&self) -> SourceTelemetrySnapshot {
        let m = self.scheduler.metrics_snapshot();
        let total_samples = self.telemetry.decoded_samples.load(Ordering::Relaxed);
        let total_batches = self.telemetry.decoded_batches.load(Ordering::Relaxed);
        let decode_errors = self.telemetry.decode_errors.load(Ordering::Relaxed);
        let state = self.runtime_state();
        let is_active = state == SourceRuntimeState::Running;
        let (samples_per_sec, bytes_per_sec) =
            self.telemetry
                .compute_rates(total_samples, m.rx_bytes, is_active);

        SourceTelemetrySnapshot {
            source_id: self.source_id.clone(),
            state,
            samples_per_sec,
            bytes_per_sec,
            dropped_packets: m.dropped_packets,
            total_rx_packets: m.rx_packets,
            total_rx_bytes: m.rx_bytes,
            total_decoded_samples: total_samples,
            total_decoded_batches: total_batches,
            decode_errors,
            total_tx_submitted: m.tx_submitted,
            total_tx_coalesced: m.tx_coalesced,
            total_tx_dispatched: m.tx_dispatched,
            clock_diagnostics: self.clock_diagnostics(),
        }
    }
}

impl Drop for SourceSession {
    fn drop(&mut self) {
        self.telemetry.stop_requested.store(true, Ordering::Release);
        let _ = self.scheduler.set_raw_tap(None);
        if let Some((_, rec)) = self.active_recorder.take() {
            let _ = rec.stop_and_flush();
        }
    }
}

struct WorkerPipelineCtx {
    codec: Arc<Mutex<Box<dyn ProtocolCodec>>>,
    clock: Arc<Mutex<StreamClockSynchronizer>>,
    hub: Arc<ChannelHub>,
    entries: Vec<Arc<ChannelEntry>>,
    descs: Vec<ChannelDescriptor>,
    telemetry: Arc<WorkerTelemetryState>,
    raw_tap: SharedRawTap,
    sched_metrics: Arc<TransportMetrics>,
}

fn run_source_worker_loop(rx: PacketReceiver, ctx: WorkerPipelineCtx) {
    let mut batch = DecodedBatch::from_descriptors(&ctx.descs, 4_096);

    loop {
        if ctx.telemetry.seek_reset_requested.swap(false, Ordering::AcqRel) {
            ctx.codec.lock().reset_state();
            ctx.clock.lock().reanchor_preserve_monotonic();
            ctx.telemetry.has_prev_raw_recv.store(false, Ordering::Release);
        }

        let first_pkt = match rx.recv_timeout(Duration::from_millis(5)) {
            Ok(pkt) => pkt,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                if ctx.telemetry.stop_requested.load(Ordering::Acquire) && rx.is_empty() {
                    break;
                }
                continue;
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                if !ctx.telemetry.stop_requested.load(Ordering::Acquire) {
                    ctx.telemetry.worker_completed.store(true, Ordering::Release);
                }
                break;
            }
        };

        process_single_packet(first_pkt, &ctx, &mut batch);

        // Drain up to 32 immediately available packets in the ring before re-checking control flags
        for _ in 0..32 {
            if ctx.telemetry.seek_reset_requested.swap(false, Ordering::AcqRel) {
                ctx.codec.lock().reset_state();
                ctx.clock.lock().reanchor_preserve_monotonic();
                ctx.telemetry.has_prev_raw_recv.store(false, Ordering::Release);
            }
            match rx.try_recv() {
                Ok(pkt) => {
                    process_single_packet(pkt, &ctx, &mut batch);
                }
                Err(_) => break,
            }
        }
    }

    ctx.telemetry.worker_active.store(false, Ordering::Release);
}

fn process_single_packet(
    pkt: crate::transport::RawPacket,
    ctx: &WorkerPipelineCtx,
    batch: &mut DecodedBatch,
) {
    let raw_recv_ns = pkt.host_recv_ns;
    ctx.sched_metrics.record_rx(pkt.bytes.len());
    {
        let tap_guard = ctx.raw_tap.load();
        if let Some(ref tap) = **tap_guard {
            tap.on_raw_bytes(RawTapDirection::Rx, raw_recv_ns, &pkt.bytes);
        }
    }

    if ctx.telemetry.has_prev_raw_recv.load(Ordering::Acquire) {
        let prev_raw = ctx.telemetry.prev_raw_recv_ns.load(Ordering::Relaxed);
        if raw_recv_ns.saturating_add(1_000_000) < prev_raw {
            ctx.codec.lock().reset_state();
            ctx.clock.lock().reanchor_preserve_monotonic();
        }
    }
    ctx.telemetry
        .prev_raw_recv_ns
        .store(raw_recv_ns, Ordering::Relaxed);
    ctx.telemetry.has_prev_raw_recv.store(true, Ordering::Release);

    batch.clear();
    batch.packet_recv_ns = raw_recv_ns;
    let feed_res = {
        let mut c = ctx.codec.lock();
        c.feed_bytes(&pkt.bytes, raw_recv_ns, batch)
    };
    if batch.packet_recv_ns == 0 && raw_recv_ns != 0 {
        batch.packet_recv_ns = raw_recv_ns;
    }

    let rows = match feed_res {
        Ok(r) => r,
        Err(CoreError::Protocol {
            recoverable: true, ..
        }) => {
            ctx.telemetry.decode_errors.fetch_add(1, Ordering::Relaxed);
            return;
        }
        Err(err) => {
            ctx.telemetry.decode_errors.fetch_add(1, Ordering::Relaxed);
            ctx.telemetry.worker_error.store(true, Ordering::Release);
            *ctx.telemetry.last_error_msg.lock() = Some(err.to_string());
            return;
        }
    };

    if !batch.ack_events.is_empty() {
        ctx.telemetry
            .ack_events_received
            .fetch_add(batch.ack_events.len() as u64, Ordering::Relaxed);
    }

    if rows == 0 {
        return;
    }

    let ts_res = {
        let mut clk = ctx.clock.lock();
        clk.populate_batch_timestamps(batch)
    };
    if let Err(err) = ts_res {
        ctx.telemetry.decode_errors.fetch_add(1, Ordering::Relaxed);
        ctx.telemetry.worker_error.store(true, Ordering::Release);
        *ctx.telemetry.last_error_msg.lock() = Some(err.to_string());
        return;
    }

    match ctx.hub.ingest_decoded_batch(&ctx.entries, batch) {
        Ok(committed) => {
            ctx.telemetry
                .decoded_samples
                .fetch_add(committed as u64, Ordering::Relaxed);
            ctx.telemetry.decoded_batches.fetch_add(1, Ordering::Relaxed);
            if let Some(&last_ts) = batch.timestamps_ns.last() {
                ctx.telemetry
                    .last_packet_recv_ns
                    .store(last_ts, Ordering::Relaxed);
                ctx.telemetry
                    .last_sample_ts_ns
                    .store(last_ts, Ordering::Relaxed);
            }
        }
        Err(err) => {
            ctx.telemetry.decode_errors.fetch_add(1, Ordering::Relaxed);
            ctx.telemetry.worker_error.store(true, Ordering::Release);
            *ctx.telemetry.last_error_msg.lock() = Some(err.to_string());
        }
    }
}
