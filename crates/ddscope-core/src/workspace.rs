//! Top-level multi-source session pipeline (`SourceSession`) and workspace orchestration (`CoreWorkspace`).
//!
//! - [`SourceSession`] (`TB6`) integrates [`TransportScheduler`], [`StreamClockSynchronizer`],
//!   [`ProtocolCodec`], and [`ChannelHub`].
//! - [`CoreWorkspace`] (`T04`) orchestrates multiple concurrent `SourceSession`s, independent
//!   [`ScopeController`] viewports, [`SyncGroupCoordinator`] timebase synchronization, automatic
//!   [`PinnedGuard`] chunk retention, M4-accelerated trigger scanning, and 60/120Hz `DDSF v1` packing.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle as ThreadJoinHandle;
use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};

use crate::clock::{ClockMode, ClockSyncConfig, ClockSyncDiagnostics, StreamClockSynchronizer};
use crate::protocol::{
    encode_hex_string, DownlinkPayload, FireWaterConfig, FireWaterCsvCodec, FixedSchemaBinaryCodec,
    FixedSchemaBinaryConfig, JustFloatCodec, JustFloatConfig, ProtocolCodec, ProtocolRegistry,
    SimPresetCodec, SimPresetConfig, TransactionResult,
};
use crate::scope::{
    DdsfFramePacker, EdgeSlope, HysteresisBandState, ScopeController, ScopeControllerConfig,
    ScopeStepOutcome, ScopeViewportPayload, ScopeViewportWindow, SyncGroupCoordinator,
    SyncGroupState, TrackPayload, TriggerCondition, TriggerRuntimeState, TriggerSweepMode,
    DDSF_FLAG_PINNED_ACTIVE, DDSF_FLAG_QUOTA_WARNING,
};
use crate::storage::{
    ChannelEntry, ChannelHub, ChannelStore, M4QueryResult, MemoryQuotaManager, MemoryQuotaStats,
    NativeScalarSeries, PinnedGuard, ViewportExtract,
};
use crate::transport::{
    list_serial_ports, OutboundFrame, PacketReceiver, PendingSchedulerStop, RawByteRecorder,
    RawByteTap, RawFileReplayConfig, RawFileReplayTransport, RawTapDirection, ReplayControlHandle,
    SerialConfig, SerialPortInfo, SerialTransport, SharedRawTap, SimConfig, SimControlHandle,
    SimSignalTransport, TcpClientConfig, TcpClientTransport, TcpServerConfig, TcpServerTransport,
    Transport, TransportCaps, TransportKind, TransportMetrics, TransportMetricsSnapshot,
    TransportRegistry, TransportRuntimeConfig, TransportScheduler, TransportState, TxPriority,
    UdpConfig, UdpTransport,
};
use crate::types::{
    ChannelDescriptor, ChannelId, ChannelKind, CoreError, CoreResult, DataChunk, DecodedBatch,
    M4Bucket, M4_BRANCH_FACTOR, ScalarValue, ScopeId, SourceId, SyncGroupId, TimestampNs,
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
                counter_bits: 32,
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

    /// Detached pending shutdown handle allowing [`CoreWorkspace`] to release session locks
    /// before awaiting async IO and blocking worker thread join.
    pub(crate) fn begin_stop(&mut self) -> PendingSourceStop {
        self.telemetry.stop_requested.store(true, Ordering::Release);
        self.telemetry
            .worker_completed
            .store(false, Ordering::Release);
        PendingSourceStop {
            scheduler_stop: self.scheduler.begin_stop(),
            worker_handle: self.worker_handle.take(),
            telemetry: Arc::clone(&self.telemetry),
        }
    }

    /// Gracefully stops the transport IO task, drains all remaining packets through the
    /// protocol worker into `ChannelHub`, and flushes any active raw byte recorder.
    pub async fn stop(&mut self) -> CoreResult<SourceSummary> {
        self.begin_stop().finish().await?;
        Ok(self.summary())
    }

    /// Synchronously stops the session using the provided Tokio runtime `handle`.
    pub fn stop_blocking(&mut self, handle: &tokio::runtime::Handle) -> CoreResult<SourceSummary> {
        self.begin_stop().finish_blocking(handle)?;
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

/// Detached stop handle allowing [`CoreWorkspace`] to release session mutexes before
/// awaiting transport scheduler shutdown and worker thread termination.
#[derive(Debug)]
pub(crate) struct PendingSourceStop {
    scheduler_stop: PendingSchedulerStop,
    worker_handle: Option<ThreadJoinHandle<()>>,
    telemetry: Arc<WorkerTelemetryState>,
}

impl PendingSourceStop {
    pub(crate) async fn finish(self) -> CoreResult<()> {
        let sched_res = self.scheduler_stop.wait().await;
        self.telemetry.stop_requested.store(true, Ordering::Release);
        if let Some(join) = self.worker_handle {
            let join_res = tokio::task::spawn_blocking(move || join.join()).await;
            match join_res {
                Ok(Ok(())) => {}
                Ok(Err(_panic)) => {
                    return Err(CoreError::Transport(
                        "source worker thread panicked during shutdown".into(),
                    ));
                }
                Err(join_err) => {
                    return Err(CoreError::Transport(format!(
                        "failed waiting for source worker shutdown: {join_err}"
                    )));
                }
            }
        }
        self.telemetry.worker_active.store(false, Ordering::Release);
        sched_res
    }

    pub(crate) fn finish_blocking(self, handle: &tokio::runtime::Handle) -> CoreResult<()> {
        let sched_res = handle.block_on(self.scheduler_stop.wait());
        self.telemetry.stop_requested.store(true, Ordering::Release);
        if let Some(join) = self.worker_handle {
            if join.join().is_err() {
                return Err(CoreError::Transport(
                    "source worker thread panicked during shutdown".into(),
                ));
            }
        }
        self.telemetry.worker_active.store(false, Ordering::Release);
        sched_res
    }
}

const DEFAULT_WORKSPACE_QUOTA_BYTES: usize = 256 * 1024 * 1024;
const DEFAULT_SCOPE_WIDTH_PX: u32 = 1280;
const DEFAULT_DDSF_PACKER_CAPACITY: usize = 256 * 1024;

#[inline]
fn default_scope_width_px() -> u32 {
    DEFAULT_SCOPE_WIDTH_PX
}

#[inline]
const fn default_true() -> bool {
    true
}

/// Visual lane representation for a track bound inside a scope viewport.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeLaneKind {
    #[default]
    Analog,
    Digital,
    StateRibbon,
}

/// Binding connecting a [`ChannelId`] to a visual lane inside a [`CoreWorkspace`] scope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeTrackBinding {
    pub channel_id: ChannelId,
    #[serde(default)]
    pub lane_kind: ScopeLaneKind,
    #[serde(default)]
    pub color_rgba: Option<u32>,
    #[serde(default)]
    pub label: Option<String>,
}

impl ScopeTrackBinding {
    /// Creates an analog waveform track binding.
    pub fn analog(channel_id: ChannelId) -> Self {
        Self {
            channel_id,
            lane_kind: ScopeLaneKind::Analog,
            color_rgba: None,
            label: None,
        }
    }

    /// Creates a digital edge lane track binding.
    pub fn digital(channel_id: ChannelId) -> Self {
        Self {
            channel_id,
            lane_kind: ScopeLaneKind::Digital,
            color_rgba: None,
            label: None,
        }
    }

    /// Creates a discrete state-ribbon track binding.
    pub fn state_ribbon(channel_id: ChannelId) -> Self {
        Self {
            channel_id,
            lane_kind: ScopeLaneKind::StateRibbon,
            color_rgba: None,
            label: None,
        }
    }

    /// Attaches an RGBA color override.
    pub fn with_color_rgba(mut self, color_rgba: u32) -> Self {
        self.color_rgba = Some(color_rgba);
        self
    }

    /// Attaches a custom legend label override.
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
}

/// Configuration for registering or updating a scope instance inside [`CoreWorkspace`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScopeConfig {
    pub scope_id: ScopeId,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub controller: ScopeControllerConfig,
    #[serde(default)]
    pub tracks: Vec<ScopeTrackBinding>,
    #[serde(default)]
    pub sync_group_id: Option<SyncGroupId>,
    #[serde(default = "default_scope_width_px")]
    pub width_px: u32,
    #[serde(default = "default_true")]
    pub auto_pin_on_latch: bool,
}

impl ScopeConfig {
    /// Creates a [`ScopeConfig`] with default pixel width (`1280px`) and auto-pin enabled.
    pub fn new(scope_id: impl Into<ScopeId>, controller: ScopeControllerConfig) -> Self {
        Self {
            scope_id: scope_id.into(),
            title: None,
            controller,
            tracks: Vec::new(),
            sync_group_id: None,
            width_px: DEFAULT_SCOPE_WIDTH_PX,
            auto_pin_on_latch: true,
        }
    }

    /// Sets the display title.
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Replaces the bound tracks.
    pub fn with_tracks(mut self, tracks: Vec<ScopeTrackBinding>) -> Self {
        self.tracks = tracks;
        self
    }

    /// Assigns this scope to a [`SyncGroupId`].
    pub fn with_sync_group(mut self, sync_group_id: impl Into<SyncGroupId>) -> Self {
        self.sync_group_id = Some(sync_group_id.into());
        self
    }

    /// Sets the target horizontal viewport resolution in pixels (clamped to `>= 1`).
    pub fn with_width_px(mut self, width_px: u32) -> Self {
        self.width_px = width_px.max(1);
        self
    }

    /// Controls automatic chunk pinning when the scope latches on a trigger.
    pub fn with_auto_pin_on_latch(mut self, enabled: bool) -> Self {
        self.auto_pin_on_latch = enabled;
        self
    }
}

/// Public summary snapshot of a registered scope inside [`CoreWorkspace`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScopeSummary {
    pub scope_id: ScopeId,
    pub title: String,
    pub runtime_state: TriggerRuntimeState,
    pub sweep_mode: TriggerSweepMode,
    pub hysteresis_state: HysteresisBandState,
    pub armed_time_ns: TimestampNs,
    pub trigger_time_ns: Option<TimestampNs>,
    pub window: Option<ScopeViewportWindow>,
    pub is_pinned: bool,
    pub pinned_channels: usize,
    pub pinned_bytes: usize,
    pub track_count: usize,
    pub sync_group_id: Option<SyncGroupId>,
    pub width_px: u32,
}

/// Cross-scope timebase synchronization update.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SyncGroupUpdate {
    pub group_id: SyncGroupId,
    pub origin_scope_id: Option<ScopeId>,
    pub time_span_ns: u64,
    pub center_offset_ns: i64,
    pub cursor_a_ratio: Option<f64>,
    pub cursor_b_ratio: Option<f64>,
}

/// Metadata and live sample counters for a single registered channel in [`CoreWorkspace`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RegisteredChannelInfo {
    pub channel_id: ChannelId,
    pub source_id: SourceId,
    pub local_path: String,
    pub display_name: String,
    pub full_path: String,
    pub kind: ChannelKind,
    pub unit: Option<String>,
    pub total_samples: u64,
    pub evicted_samples: u64,
    pub latest_timestamp_ns: Option<TimestampNs>,
    pub latest_value: Option<f64>,
}

/// Channels grouped under a single [`SourceId`] inside [`ChannelTreeSnapshot`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceChannelGroup {
    pub source_id: SourceId,
    pub channels: Vec<RegisteredChannelInfo>,
}

/// Hierarchical channel tree snapshot across all active sources and derived channels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChannelTreeSnapshot {
    pub total_channels: usize,
    pub sources: Vec<SourceChannelGroup>,
}

/// Aggregate telemetry snapshot across all sources, scopes, quota, and frame packing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceTelemetrySnapshot {
    pub sources: Vec<SourceTelemetrySnapshot>,
    pub scopes: Vec<ScopeSummary>,
    pub quota: MemoryQuotaStats,
    pub total_channels: usize,
    pub total_frames_packed: u64,
    pub last_frame_pack_ns: u64,
}

struct ManagedScope {
    config: ScopeConfig,
    controller: ScopeController,
    pinned_guards: HashMap<ChannelId, PinnedGuard>,
    last_scan_abs_sample: Option<u64>,
    pins_acquired_for_latch: bool,
}

impl ManagedScope {
    fn new(mut config: ScopeConfig) -> Self {
        config.width_px = config.width_px.max(1);
        config.controller.scope_id = config.scope_id;
        config.controller.sync_group_id = config.sync_group_id;
        let controller = ScopeController::new(config.controller.clone());
        Self {
            config,
            controller,
            pinned_guards: HashMap::new(),
            last_scan_abs_sample: None,
            pins_acquired_for_latch: false,
        }
    }

    fn summary(&self) -> ScopeSummary {
        let pinned_bytes: usize = self.pinned_guards.values().map(|g| g.pinned_bytes()).sum();
        let current_window = self.controller.current_window();
        let trigger_time_ns = current_window
            .and_then(|w| w.trigger_timestamp_ns)
            .or_else(|| self.controller.active_hit().map(|h| h.timestamp_ns));
        ScopeSummary {
            scope_id: self.config.scope_id,
            title: self
                .config
                .title
                .clone()
                .or_else(|| self.controller.config().title.clone())
                .unwrap_or_else(|| self.config.scope_id.to_string()),
            runtime_state: self.controller.current_state(),
            sweep_mode: self.controller.config().sweep_mode,
            hysteresis_state: self
                .controller
                .trigger_engine()
                .map_or(HysteresisBandState::Unprimed, |e| e.state()),
            armed_time_ns: self.controller.armed_time_ns(),
            trigger_time_ns,
            window: current_window,
            is_pinned: self.controller.is_pinned(),
            pinned_channels: self.pinned_guards.len(),
            pinned_bytes,
            track_count: self.config.tracks.len(),
            sync_group_id: self.config.sync_group_id,
            width_px: self.config.width_px,
        }
    }

    fn clear_pins(&mut self) {
        self.pinned_guards.clear();
        self.pins_acquired_for_latch = false;
    }
}

/// Top-level pure-Rust engine orchestrator coordinating sources, storage, scopes,
/// chunk pinning, sync groups, M4 trigger scanning, and 60/120Hz `DDSF v1` framing.
pub struct CoreWorkspace {
    hub: Arc<ChannelHub>,
    quota_manager: Arc<MemoryQuotaManager>,
    tokio_handle: tokio::runtime::Handle,
    transport_registry: Arc<TransportRegistry>,
    protocol_registry: Arc<ProtocolRegistry>,
    sources: RwLock<HashMap<SourceId, Arc<Mutex<SourceSession>>>>,
    scopes: Mutex<BTreeMap<ScopeId, ManagedScope>>,
    sync_groups: SyncGroupCoordinator,
    frame_seq: AtomicU64,
    last_frame_pack_ns: AtomicU64,
    packer: Mutex<DdsfFramePacker>,
}

impl std::fmt::Debug for CoreWorkspace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoreWorkspace")
            .field("source_count", &self.sources.read().len())
            .field("scope_count", &self.scopes.lock().len())
            .field("quota", &self.hub.quota_stats())
            .field("frame_seq", &self.frame_seq.load(Ordering::Relaxed))
            .finish()
    }
}

impl CoreWorkspace {
    /// Creates a new [`CoreWorkspace`] with a default 256 MiB global chunk memory quota.
    pub fn new(tokio_handle: tokio::runtime::Handle) -> Self {
        Self::with_quota_bytes(tokio_handle, DEFAULT_WORKSPACE_QUOTA_BYTES)
    }

    /// Creates a new [`CoreWorkspace`] enforcing `quota_bytes` across all channels and scopes.
    pub fn with_quota_bytes(tokio_handle: tokio::runtime::Handle, quota_bytes: usize) -> Self {
        let quota_manager = Arc::new(MemoryQuotaManager::new(quota_bytes));
        let hub = Arc::new(ChannelHub::with_quota_manager(Arc::clone(&quota_manager)));
        Self {
            hub,
            quota_manager,
            tokio_handle,
            transport_registry: Arc::new(TransportRegistry::with_builtin_transports()),
            protocol_registry: Arc::new(ProtocolRegistry::with_builtin_codecs()),
            sources: RwLock::new(HashMap::new()),
            scopes: Mutex::new(BTreeMap::new()),
            sync_groups: SyncGroupCoordinator::new(),
            frame_seq: AtomicU64::new(0),
            last_frame_pack_ns: AtomicU64::new(0),
            packer: Mutex::new(DdsfFramePacker::new(DEFAULT_DDSF_PACKER_CAPACITY)),
        }
    }

    /// Returns the shared [`ChannelHub`] storage registry.
    #[inline]
    pub fn hub(&self) -> &Arc<ChannelHub> {
        &self.hub
    }

    /// Returns the global [`MemoryQuotaManager`].
    #[inline]
    pub fn quota_manager(&self) -> &Arc<MemoryQuotaManager> {
        &self.quota_manager
    }

    /// Returns the Tokio runtime handle bound to this workspace.
    #[inline]
    pub fn tokio_handle(&self) -> &tokio::runtime::Handle {
        &self.tokio_handle
    }

    /// Returns a snapshot of global chunk memory quota statistics.
    #[inline]
    pub fn quota_stats(&self) -> MemoryQuotaStats {
        self.hub.quota_stats()
    }

    /// Enforces global quota eviction across unpinned chunks immediately.
    #[inline]
    pub fn enforce_quota(&self) {
        self.hub.enforce_global_quota();
    }

    /// Enumerates OS serial ports currently visible to the host.
    pub fn list_serial_ports(&self) -> CoreResult<Vec<SerialPortInfo>> {
        list_serial_ports()
    }

    /// Registers a new [`SourceSession`] from `config`, optionally auto-starting it.
    pub fn create_source(&self, config: SourceConfig) -> CoreResult<SourceSummary> {
        let source_id = config.source_id.clone();
        if self.sources.read().contains_key(&source_id) {
            return Err(CoreError::SourceAlreadyExists(source_id));
        }

        let auto_start = config.auto_start;
        let mut session = SourceSession::with_registries(
            config,
            Arc::clone(&self.hub),
            &self.transport_registry,
            &self.protocol_registry,
        )?;
        if auto_start {
            session.start_on(&self.tokio_handle)?;
        }
        let summary = session.summary();
        let entry = Arc::new(Mutex::new(session));

        let mut sources = self.sources.write();
        if sources.contains_key(&source_id) {
            // Another concurrent caller registered first; stop our orphan if auto-started.
            drop(sources);
            if let Ok(orphan) = Arc::try_unwrap(entry) {
                let mut orphan_sess = orphan.into_inner();
                let _ = orphan_sess.stop_blocking(&self.tokio_handle);
            }
            return Err(CoreError::SourceAlreadyExists(source_id));
        }
        sources.insert(source_id, entry);
        Ok(summary)
    }

    fn get_source_arc(&self, source_id: &SourceId) -> CoreResult<Arc<Mutex<SourceSession>>> {
        self.sources
            .read()
            .get(source_id)
            .cloned()
            .ok_or_else(|| CoreError::SourceNotFound(source_id.clone()))
    }

    /// Starts (or restarts) a registered source session on the workspace Tokio runtime.
    pub fn start_source(&self, source_id: &SourceId) -> CoreResult<SourceSummary> {
        let arc = self.get_source_arc(source_id)?;
        let mut sess = arc.lock();
        sess.start_on(&self.tokio_handle)
    }

    /// Gracefully stops a source session without holding workspace/session locks across `.await`.
    pub async fn stop_source(&self, source_id: &SourceId) -> CoreResult<SourceSummary> {
        let arc = self.get_source_arc(source_id)?;
        let pending = {
            let mut sess = arc.lock();
            sess.begin_stop()
        };
        pending.finish().await?;
        let sess = arc.lock();
        Ok(sess.summary())
    }

    /// Synchronously stops a source session on `self.tokio_handle` (must not be called directly
    /// from inside a Tokio runtime thread without blocking safeguards).
    pub fn stop_source_blocking(&self, source_id: &SourceId) -> CoreResult<SourceSummary> {
        let arc = self.get_source_arc(source_id)?;
        let pending = {
            let mut sess = arc.lock();
            sess.begin_stop()
        };
        pending.finish_blocking(&self.tokio_handle)?;
        let sess = arc.lock();
        Ok(sess.summary())
    }

    /// Stops (if running) and removes a source session, along with its registered channels.
    pub async fn remove_source(&self, source_id: &SourceId) -> CoreResult<()> {
        let arc = self
            .sources
            .write()
            .remove(source_id)
            .ok_or_else(|| CoreError::SourceNotFound(source_id.clone()))?;
        let pending = {
            let mut sess = arc.lock();
            sess.begin_stop()
        };
        pending.finish().await?;
        self.hub.remove_source_channels(source_id);
        Ok(())
    }

    /// Returns summaries of all registered sources sorted by [`SourceId`].
    pub fn list_sources(&self) -> Vec<SourceSummary> {
        let arcs: Vec<_> = self.sources.read().values().cloned().collect();
        let mut out: Vec<SourceSummary> = arcs.into_iter().map(|a| a.lock().summary()).collect();
        out.sort_by(|a, b| a.source_id.as_str().cmp(b.source_id.as_str()));
        out
    }

    /// Sends a structured downlink command to a running bidirectional source session.
    pub fn send_downlink(
        &self,
        source_id: &SourceId,
        req: &DownlinkRequest,
    ) -> CoreResult<TxDispatchReceipt> {
        let arc = self.get_source_arc(source_id)?;
        let sess = arc.lock();
        sess.send_downlink(req)
    }

    /// Enables or disables bypass raw byte recording (`.ddraw` / `.bin`) on a source session.
    pub fn set_raw_recorder(
        &self,
        source_id: &SourceId,
        enabled: bool,
        output_path: Option<impl AsRef<Path>>,
    ) -> CoreResult<SourceSummary> {
        let arc = self.get_source_arc(source_id)?;
        let mut sess = arc.lock();
        sess.set_raw_recorder(enabled, output_path)
    }

    /// Starts bypass raw-byte `.ddraw` recording on a capable source session.
    pub fn start_raw_recording(
        &self,
        source_id: &SourceId,
        output_path: impl AsRef<Path>,
    ) -> CoreResult<PathBuf> {
        let path_buf = output_path.as_ref().to_path_buf();
        let arc = self.get_source_arc(source_id)?;
        let mut sess = arc.lock();
        sess.set_raw_recorder(true, Some(&path_buf))?;
        Ok(path_buf)
    }

    /// Stops and flushes an active `.ddraw` bypass raw recorder for `source_id`.
    pub fn stop_raw_recording(&self, source_id: &SourceId) -> CoreResult<SourceSummary> {
        let arc = self.get_source_arc(source_id)?;
        let mut sess = arc.lock();
        sess.disable_raw_recorder()
    }

    /// Returns the runtime [`SimControlHandle`] if `source_id` uses `SimSignalTransport`.
    pub fn sim_control(&self, source_id: &SourceId) -> CoreResult<Option<SimControlHandle>> {
        let arc = self.get_source_arc(source_id)?;
        let sess = arc.lock();
        Ok(sess.sim_control().cloned())
    }

    /// Returns the runtime [`ReplayControlHandle`] if `source_id` uses `RawFileReplayTransport`.
    pub fn replay_control(&self, source_id: &SourceId) -> CoreResult<Option<ReplayControlHandle>> {
        let arc = self.get_source_arc(source_id)?;
        let sess = arc.lock();
        Ok(sess.replay_control().cloned())
    }

    /// Seeks or updates playback pause/speed state on a `RawFileReplay` source, resetting
    /// protocol framing and re-anchoring clock state without regressing timestamps.
    pub fn seek_replay_source(
        &self,
        source_id: &SourceId,
        offset_ratio: Option<f64>,
        paused: Option<bool>,
        playback_speed: Option<f64>,
    ) -> CoreResult<()> {
        let arc = self.get_source_arc(source_id)?;
        let sess = arc.lock();
        sess.seek_replay(offset_ratio, paused, playback_speed)
    }

    /// Waits for a finite source (e.g. `RawFileReplay`) to complete streaming.
    pub async fn wait_for_source_completion(
        &self,
        source_id: &SourceId,
        timeout: Duration,
    ) -> CoreResult<()> {
        let arc = self.get_source_arc(source_id)?;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let state = {
                let sess = arc.lock();
                match sess.runtime_state() {
                    SourceRuntimeState::Error => {
                        let msg = sess
                            .telemetry
                            .last_error_msg
                            .lock()
                            .clone()
                            .unwrap_or_else(|| "unknown source worker error".into());
                        return Err(CoreError::Transport(msg));
                    }
                    s => s,
                }
            };
            if matches!(
                state,
                SourceRuntimeState::Completed | SourceRuntimeState::Stopped
            ) {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(CoreError::Transport(format!(
                    "timed out waiting for source `{source_id}` completion after {timeout:?}"
                )));
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// Gracefully shuts down all registered sources (waiting for drains and thread joins).
    pub async fn shutdown(&self) -> CoreResult<()> {
        let pending: Vec<PendingSourceStop> = {
            let mut sources = self.sources.write();
            let mut list = Vec::with_capacity(sources.len());
            for arc in sources.values_mut() {
                list.push(arc.lock().begin_stop());
            }
            list
        };
        let mut first_err = None;
        for stop in pending {
            if let Err(err) = stop.finish().await {
                if first_err.is_none() {
                    first_err = Some(err);
                }
            }
        }
        self.scopes.lock().clear();
        self.sync_groups.clear();
        match first_err {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    /// Resolves a channel entry by `source_id` and local channel path or display name (e.g. `"ch0"`).
    pub fn find_channel(
        &self,
        source_id: &SourceId,
        channel_name: &str,
    ) -> Option<Arc<ChannelEntry>> {
        let expected = format!("{}/{}", source_id.as_str(), channel_name);
        self.hub.channels_for_source(source_id).into_iter().find(|e| {
            let d = e.descriptor();
            e.path().as_str() == expected
                || d.local_path == channel_name
                || d.display_name.as_deref() == Some(channel_name)
        })
    }

    /// Returns a hierarchical snapshot of all sources and registered channels in `ChannelHub`.
    pub fn channel_tree(&self) -> ChannelTreeSnapshot {
        let mut by_source: BTreeMap<String, Vec<RegisteredChannelInfo>> = BTreeMap::new();
        for entry in self.hub.all_channels() {
            let desc = entry.descriptor();
            let latest = entry.latest_value();
            let evicted_samples = match entry.store() {
                ChannelStore::Scalar(s) => s.read().evicted_samples(),
                ChannelStore::Frame { .. } | ChannelStore::Event(_) => 0,
            };
            let info = RegisteredChannelInfo {
                channel_id: entry.id(),
                source_id: desc.source_id.clone(),
                local_path: desc.local_path.clone(),
                display_name: desc.display_name.clone().unwrap_or_else(|| desc.local_path.clone()),
                full_path: entry.path().as_str().to_owned(),
                kind: desc.kind.clone(),
                unit: desc.unit.clone(),
                total_samples: entry.total_samples(),
                evicted_samples,
                latest_timestamp_ns: latest.map(|(ts, _)| ts),
                latest_value: latest.map(|(_, val)| val),
            };
            by_source
                .entry(desc.source_id.as_str().to_owned())
                .or_default()
                .push(info);
        }

        let mut total_channels = 0;
        let mut sources = Vec::with_capacity(by_source.len());
        for (src_str, mut channels) in by_source {
            channels.sort_by(|a, b| {
                a.local_path
                    .cmp(&b.local_path)
                    .then_with(|| a.channel_id.cmp(&b.channel_id))
            });
            total_channels += channels.len();
            sources.push(SourceChannelGroup {
                source_id: SourceId::new(src_str),
                channels,
            });
        }

        ChannelTreeSnapshot {
            total_channels,
            sources,
        }
    }

    /// Registers a new scope or replaces an existing scope configuration.
    pub fn upsert_scope(&self, mut config: ScopeConfig) -> CoreResult<ScopeSummary> {
        let scope_id = config.scope_id;
        config.controller.scope_id = scope_id;
        config.controller.sync_group_id = config.sync_group_id;
        let sync_group = config.sync_group_id;

        let summary = {
            let mut scopes = self.scopes.lock();
            if let Some(existing) = scopes.get_mut(&scope_id) {
                existing.config.title = config.title.clone();
                existing.config.tracks = config.tracks.clone();
                existing.config.width_px = config.width_px.max(1);
                existing.config.auto_pin_on_latch = config.auto_pin_on_latch;
                existing.config.sync_group_id = config.sync_group_id;
                existing.controller.update_config(config.controller.clone());
                if !existing.controller.is_pinned() {
                    existing.clear_pins();
                }
                existing.summary()
            } else {
                let managed = ManagedScope::new(config);
                let s = managed.summary();
                scopes.insert(scope_id, managed);
                s
            }
        };

        if let Some(group_id) = sync_group {
            self.sync_groups.register_scope(scope_id, group_id);
        } else {
            self.sync_groups.unregister_scope(scope_id);
        }

        Ok(summary)
    }

    /// Removes a scope and releases any [`PinnedGuard`] chunk locks it held.
    pub fn remove_scope(&self, scope_id: &ScopeId) -> CoreResult<()> {
        let removed = self.scopes.lock().remove(scope_id);
        if removed.is_none() {
            return Err(CoreError::ScopeNotFound(*scope_id));
        }
        self.sync_groups.unregister_scope(*scope_id);
        Ok(())
    }

    /// Returns a summary snapshot of all active scopes.
    pub fn list_scopes(&self) -> Vec<ScopeSummary> {
        self.scopes
            .lock()
            .values()
            .map(ManagedScope::summary)
            .collect()
    }

    /// Returns the summary for a single scope.
    pub fn scope_summary(&self, scope_id: &ScopeId) -> CoreResult<ScopeSummary> {
        self.scopes
            .lock()
            .get(scope_id)
            .map(ManagedScope::summary)
            .ok_or(CoreError::ScopeNotFound(*scope_id))
    }

    /// Replaces the track bindings for an existing scope without clobbering already-pinned guards.
    pub fn set_scope_tracks(
        &self,
        scope_id: &ScopeId,
        tracks: Vec<ScopeTrackBinding>,
    ) -> CoreResult<ScopeSummary> {
        let mut scopes = self.scopes.lock();
        let scope = scopes
            .get_mut(scope_id)
            .ok_or(CoreError::ScopeNotFound(*scope_id))?;
        scope.config.tracks = tracks;
        if scope.controller.is_pinned() && scope.config.auto_pin_on_latch {
            if let Some(win) = scope.controller.current_window() {
                let t_start = win.win_t_start_ns;
                let t_end = t_start.saturating_add(win.win_t_span_ns);
                for track in &scope.config.tracks {
                    if let std::collections::hash_map::Entry::Vacant(e) =
                        scope.pinned_guards.entry(track.channel_id)
                    {
                        if let Some(entry) = self.hub.get(track.channel_id) {
                            if matches!(entry.store(), ChannelStore::Scalar(_)) {
                                if let Ok(guard) = entry.pin_range(t_start, t_end) {
                                    if !guard.is_empty() {
                                        e.insert(guard);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(scope.summary())
    }

    /// Adjusts `scope_id`'s horizontal time span and pan offset (supporting arbitrary zoom-in on
    /// pinned chunks after ring-buffer rollover) and propagates changes if assigned to a sync group.
    pub fn set_scope_viewport(
        &self,
        scope_id: &ScopeId,
        time_span_ns: u64,
        pan_offset_ns: i64,
        width_px: Option<u32>,
    ) -> CoreResult<ScopeSummary> {
        let (summary, sync_info) = {
            let mut scopes = self.scopes.lock();
            let scope = scopes
                .get_mut(scope_id)
                .ok_or(CoreError::ScopeNotFound(*scope_id))?;
            scope.controller.set_time_span_ns(time_span_ns);
            scope.controller.set_pan_offset_ns(pan_offset_ns);
            if let Some(w) = width_px {
                scope.config.width_px = w.max(1);
            }
            let sync_info = scope
                .config
                .sync_group_id
                .map(|group_id| (group_id, scope.controller.config().time_span_ns, pan_offset_ns));
            (scope.summary(), sync_info)
        };

        if let Some((group_id, applied_span, applied_offset)) = sync_info {
            self.sync_groups
                .update_timebase(group_id, applied_span, applied_offset);
            let member_ids = self.sync_groups.get_group_scopes(group_id);
            let mut scopes = self.scopes.lock();
            for sid in member_ids {
                if &sid == scope_id {
                    continue;
                }
                if let Some(peer) = scopes.get_mut(&sid) {
                    peer.controller.set_time_span_ns(applied_span);
                    peer.controller.set_pan_offset_ns(applied_offset);
                }
            }
        }

        Ok(summary)
    }

    /// Drops any pinned chunk guards held by `scope_id` and re-arms its trigger engine for new data.
    pub fn rearm_scope(&self, scope_id: &ScopeId) -> CoreResult<ScopeSummary> {
        let (max_ts, trig_total_samples) = {
            let scopes = self.scopes.lock();
            let scope = scopes
                .get(scope_id)
                .ok_or(CoreError::ScopeNotFound(*scope_id))?;
            let max_ts = scope_max_channel_timestamp(&self.hub, scope);
            let trig_total = trigger_channel_id(&scope.controller)
                .and_then(|cid| self.hub.get(cid))
                .map(|entry| entry.total_samples());
            (max_ts, trig_total)
        };
        let armed_at = max_ts.map_or(0, |t| t.saturating_add(1));

        let mut scopes = self.scopes.lock();
        let scope = scopes
            .get_mut(scope_id)
            .ok_or(CoreError::ScopeNotFound(*scope_id))?;
        scope.clear_pins();
        scope.last_scan_abs_sample = trig_total_samples;
        scope.controller.rearm_at(armed_at);
        Ok(scope.summary())
    }

    /// Re-arms `scope_id` explicitly anchored at `armed_at_ns`.
    pub fn rearm_scope_at(
        &self,
        scope_id: &ScopeId,
        armed_at_ns: TimestampNs,
    ) -> CoreResult<ScopeSummary> {
        let mut scopes = self.scopes.lock();
        let scope = scopes
            .get_mut(scope_id)
            .ok_or(CoreError::ScopeNotFound(*scope_id))?;
        scope.clear_pins();
        scope.last_scan_abs_sample = None;
        scope.controller.rearm_at(armed_at_ns);
        Ok(scope.summary())
    }

    /// Updates a shared [`SyncGroupId`] timebase & cursor state and synchronizes sibling scopes.
    pub fn update_sync_group(&self, update: &SyncGroupUpdate) -> CoreResult<SyncGroupState> {
        self.sync_groups.update_timebase(
            update.group_id,
            update.time_span_ns,
            update.center_offset_ns,
        );
        self.sync_groups.update_cursors(
            update.group_id,
            update.cursor_a_ratio,
            update.cursor_b_ratio,
        );
        let state = self
            .sync_groups
            .get_group_state(update.group_id)
            .ok_or_else(|| {
                CoreError::InvalidConfig(format!("sync group {:?} missing", update.group_id))
            })?;

        let member_ids = self.sync_groups.get_group_scopes(update.group_id);
        let mut scopes = self.scopes.lock();
        for sid in member_ids {
            if let Some(scope) = scopes.get_mut(&sid) {
                scope.controller.set_time_span_ns(state.time_span_ns);
                scope.controller.set_pan_offset_ns(state.center_offset_ns);
            }
        }
        Ok(state)
    }

    /// Returns the synchronized state snapshot for `group_id`, if any.
    pub fn sync_group_state(&self, group_id: SyncGroupId) -> Option<SyncGroupState> {
        self.sync_groups.get_group_state(group_id)
    }

    /// Evaluates trigger conditions (with M4 L2/L1 fast-forwarding), advances post-trigger windows,
    /// and acquires/refreshes [`PinnedGuard`] chunk locks across all registered scopes.
    ///
    /// Also enforces global chunk memory quota so unpinned roll-over never stalls.
    pub fn tick_scopes(&self, now_ns: TimestampNs) -> Vec<ScopeSummary> {
        let mut scopes = self.scopes.lock();
        let mut summaries = Vec::with_capacity(scopes.len());

        for scope in scopes.values_mut() {
            step_managed_scope(&self.hub, scope, now_ns);
            summaries.push(scope.summary());
        }
        drop(scopes);

        self.hub.enforce_global_quota();
        summaries
    }

    /// Advances all scopes at `now_ns`, extracts viewport waveforms (reading directly from
    /// [`PinnedGuard`] chunks whenever latched/pinned), and serializes a `DDSF v1` frame.
    ///
    /// Passes the zero-copy frame slice directly to `f` without heap allocation.
    pub fn step_and_pack_ddsf_with<R>(
        &self,
        now_ns: TimestampNs,
        f: impl FnOnce(&[u8]) -> R,
    ) -> CoreResult<R> {
        let payloads = {
            let mut scopes = self.scopes.lock();
            let mut list = Vec::with_capacity(scopes.len());
            for scope in scopes.values_mut() {
                step_managed_scope(&self.hub, scope, now_ns);
                if let Some(vp) = extract_scope_viewport(&self.hub, scope, now_ns) {
                    list.push(vp);
                }
            }
            list
        };

        self.hub.enforce_global_quota();
        self.pack_prepared_viewports_with(now_ns, &payloads, f)
    }

    /// Convenience wrapper around [`Self::step_and_pack_ddsf_with`] returning an owned [`Bytes`]
    /// copy of the serialized `DDSF v1` frame.
    pub fn step_and_pack_ddsf(&self, now_ns: TimestampNs) -> CoreResult<Bytes> {
        self.step_and_pack_ddsf_with(now_ns, Bytes::copy_from_slice)
    }

    /// Extracts viewports for the current scope states (without scanning new triggers) and packs
    /// a `DDSF v1` frame.
    pub fn pack_ddsf_frame_with<R>(
        &self,
        now_ns: TimestampNs,
        f: impl FnOnce(&[u8]) -> R,
    ) -> CoreResult<R> {
        let payloads = {
            let scopes = self.scopes.lock();
            let mut list = Vec::with_capacity(scopes.len());
            for scope in scopes.values() {
                if let Some(vp) = extract_scope_viewport(&self.hub, scope, now_ns) {
                    list.push(vp);
                }
            }
            list
        };
        self.pack_prepared_viewports_with(now_ns, &payloads, f)
    }

    /// Owned [`Bytes`] variant of [`Self::pack_ddsf_frame_with`].
    pub fn pack_ddsf_frame(&self, now_ns: TimestampNs) -> CoreResult<Bytes> {
        self.pack_ddsf_frame_with(now_ns, Bytes::copy_from_slice)
    }

    fn pack_prepared_viewports_with<R>(
        &self,
        now_ns: TimestampNs,
        scopes: &[ScopeViewportPayload],
        f: impl FnOnce(&[u8]) -> R,
    ) -> CoreResult<R> {
        let t0 = Instant::now();
        let snapshots = self.hub.collect_latest_snapshots();
        let quota_flags = self.quota_manager.flags();
        let mut flags: u16 = 0;
        if (quota_flags & 1) != 0 {
            flags |= DDSF_FLAG_QUOTA_WARNING;
        }
        if (quota_flags & 2) != 0 {
            flags |= DDSF_FLAG_PINNED_ACTIVE;
        }
        let seq = self.frame_seq.fetch_add(1, Ordering::Relaxed) + 1;

        let res = {
            let mut packer = self.packer.lock();
            let slice = packer.pack_frame(seq as u32, now_ns, flags, scopes, &snapshots)?;
            f(slice)
        };
        let elapsed_ns = t0.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        self.last_frame_pack_ns.store(elapsed_ns, Ordering::Relaxed);
        Ok(res)
    }

    /// Returns a combined telemetry snapshot across all sources, scopes, quota, and packing.
    pub fn telemetry_snapshot(&self) -> WorkspaceTelemetrySnapshot {
        let sources: Vec<SourceSummaryOrArc> = self
            .sources
            .read()
            .values()
            .map(|a| SourceSummaryOrArc(Arc::clone(a)))
            .collect();
        let mut source_snaps: Vec<SourceTelemetrySnapshot> = sources
            .into_iter()
            .map(|s| s.0.lock().telemetry_snapshot())
            .collect();
        source_snaps.sort_by(|a, b| a.source_id.as_str().cmp(b.source_id.as_str()));

        let scopes = self.list_scopes();
        let quota = self.hub.quota_stats();
        let total_channels = self.hub.all_channels().len();
        WorkspaceTelemetrySnapshot {
            sources: source_snaps,
            scopes,
            quota,
            total_channels,
            total_frames_packed: self.frame_seq.load(Ordering::Relaxed),
            last_frame_pack_ns: self.last_frame_pack_ns.load(Ordering::Relaxed),
        }
    }
}

struct SourceSummaryOrArc(Arc<Mutex<SourceSession>>);

fn trigger_channel_id(ctrl: &ScopeController) -> Option<ChannelId> {
    ctrl.config().trigger.map(|cond| match cond {
        TriggerCondition::Edge { channel, .. } | TriggerCondition::StateEquals { channel, .. } => {
            channel
        }
    })
}

fn scope_max_channel_timestamp(hub: &ChannelHub, scope: &ManagedScope) -> Option<TimestampNs> {
    let mut best: Option<TimestampNs> = None;
    for track in &scope.config.tracks {
        if let Some(entry) = hub.get(track.channel_id) {
            if let Some((ts, _)) = entry.latest_value() {
                best = Some(best.map_or(ts, |prev| prev.max(ts)));
            }
        }
    }
    if let Some(trig_cid) = trigger_channel_id(&scope.controller) {
        if let Some(entry) = hub.get(trig_cid) {
            if let Some((ts, _)) = entry.latest_value() {
                best = Some(best.map_or(ts, |prev| prev.max(ts)));
            }
        }
    }
    best
}

fn pin_scope_range(
    hub: &ChannelHub,
    scope: &mut ManagedScope,
    t_start_ns: TimestampNs,
    t_end_ns: TimestampNs,
) {
    if !scope.config.auto_pin_on_latch {
        return;
    }
    let mut target_channels: HashSet<ChannelId> =
        scope.config.tracks.iter().map(|t| t.channel_id).collect();
    if let Some(trig_cid) = trigger_channel_id(&scope.controller) {
        target_channels.insert(trig_cid);
    }

    for cid in target_channels {
        let Some(entry) = hub.get(cid) else {
            continue;
        };
        if !matches!(entry.store(), ChannelStore::Scalar(_)) {
            continue;
        }
        // Pin new range first while holding any prior partial guard so pin_count never
        // briefly drops to 0 during concurrent worker evictions; only fall back to
        // dropping the prior guard first if QuotaExceeded is hit.
        let pin_res = match entry.pin_range(t_start_ns, t_end_ns) {
            Ok(guard) => Ok(guard),
            Err(CoreError::QuotaExceeded { .. }) => {
                let prev_guard = scope.pinned_guards.remove(&cid);
                drop(prev_guard);
                entry.pin_range(t_start_ns, t_end_ns)
            }
            Err(err) => Err(err),
        };
        if let Ok(guard) = pin_res {
            if !guard.is_empty() {
                scope.pinned_guards.insert(cid, guard);
            } else {
                scope.pinned_guards.remove(&cid);
            }
        }
    }
}

fn step_managed_scope(hub: &ChannelHub, scope: &mut ManagedScope, now_ns: TimestampNs) {
    let sweep_mode = scope.controller.config().sweep_mode;
    let channel_max_ts = scope_max_channel_timestamp(hub, scope);

    if sweep_mode == TriggerSweepMode::Roll {
        let roll_now = channel_max_ts.map_or(now_ns, |ts| ts.max(now_ns));
        scope.controller.step(roll_now);
        return;
    }

    if matches!(
        scope.controller.current_state(),
        TriggerRuntimeState::Armed | TriggerRuntimeState::AutoFreerun
    ) {
        if let Some(cond) = scope.controller.config().trigger {
            match cond {
                TriggerCondition::Edge {
                    channel,
                    level,
                    hysteresis,
                    slope,
                } => {
                    if let Some(entry) = hub.get(channel) {
                        if let ChannelStore::Scalar(series_lock) = entry.store() {
                            // Scope read guard strictly to trigger scan; never hold across pin_range.
                            let series = series_lock.read();
                            let _ = scan_scalar_trigger_m4(
                                &series, scope, channel, slope, level, hysteresis,
                            );
                        }
                    }
                }
                TriggerCondition::StateEquals { channel, .. } => {
                    if let Some(entry) = hub.get(channel) {
                        let _ = scan_event_trigger(entry.store(), scope, channel);
                    }
                }
            }
        }
    }

    if scope.controller.current_state() == TriggerRuntimeState::Triggered {
        // Advance Triggered -> LatchedPinned/Armed ONLY via actual channel sample timestamps so
        // host wall-clock jitter never latches Single before post-trigger samples arrive.
        let post_ts = channel_max_ts.unwrap_or(now_ns);
        let outcome = scope.controller.step(post_ts);
        if matches!(outcome, ScopeStepOutcome::CaptureCompleted { .. })
            && !scope.controller.is_pinned()
        {
            scope.clear_pins();
        } else if scope.controller.current_state() == TriggerRuntimeState::Triggered
            && scope.controller.config().sweep_mode == TriggerSweepMode::Single
            && scope.config.auto_pin_on_latch
            && scope.pinned_guards.is_empty()
        {
            if let Some(hit) = scope.controller.active_hit() {
                let pre_ratio = scope.controller.config().pre_trigger_ratio.clamp(0.0, 1.0);
                let pre_span = (pre_ratio * scope.controller.config().time_span_ns as f64).round()
                    as u64;
                let pre_start = hit.timestamp_ns.saturating_sub(pre_span);
                let target_end = scope.controller.target_win_end_ns();
                pin_scope_range(hub, scope, pre_start, target_end);
            }
        }
    }

    if scope.controller.current_state() == TriggerRuntimeState::LatchedPinned {
        if scope.config.auto_pin_on_latch && !scope.pins_acquired_for_latch {
            if let Some(win) = scope.controller.current_window() {
                let t_end = win.win_t_start_ns.saturating_add(win.win_t_span_ns);
                pin_scope_range(hub, scope, win.win_t_start_ns, t_end);
                scope.pins_acquired_for_latch = true;
            }
        }
        return;
    }

    if matches!(
        scope.controller.current_state(),
        TriggerRuntimeState::Armed | TriggerRuntimeState::AutoFreerun
    ) {
        let auto_now = channel_max_ts.map_or(now_ns, |ts| ts.max(now_ns));
        if matches!(
            scope.controller.step(auto_now),
            ScopeStepOutcome::AutoFreerunFired { .. }
        ) {
            scope.clear_pins();
        }
    }
}

fn scan_scalar_trigger_m4(
    series: &NativeScalarSeries,
    scope: &mut ManagedScope,
    channel: ChannelId,
    slope: EdgeSlope,
    level: f64,
    hysteresis: f64,
) -> bool {
    let total = series.total_samples();
    let evicted = series.evicted_samples();
    if total <= evicted {
        return false;
    }

    let mut cur_abs = scope
        .last_scan_abs_sample
        .map_or(evicted, |prev| prev.max(evicted));
    if cur_abs >= total {
        return false;
    }

    let armed_time_ns = scope.controller.armed_time_ns();
    let band_half = hysteresis.abs();
    let eps = (level.abs() + band_half + 1.0) * 1e-4 + 1e-6;
    let margin = band_half + eps;
    let scale = series.scale();

    let sealed = series.sealed_chunks();
    let offsets = series.chunk_offsets();

    for (chunk_idx, chunk_handle) in sealed.iter().enumerate() {
        let chunk_base = offsets.get(chunk_idx).copied().unwrap_or(0);
        let ch_len = chunk_handle.sample_count as u64;
        let chunk_end = chunk_base + ch_len;
        if cur_abs >= chunk_end {
            continue;
        }
        if chunk_handle.t_end_ns < armed_time_ns {
            cur_abs = chunk_end;
            scope.last_scan_abs_sample = Some(cur_abs);
            continue;
        }

        let Some(arc) = chunk_handle.storage.as_in_memory() else {
            cur_abs = chunk_end;
            scope.last_scan_abs_sample = Some(cur_abs);
            continue;
        };
        let DataChunk::Scalar(chunk) = arc.as_ref() else {
            cur_abs = chunk_end;
            scope.last_scan_abs_sample = Some(cur_abs);
            continue;
        };

        let mut i = (cur_abs - chunk_base) as usize;
        let end = chunk.len();

        if let Some(ts_at_i) = chunk.timestamps.get_ns(i) {
            if ts_at_i < armed_time_ns {
                i = crate::storage::scalar_series::ts_col_partition_point(
                    &chunk.timestamps,
                    end,
                    |ts| ts < armed_time_ns,
                )
                .max(i);
            }
        }

        while i < end {
            let l2_block = M4_BRANCH_FACTOR * M4_BRANCH_FACTOR;
            if i % l2_block == 0 && i + l2_block <= end {
                let l2_idx = i / l2_block;
                if let Some(b) = chunk.m4.l2.as_slice().get(l2_idx) {
                    if can_fast_forward_bucket(
                        &scope.controller,
                        b,
                        slope,
                        level,
                        margin,
                        armed_time_ns,
                    ) {
                        let tail_idx = i + l2_block - 1;
                        if let Some((ts, val)) = chunk.get_sample_as_f64(tail_idx) {
                            scope.last_scan_abs_sample = Some(chunk_base + tail_idx as u64 + 1);
                            let outcome = scope.controller.process_scalar_sample(channel, ts, val);
                            if matches!(
                                outcome,
                                ScopeStepOutcome::TriggerDetected { .. }
                                    | ScopeStepOutcome::CaptureCompleted { .. }
                            ) {
                                return true;
                            }
                            i += l2_block;
                            continue;
                        }
                    }
                }
            }

            let l1_block = M4_BRANCH_FACTOR;
            if i % l1_block == 0 && i + l1_block <= end {
                let l1_idx = i / l1_block;
                if let Some(b) = chunk.m4.l1.as_slice().get(l1_idx) {
                    if can_fast_forward_bucket(
                        &scope.controller,
                        b,
                        slope,
                        level,
                        margin,
                        armed_time_ns,
                    ) {
                        let tail_idx = i + l1_block - 1;
                        if let Some((ts, val)) = chunk.get_sample_as_f64(tail_idx) {
                            scope.last_scan_abs_sample = Some(chunk_base + tail_idx as u64 + 1);
                            let outcome = scope.controller.process_scalar_sample(channel, ts, val);
                            if matches!(
                                outcome,
                                ScopeStepOutcome::TriggerDetected { .. }
                                    | ScopeStepOutcome::CaptureCompleted { .. }
                            ) {
                                return true;
                            }
                            i += l1_block;
                            continue;
                        }
                    }
                }
            }

            if let Some((ts, val)) = chunk.get_sample_as_f64(i) {
                scope.last_scan_abs_sample = Some(chunk_base + i as u64 + 1);
                if ts >= armed_time_ns {
                    let outcome = scope.controller.process_scalar_sample(channel, ts, val);
                    if matches!(
                        outcome,
                        ScopeStepOutcome::TriggerDetected { .. }
                            | ScopeStepOutcome::CaptureCompleted { .. }
                    ) {
                        return true;
                    }
                }
            }
            i += 1;
        }
        cur_abs = chunk_end;
    }

    // Active (in-progress) chunk tail.
    let active_base = series.total_sealed();
    if cur_abs < total && total > active_base {
        let active = series.active_chunk();
        let active_len = (total - active_base) as usize;
        let mut i = cur_abs.saturating_sub(active_base) as usize;
        while i < active_len {
            if let Some((ts, val)) = active.get_sample_as_f64(i, &scale) {
                scope.last_scan_abs_sample = Some(active_base + i as u64 + 1);
                if ts >= armed_time_ns {
                    let outcome = scope.controller.process_scalar_sample(channel, ts, val);
                    if matches!(
                        outcome,
                        ScopeStepOutcome::TriggerDetected { .. }
                            | ScopeStepOutcome::CaptureCompleted { .. }
                    ) {
                        return true;
                    }
                }
            }
            i += 1;
        }
    }

    false
}

#[inline]
fn can_fast_forward_bucket(
    ctrl: &ScopeController,
    b: &M4Bucket,
    slope: EdgeSlope,
    level: f64,
    margin: f64,
    armed_time_ns: TimestampNs,
) -> bool {
    let Some(engine) = ctrl.trigger_engine() else {
        return false;
    };
    let Some((_, prev_val)) = engine.last_scalar_sample() else {
        return false;
    };
    let cfg = ctrl.config();
    if cfg.sweep_mode == TriggerSweepMode::Auto
        && b.t_last_ns.saturating_sub(armed_time_ns) >= cfg.auto_timeout_ns
    {
        return false;
    }
    if !b.v_min.is_finite() || !b.v_max.is_finite() {
        return false;
    }
    let band_state = engine.state();
    let bucket_below = (b.v_max as f64) < level - margin
        && prev_val < level - margin
        && matches!(
            (slope, band_state),
            (EdgeSlope::Rising | EdgeSlope::Either, HysteresisBandState::ArmedLow)
                | (EdgeSlope::Falling, HysteresisBandState::Unprimed)
        );
    let bucket_above = (b.v_min as f64) > level + margin
        && prev_val > level + margin
        && matches!(
            (slope, band_state),
            (EdgeSlope::Falling | EdgeSlope::Either, HysteresisBandState::ArmedHigh)
                | (EdgeSlope::Rising, HysteresisBandState::Unprimed)
        );
    bucket_below || bucket_above
}

fn scan_event_trigger(
    store: &ChannelStore,
    scope: &mut ManagedScope,
    channel: ChannelId,
) -> bool {
    let ChannelStore::Event(events_lock) = store else {
        return false;
    };
    let events = events_lock.read();
    let total = events.total_samples();
    let cur = scope.last_scan_abs_sample.unwrap_or(0);
    if cur >= total {
        return false;
    }
    let armed_time_ns = scope.controller.armed_time_ns();
    for idx in cur..total {
        scope.last_scan_abs_sample = Some(idx + 1);
        if let Some((ts, state)) = events.get_transition_by_index(idx) {
            if ts >= armed_time_ns {
                let outcome = scope.controller.process_state_sample(channel, ts, state);
                if matches!(
                    outcome,
                    ScopeStepOutcome::TriggerDetected { .. }
                        | ScopeStepOutcome::CaptureCompleted { .. }
                ) {
                    return true;
                }
            }
        }
    }
    false
}

fn extract_scope_viewport(
    hub: &ChannelHub,
    scope: &ManagedScope,
    _now_ns: TimestampNs,
) -> Option<ScopeViewportPayload> {
    let win = scope.controller.current_window()?;
    let t_min = win.win_t_start_ns;
    let t_max = t_min.saturating_add(win.win_t_span_ns);
    let w_px = scope.config.width_px.max(1) as usize;
    let mut tracks = Vec::with_capacity(scope.config.tracks.len());
    let use_pins = scope.controller.is_pinned();

    for binding in &scope.config.tracks {
        let entry = hub.get(binding.channel_id);
        let pinned_guard = if use_pins {
            scope.pinned_guards.get(&binding.channel_id)
        } else {
            None
        };

        match binding.lane_kind {
            ScopeLaneKind::Analog => {
                let mut res = if let Some(guard) = pinned_guard {
                    guard.query_m4_viewport_with_stats(t_min, t_max, w_px)
                } else if let Some(ref e) = entry {
                    e.query_m4_viewport_with_stats(t_min, t_max, w_px)
                } else {
                    continue;
                };

                // If a pinned guard has empty overlap (e.g. viewport reaches unsealed active tail),
                // fall back to ChannelEntry seamlessly.
                let is_empty_pin = match &res {
                    Ok(ViewportExtract {
                        result: M4QueryResult::Columns(v),
                        ..
                    }) => v.is_empty(),
                    Ok(ViewportExtract {
                        result: M4QueryResult::Raw(v),
                        ..
                    }) => v.is_empty(),
                    Err(_) => true,
                };
                if is_empty_pin && pinned_guard.is_some() {
                    if let Some(ref e) = entry {
                        res = e.query_m4_viewport_with_stats(t_min, t_max, w_px);
                    }
                }

                if let Ok(ViewportExtract { result, stats }) = res {
                    match result {
                        M4QueryResult::Columns(cols) => {
                            tracks.push(TrackPayload::new_m4(
                                binding.channel_id.0,
                                cols,
                                Some(stats),
                            ));
                        }
                        M4QueryResult::Raw(points) => {
                            tracks.push(TrackPayload::new_raw_xy_with_stats(
                                binding.channel_id.0,
                                points,
                                Some(stats),
                            ));
                        }
                    }
                }
            }
            ScopeLaneKind::Digital => {
                let mut res = if let Some(guard) = pinned_guard {
                    guard.extract_digital_edges(t_min, t_max)
                } else if let Some(ref e) = entry {
                    e.extract_digital_edges(t_min, t_max)
                } else {
                    continue;
                };
                if pinned_guard.is_some() && res.as_ref().is_ok_and(|(_, edges)| edges.is_empty()) {
                    if let Some(ref e) = entry {
                        if let Ok(live) = e.extract_digital_edges(t_min, t_max) {
                            if !live.1.is_empty() {
                                res = Ok(live);
                            }
                        }
                    }
                }
                if let Ok((init_level, edges)) = res {
                    tracks.push(TrackPayload::new_digital_edges(
                        binding.channel_id.0,
                        init_level,
                        edges,
                    ));
                }
            }
            ScopeLaneKind::StateRibbon => {
                if let Some(ref e) = entry {
                    if let Ok(ribbons) = e.extract_state_ribbons(t_min, t_max) {
                        tracks.push(TrackPayload::new_state_ribbons(
                            binding.channel_id.0,
                            ribbons,
                        ));
                    }
                }
            }
        }
    }

    Some(ScopeViewportPayload {
        scope_id: scope.config.scope_id.0,
        trigger_state: win.trigger_state.as_u8(),
        mode: win.mode.as_u8(),
        win_t_start_ns: win.win_t_start_ns,
        win_t_span_ns: win.win_t_span_ns,
        subpixel_offset_ns: win.subpixel_offset_ns,
        tracks,
    })
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
