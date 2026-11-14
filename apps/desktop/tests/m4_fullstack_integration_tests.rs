//! End-to-end integration tests for Milestone M4 (`T06` & `T07` / Gate 3):
//! - `T06`: `build_live_desktop_state` (`2 GiB` default quota, seeded `sim0` 2 MHz + `Tier1HardwareCounter`),
//!   `ZeroCopyDataBridge` DDSF v1 framing, and `WorkspaceTelemetryEvent` conversion.
//! - `T07`: Multi-source concurrent ingestion (`sim0` + real `127.0.0.1` UDP `JustFloat` + `.ddraw` recording + `RawFileReplay` seek),
//!   independent `Single` glitch latch + `Chunk Pinning` surviving `2x` `16 MiB` rollover + `10,000x` zoom + `rearm_scope`,
//!   and `SyncGroup` alignment + closed-loop `kp` / `freq_hz` TX downlink modulation.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tokio::io::AsyncWriteExt;
use tokio::runtime::Handle;

use ddscope_core::clock::{ClockMode, StreamClockSynchronizer};
use ddscope_core::ddsf::{
    DdsfBlockKind, DdsfZeroCopyReader, DDSF_FLAG_PINNED_ACTIVE, DDSF_MAGIC, DDSF_VERSION_V1,
};
use ddscope_core::protocol::{
    default_sim_state_labels, FireWaterColumnSpec, FireWaterConfig, FireWaterCsvCodec,
    JustFloatCodec, JustFloatConfig, JUSTFLOAT_TAIL_MAGIC,
};
use ddscope_core::scope::TriggerRuntimeState;
use ddscope_core::storage::{ChannelEntry, ChannelStore};
use ddscope_core::transport::{
    host_monotonic_ns, DdrawReader, RawTapDirection, SerialConfig, SerialTransport,
    TransportRuntimeConfig, UdpConfig, UdpTransport, VirtualSerialPair, VirtualUdpHub,
    DDRAW_VERSION,
};
use ddscope_core::types::{ScopeId, SourceId, SyncGroupId};
use ddscope_core::workspace::{SourceRuntimeState, SourceSession, SourceSessionComponents};
use ddscope_desktop_lib::commands::{
    convert, ClockConfigDto, CreateSourceRequestDto, DownlinkPayloadDto, EdgeSlopeDto,
    ProtocolConfigDto, RawRecorderRequestDto, ScopeLaneKindDto, ScopeTrackBindingDto,
    ScopeViewportConfigDto, SeekReplayRequestDto, SendDownlinkRequestDto, SourceRuntimeStateDto,
    SyncGroupUpdateDto, TransportConfigDto, TriggerConditionDto, TriggerSweepModeDto,
    TxPriorityDto,
};
use ddscope_desktop_lib::data_bridge::{DataBridgeError, DataBridgeModeDto, FrameSink};
use ddscope_desktop_lib::runtime::{
    build_live_desktop_state, DesktopRuntimeConfig, DEFAULT_DESKTOP_QUOTA_BYTES,
};

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

struct TempFileGuard {
    path: PathBuf,
}

impl TempFileGuard {
    fn new(prefix: &str, ext: &str) -> Self {
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("ddscope_m4_{prefix}_{pid}_{seq}.{ext}"));
        let _ = std::fs::remove_file(&path);
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

struct CapturingSink {
    frames: Mutex<Vec<(u32, Vec<u8>)>>,
}

impl CapturingSink {
    fn new() -> Self {
        Self {
            frames: Mutex::new(Vec::new()),
        }
    }

    fn last_frame(&self) -> Option<(u32, Vec<u8>)> {
        self.frames.lock().last().cloned()
    }
}

impl FrameSink for CapturingSink {
    fn mode(&self) -> DataBridgeModeDto {
        DataBridgeModeDto::RawIpcChannel
    }

    fn push_ddsf_frame(
        &self,
        frame_bytes: &[u8],
        frame_seq: u32,
    ) -> Result<DataBridgeModeDto, DataBridgeError> {
        self.frames.lock().push((frame_seq, frame_bytes.to_vec()));
        Ok(DataBridgeModeDto::RawIpcChannel)
    }
}

fn entry_evicted_samples(entry: &ChannelEntry) -> u64 {
    match entry.store() {
        ChannelStore::Scalar(s) => s.read().evicted_samples(),
        ChannelStore::Frame { .. } | ChannelStore::Event(_) => 0,
    }
}

async fn wait_until<F>(mut cond: F, timeout: Duration, label: &str)
where
    F: FnMut() -> bool,
{
    let deadline = Instant::now() + timeout;
    while !cond() {
        assert!(
            Instant::now() < deadline,
            "timed out after {timeout:?} waiting for: {label}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn encode_justfloat_datagram(values: &[f32]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(values.len() * 4 + JUSTFLOAT_TAIL_MAGIC.len());
    for &v in values {
        buf.extend_from_slice(&v.to_le_bytes());
    }
    buf.extend_from_slice(&JUSTFLOAT_TAIL_MAGIC);
    buf
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m4_t06_live_runtime_bootstrap_and_zero_copy_ddsf_stream() {
    let state = build_live_desktop_state(Handle::current(), DesktopRuntimeConfig::default())
        .expect("build_live_desktop_state must succeed");
    assert!(state.is_live());

    let engine = state.engine_info();
    assert!(engine.bridge_ready);
    assert_eq!(engine.ddsf_version, DDSF_VERSION_V1);

    let ws = state.workspace().expect("live workspace attached");
    assert_eq!(ws.quota_stats().quota_bytes, DEFAULT_DESKTOP_QUOTA_BYTES);

    let tree = state.get_channel_tree().expect("get_channel_tree");
    let sim0_group = tree
        .sources
        .iter()
        .find(|g| g.source.source_id == "sim0")
        .expect("seeded sim0 source present");
    assert_eq!(sim0_group.channels.len(), 8);
    assert_eq!(sim0_group.source.transport_kind, "sim");

    let channel_id_by_path = |local_path: &str| -> u32 {
        sim0_group
            .channels
            .iter()
            .find(|c| c.local_path == local_path)
            .unwrap_or_else(|| panic!("missing sim0/{local_path}"))
            .channel_id
    };

    let ch0 = channel_id_by_path("ch0");
    let ch1 = channel_id_by_path("ch1");
    let ch2 = channel_id_by_path("ch2");
    let ch3 = channel_id_by_path("ch3");
    let pwm_a = channel_id_by_path("pwm_a");
    let pwm_b = channel_id_by_path("pwm_b");
    let fault_pin = channel_id_by_path("fault_pin");
    let ctrl_state = channel_id_by_path("ctrl_state");

    let sink = Arc::new(CapturingSink::new());
    state.data_bridge().attach_sink(Arc::clone(&sink) as Arc<dyn FrameSink>);

    state
        .upsert_scope_viewport(ScopeViewportConfigDto {
            scope_id: 1,
            title: Some("Main MSO".into()),
            canvas_width_px: 1024,
            time_span_ns: 2_000_000.0,
            pre_trigger_ratio: 0.25,
            pan_offset_ns: 0.0,
            sweep_mode: TriggerSweepModeDto::Auto,
            auto_timeout_ms: 100,
            trigger: Some(TriggerConditionDto::Edge {
                channel_id: ch0,
                level: 0.0,
                hysteresis: 0.5,
                slope: EdgeSlopeDto::Rising,
            }),
            tracks: vec![
                ScopeTrackBindingDto {
                    channel_id: ch0,
                    lane: ScopeLaneKindDto::Analog,
                    visible: true,
                    y_scale: None,
                    y_offset: None,
                    color_rgba: None,
                },
                ScopeTrackBindingDto {
                    channel_id: ch1,
                    lane: ScopeLaneKindDto::Analog,
                    visible: true,
                    y_scale: None,
                    y_offset: None,
                    color_rgba: None,
                },
                ScopeTrackBindingDto {
                    channel_id: ch2,
                    lane: ScopeLaneKindDto::Analog,
                    visible: true,
                    y_scale: None,
                    y_offset: None,
                    color_rgba: None,
                },
                ScopeTrackBindingDto {
                    channel_id: pwm_a,
                    lane: ScopeLaneKindDto::Digital,
                    visible: true,
                    y_scale: None,
                    y_offset: None,
                    color_rgba: None,
                },
                ScopeTrackBindingDto {
                    channel_id: pwm_b,
                    lane: ScopeLaneKindDto::Digital,
                    visible: true,
                    y_scale: None,
                    y_offset: None,
                    color_rgba: None,
                },
                ScopeTrackBindingDto {
                    channel_id: fault_pin,
                    lane: ScopeLaneKindDto::Digital,
                    visible: true,
                    y_scale: None,
                    y_offset: None,
                    color_rgba: None,
                },
                ScopeTrackBindingDto {
                    channel_id: ctrl_state,
                    lane: ScopeLaneKindDto::StateRibbon,
                    visible: true,
                    y_scale: None,
                    y_offset: None,
                    color_rgba: None,
                },
            ],
            sync_group_id: None,
            paused: false,
        })
        .expect("upsert scope 1");

    state
        .upsert_scope_viewport(ScopeViewportConfigDto {
            scope_id: 2,
            title: Some("Glitch Catcher".into()),
            canvas_width_px: 1024,
            time_span_ns: 2_000_000.0,
            pre_trigger_ratio: 0.5,
            pan_offset_ns: 0.0,
            sweep_mode: TriggerSweepModeDto::Single,
            auto_timeout_ms: 100,
            trigger: Some(TriggerConditionDto::Edge {
                channel_id: ch3,
                level: 60.0,
                hysteresis: 2.0,
                slope: EdgeSlopeDto::Rising,
            }),
            tracks: vec![
                ScopeTrackBindingDto {
                    channel_id: ch3,
                    lane: ScopeLaneKindDto::Analog,
                    visible: true,
                    y_scale: None,
                    y_offset: None,
                    color_rgba: None,
                },
                ScopeTrackBindingDto {
                    channel_id: fault_pin,
                    lane: ScopeLaneKindDto::Digital,
                    visible: true,
                    y_scale: None,
                    y_offset: None,
                    color_rgba: None,
                },
            ],
            sync_group_id: None,
            paused: false,
        })
        .expect("upsert scope 2");

    let sim0_id = SourceId::new("sim0");
    let ch0_entry = ws.find_channel(&sim0_id, "ch0").expect("sim0/ch0");
    let state_entry = ws
        .find_channel(&sim0_id, "ctrl_state")
        .expect("sim0/ctrl_state");
    wait_until(
        || ch0_entry.total_samples() >= 10_000 && state_entry.total_samples() >= 1,
        Duration::from_secs(5),
        "sim0 accumulated >= 10,000 samples and ctrl_state event",
    )
    .await;

    let _ = state
        .step_and_push_ddsf_frame(host_monotonic_ns())
        .expect("prime scopes");
    assert_eq!(
        ws.scope_summary(&ScopeId(2)).unwrap().runtime_state,
        TriggerRuntimeState::Armed
    );

    state
        .send_downlink(SendDownlinkRequestDto {
            source_id: "sim0".into(),
            payload: DownlinkPayloadDto::AsciiText {
                text: "GLITCH".into(),
                append_newline: true,
            },
            priority: TxPriorityDto::High,
            coalesce_key: None,
        })
        .expect("send GLITCH");

    let ready_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let _ = state
            .step_and_push_ddsf_frame(host_monotonic_ns())
            .expect("step until scopes window + latch ready");
        let s1 = ws.scope_summary(&ScopeId(1)).unwrap();
        let s2 = ws.scope_summary(&ScopeId(2)).unwrap();
        if s1.window.is_some() && s2.runtime_state == TriggerRuntimeState::LatchedPinned {
            break;
        }
        assert!(
            Instant::now() < ready_deadline,
            "timed out waiting for scope1 window ({}) and scope2 LatchedPinned ({:?})",
            s1.window.is_some(),
            s2.runtime_state
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    let pushed_mode = state
        .step_and_push_ddsf_frame(host_monotonic_ns())
        .expect("step_and_push_ddsf_frame")
        .expect("attached frame mode");
    assert_eq!(pushed_mode, DataBridgeModeDto::RawIpcChannel);

    let bridge_stats = state.data_bridge_stats();
    assert!(bridge_stats.frames_pushed >= 1.0);

    let (_seq, bytes) = sink.last_frame().expect("captured DDSF v1 frame");
    let reader = DdsfZeroCopyReader::new(&bytes).expect("valid DDSF v1 frame");
    assert_eq!(reader.header().magic, DDSF_MAGIC);
    assert_eq!(reader.header().version, DDSF_VERSION_V1);
    assert_eq!(reader.header().scope_count, 2);
    assert_ne!(
        reader.header().flags & DDSF_FLAG_PINNED_ACTIVE,
        0,
        "Scope 2 Single glitch latch must mark DDSF_FLAG_PINNED_ACTIVE"
    );

    let scope1_pkt = reader
        .scopes()
        .find(|s| s.header().scope_id == 1)
        .expect("scope 1 packet");
    assert!(
        reader.scopes().any(|s| s.header().scope_id == 2),
        "scope 2 packet must be emitted after Single glitch latch"
    );
    let mut saw_analog = false;
    let mut saw_digital = false;
    let mut saw_state = false;
    for tr in scope1_pkt.tracks() {
        let bk = tr.header().block_kind;
        if bk == DdsfBlockKind::AnalogM4Column.as_u8() || bk == DdsfBlockKind::AnalogRawXY.as_u8() {
            saw_analog = true;
        } else if bk == DdsfBlockKind::DigitalEdges.as_u8() {
            saw_digital = true;
        } else if bk == DdsfBlockKind::StateRibbons.as_u8() {
            saw_state = true;
        }
    }
    assert!(saw_analog, "Scope 1 must emit Analog track block");
    assert!(saw_digital, "Scope 1 must emit DigitalEdges track block");
    assert!(saw_state, "Scope 1 must emit StateRibbons track block");

    let snap = ws.telemetry_snapshot();
    let evt = convert::telemetry_snapshot_to_event(&snap, 1_000.0);
    assert_eq!(evt.quota_limit_bytes, DEFAULT_DESKTOP_QUOTA_BYTES as f64);
    let sim0_tel = evt
        .sources
        .iter()
        .find(|s| s.source_id == "sim0")
        .expect("sim0 telemetry");
    assert_eq!(sim0_tel.state, SourceRuntimeStateDto::Running);

    ws.shutdown().await.expect("workspace shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m4_t07_multi_source_concurrent_udp_ddraw_recording_and_replay() {
    let state = build_live_desktop_state(
        Handle::current(),
        DesktopRuntimeConfig {
            quota_bytes: 64 * 1024 * 1024,
            seed_default_sim_source: true,
        },
    )
    .expect("build_live_desktop_state");
    let ws = state.workspace().expect("live workspace");
    let ddraw_guard = TempFileGuard::new("udp_capture", "ddraw");

    // 1. Validate OS UDP DTO control-plane create -> start -> stop -> remove lifecycle.
    let mut os_probe_created = false;
    for _ in 0..5 {
        let probe = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind ephemeral UDP probe");
        let candidate = probe.local_addr().expect("probe local_addr");
        drop(probe);

        let create_res = state.create_source(CreateSourceRequestDto {
            source_id: "udp_os_probe".into(),
            display_name: Some("OS UDP Control-Plane Probe".into()),
            transport: TransportConfigDto::Udp {
                bind_addr: candidate.to_string(),
                multicast_group: None,
                recv_buffer_bytes: 262_144,
                remote_tx_addr: None,
            },
            clock: ClockConfigDto::Tier3HostArrival,
            protocol: ProtocolConfigDto::JustFloat {
                channel_names: vec!["p0".into(), "p1".into()],
            },
            auto_start: false,
        });
        if create_res.is_ok() {
            os_probe_created = true;
            break;
        }
    }
    assert!(os_probe_created, "create udp_os_probe on localhost");
    state
        .start_source("udp_os_probe".into())
        .expect("start udp_os_probe");
    wait_until(
        || {
            ws.list_sources()
                .into_iter()
                .any(|s| s.source_id.as_str() == "udp_os_probe" && s.state == SourceRuntimeState::Running)
        },
        Duration::from_secs(3),
        "udp_os_probe running",
    )
    .await;
    state
        .stop_source("udp_os_probe".into())
        .await
        .expect("stop udp_os_probe");
    state
        .remove_source("udp_os_probe".into())
        .await
        .expect("remove udp_os_probe");

    // 2. Register sandbox-deterministic heterogeneous virtual sources (UDP JustFloat + Serial FireWater)
    //    concurrently alongside seeded sim0.
    let udp_hub = VirtualUdpHub::new();
    let udp_bind: SocketAddr = "127.0.0.1:42001".parse().unwrap();
    let udp_peer_addr: SocketAddr = "127.0.0.1:42002".parse().unwrap();
    let udp_transport = UdpTransport::with_backend(
        UdpConfig {
            bind_addr: udp_bind.to_string(),
            remote_tx_addr: Some(udp_peer_addr.to_string()),
            ..Default::default()
        },
        udp_hub.backend(),
    )
    .expect("virtual udp transport");
    let udp_peer = udp_hub
        .bind_peer(udp_peer_addr, false, None)
        .expect("virtual udp peer");
    let jf_codec = JustFloatCodec::new(JustFloatConfig::new(
        "udp0",
        vec!["u0".into(), "u1".into()],
    ))
    .expect("justfloat codec");
    let udp_session = SourceSession::from_components(
        SourceSessionComponents {
            source_id: SourceId::from("udp0"),
            display_name: Some("UDP Telemetry".into()),
            transport: Box::new(udp_transport),
            clock: StreamClockSynchronizer::new(ClockMode::Tier2SoftwarePll {
                nominal_sample_rate_hz: Some(50_000.0),
            })
            .expect("tier2 clock"),
            codec: Box::new(jf_codec),
            runtime_config: TransportRuntimeConfig::default(),
            sim_control: None,
            replay_control: None,
        },
        Arc::clone(ws.hub()),
    )
    .expect("udp0 session");
    ws.register_source_session(udp_session)
        .expect("register udp0 session");

    let serial_pair = VirtualSerialPair::new("VCOM_M4");
    let mut ser_peer = serial_pair.take_peer().expect("virtual serial peer");
    let ser_transport = SerialTransport::with_provider(
        SerialConfig {
            port_name: "VCOM_M4".into(),
            baud_rate: 2_000_000,
            ..Default::default()
        },
        serial_pair.provider(),
    )
    .expect("virtual serial transport");
    let fw_codec = FireWaterCsvCodec::new(FireWaterConfig::with_columns(
        "ser0",
        vec![
            FireWaterColumnSpec::f32("temp"),
            FireWaterColumnSpec::state_enum("mode", default_sim_state_labels()),
        ],
    ))
    .expect("firewater codec");
    let ser_session = SourceSession::from_components(
        SourceSessionComponents {
            source_id: SourceId::from("ser0"),
            display_name: Some("Serial FireWater".into()),
            transport: Box::new(ser_transport),
            clock: StreamClockSynchronizer::new(ClockMode::Tier3HostArrival)
                .expect("tier3 clock"),
            codec: Box::new(fw_codec),
            runtime_config: TransportRuntimeConfig::default(),
            sim_control: None,
            replay_control: None,
        },
        Arc::clone(ws.hub()),
    )
    .expect("ser0 session");
    ws.register_source_session(ser_session)
        .expect("register ser0 session");

    state
        .set_raw_recorder(RawRecorderRequestDto {
            source_id: "udp0".into(),
            enabled: true,
            output_path: Some(ddraw_guard.path().to_string_lossy().into_owned()),
        })
        .expect("enable raw .ddraw recorder on udp0");

    let started_udp = state.start_source("udp0".into()).expect("start udp0");
    assert_eq!(started_udp.source_id, "udp0");
    let started_ser = state.start_source("ser0".into()).expect("start ser0");
    assert_eq!(started_ser.source_id, "ser0");

    wait_until(
        || {
            let sources = ws.list_sources();
            let udp_running = sources
                .iter()
                .any(|s| s.source_id.as_str() == "udp0" && s.state == SourceRuntimeState::Running);
            let ser_running = sources
                .iter()
                .any(|s| s.source_id.as_str() == "ser0" && s.state == SourceRuntimeState::Running);
            udp_running && ser_running
        },
        Duration::from_secs(3),
        "udp0 and ser0 workers running",
    )
    .await;

    let udp_id = SourceId::new("udp0");
    let ser_id = SourceId::new("ser0");
    let u0_entry = ws.find_channel(&udp_id, "u0").expect("udp0/u0");
    let temp_entry = ws.find_channel(&ser_id, "temp").expect("ser0/temp");
    let mode_entry = ws.find_channel(&ser_id, "mode").expect("ser0/mode");
    let sim_ch0_entry = ws.find_channel(&SourceId::new("sim0"), "ch0").expect("sim0/ch0");

    // Pump 250 JustFloat frames (25 bursts x 10 frames) + 150 FireWater CSV lines concurrently.
    let mut pkt_idx = 0_u32;
    for _ in 0..25 {
        let mut datagram = Vec::with_capacity(10 * 12);
        for _ in 0..10 {
            let v0 = (pkt_idx as f32) * 0.5;
            let v1 = -v0;
            datagram.extend_from_slice(&encode_justfloat_datagram(&[v0, v1]));
            pkt_idx += 1;
        }
        udp_peer
            .send_to(&datagram, udp_bind)
            .await
            .expect("virtual UDP send");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }

    for idx in 0..150_u32 {
        let line = format!("fw:{:.2},{}\n", 25.0 + (idx as f32) * 0.1, idx % 4);
        ser_peer
            .writer
            .write_all(line.as_bytes())
            .await
            .expect("virtual serial write");
        if idx % 25 == 24 {
            ser_peer.writer.flush().await.expect("virtual serial flush");
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }
    ser_peer.writer.flush().await.expect("virtual serial final flush");

    wait_until(
        || {
            u0_entry.total_samples() >= 250
                && temp_entry.total_samples() >= 150
                && mode_entry.total_samples() >= 150
                && sim_ch0_entry.total_samples() >= 1_000
        },
        Duration::from_secs(5),
        "concurrent sim0 (>=1000), udp0/u0 (>=250), ser0/temp (>=150) & ser0/mode (>=150)",
    )
    .await;

    let sink = Arc::new(CapturingSink::new());
    state.data_bridge().attach_sink(Arc::clone(&sink) as Arc<dyn FrameSink>);

    state
        .upsert_scope_viewport(ScopeViewportConfigDto {
            scope_id: 1,
            title: Some("Fused Sim + UDP + Serial".into()),
            canvas_width_px: 1024,
            time_span_ns: 5_000_000_000.0,
            pre_trigger_ratio: 0.25,
            pan_offset_ns: 0.0,
            sweep_mode: TriggerSweepModeDto::Roll,
            auto_timeout_ms: 100,
            trigger: None,
            tracks: vec![
                ScopeTrackBindingDto {
                    channel_id: sim_ch0_entry.id().0,
                    lane: ScopeLaneKindDto::Analog,
                    visible: true,
                    y_scale: None,
                    y_offset: None,
                    color_rgba: None,
                },
                ScopeTrackBindingDto {
                    channel_id: u0_entry.id().0,
                    lane: ScopeLaneKindDto::Analog,
                    visible: true,
                    y_scale: None,
                    y_offset: None,
                    color_rgba: None,
                },
                ScopeTrackBindingDto {
                    channel_id: temp_entry.id().0,
                    lane: ScopeLaneKindDto::Analog,
                    visible: true,
                    y_scale: None,
                    y_offset: None,
                    color_rgba: None,
                },
                ScopeTrackBindingDto {
                    channel_id: mode_entry.id().0,
                    lane: ScopeLaneKindDto::StateRibbon,
                    visible: true,
                    y_scale: None,
                    y_offset: None,
                    color_rgba: None,
                },
            ],
            sync_group_id: None,
            paused: false,
        })
        .expect("upsert fused scope");

    let _ = state
        .step_and_push_ddsf_frame(host_monotonic_ns())
        .expect("step_and_push_ddsf_frame");
    let (_seq, frame_bytes) = sink.last_frame().expect("captured fused frame");
    let reader = DdsfZeroCopyReader::new(&frame_bytes).expect("parse fused DDSF");
    let pkt = reader.scopes().next().expect("scope 1 packet");
    let track_ids: Vec<u32> = pkt.tracks().map(|t| t.header().channel_id).collect();
    assert!(track_ids.contains(&sim_ch0_entry.id().0));
    assert!(track_ids.contains(&u0_entry.id().0));
    assert!(track_ids.contains(&temp_entry.id().0));

    state
        .set_raw_recorder(RawRecorderRequestDto {
            source_id: "udp0".into(),
            enabled: false,
            output_path: None,
        })
        .expect("disable raw recorder on udp0");
    state.stop_source("udp0".into()).await.expect("stop udp0");
    state.stop_source("ser0".into()).await.expect("stop ser0");
    drop(ser_peer);

    let ddraw_reader = DdrawReader::open(ddraw_guard.path()).expect("open .ddraw");
    assert_eq!(ddraw_reader.header().version, DDRAW_VERSION);
    let mut rx_records = 0_usize;
    for rec in ddraw_reader {
        let rec = rec.expect("valid CRC32 .ddraw record");
        if rec.direction == RawTapDirection::Rx {
            rx_records += 1;
        }
    }
    assert!(rx_records > 0, "expected >= 1 RX record in .ddraw file");

    state
        .create_source(CreateSourceRequestDto {
            source_id: "replay0".into(),
            display_name: Some("UDP Replay".into()),
            transport: TransportConfigDto::RawFileReplay {
                file_path: ddraw_guard.path().to_string_lossy().into_owned(),
                full_speed_offline: false,
                playback_speed: 2.0,
                loop_playback: true,
                nominal_byte_rate: Some(65_536.0),
            },
            clock: ClockConfigDto::Tier3HostArrival,
            protocol: ProtocolConfigDto::JustFloat {
                channel_names: vec!["r0".into(), "r1".into()],
            },
            auto_start: true,
        })
        .expect("create and start replay0");

    let r0_entry = ws
        .find_channel(&SourceId::new("replay0"), "r0")
        .expect("replay0/r0");
    wait_until(
        || r0_entry.total_samples() >= 20,
        Duration::from_secs(5),
        "replay0/r0 initial decoded samples",
    )
    .await;

    let samples_before_seek = r0_entry.total_samples();
    state
        .seek_replay_source(SeekReplayRequestDto {
            source_id: "replay0".into(),
            offset_ratio: 0.25,
            paused: Some(false),
            playback_speed: Some(4.0),
        })
        .expect("seek replay0");

    wait_until(
        || r0_entry.total_samples() > samples_before_seek,
        Duration::from_secs(5),
        "replay0/r0 decoded additional samples after seek",
    )
    .await;

    ws.shutdown().await.expect("workspace shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m4_t07_single_glitch_latch_chunk_pinning_rollover_10000x_zoom_and_rearm() {
    let state = build_live_desktop_state(
        Handle::current(),
        DesktopRuntimeConfig {
            quota_bytes: 16 * 1024 * 1024,
            seed_default_sim_source: false,
        },
    )
    .expect("build_live_desktop_state");
    let ws = state.workspace().expect("live workspace");

    state
        .create_source(CreateSourceRequestDto {
            source_id: "sim0".into(),
            display_name: Some("High-Rate Sim".into()),
            transport: TransportConfigDto::Sim {
                sample_rate_hz: 2_000_000.0,
                analog_channels: 4,
                enable_glitch: false,
                glitch_period_samples: 4_000_000,
            },
            clock: ClockConfigDto::Tier1HardwareCounter {
                tick_hz: 2_000_000.0,
                counter_bits: 32,
            },
            protocol: ProtocolConfigDto::SimPreset,
            auto_start: false,
        })
        .expect("create sim0");

    let tree = state.get_channel_tree().expect("get_channel_tree");
    let sim_group = tree
        .sources
        .iter()
        .find(|g| g.source.source_id == "sim0")
        .expect("sim0 group");
    let ch_id = |name: &str| -> u32 {
        sim_group
            .channels
            .iter()
            .find(|c| c.local_path == name)
            .unwrap()
            .channel_id
    };
    let ch0 = ch_id("ch0");
    let ch3 = ch_id("ch3");
    let fault_pin = ch_id("fault_pin");

    let sink = Arc::new(CapturingSink::new());
    state.data_bridge().attach_sink(Arc::clone(&sink) as Arc<dyn FrameSink>);

    let scope1_cfg = ScopeViewportConfigDto {
        scope_id: 1,
        title: Some("Single Glitch Latch".into()),
        canvas_width_px: 1280,
        time_span_ns: 20_000_000.0,
        pre_trigger_ratio: 0.5,
        pan_offset_ns: 0.0,
        sweep_mode: TriggerSweepModeDto::Single,
        auto_timeout_ms: 100,
        trigger: Some(TriggerConditionDto::Edge {
            channel_id: ch3,
            level: 60.0,
            hysteresis: 2.0,
            slope: EdgeSlopeDto::Rising,
        }),
        tracks: vec![
            ScopeTrackBindingDto {
                channel_id: ch3,
                lane: ScopeLaneKindDto::Analog,
                visible: true,
                y_scale: None,
                y_offset: None,
                color_rgba: None,
            },
            ScopeTrackBindingDto {
                channel_id: fault_pin,
                lane: ScopeLaneKindDto::Digital,
                visible: true,
                y_scale: None,
                y_offset: None,
                color_rgba: None,
            },
        ],
        sync_group_id: None,
        paused: false,
    };
    state
        .upsert_scope_viewport(scope1_cfg.clone())
        .expect("upsert scope 1");

    state
        .upsert_scope_viewport(ScopeViewportConfigDto {
            scope_id: 2,
            title: Some("Independent Roll".into()),
            canvas_width_px: 1280,
            time_span_ns: 20_000_000.0,
            pre_trigger_ratio: 0.5,
            pan_offset_ns: 0.0,
            sweep_mode: TriggerSweepModeDto::Roll,
            auto_timeout_ms: 100,
            trigger: None,
            tracks: vec![ScopeTrackBindingDto {
                channel_id: ch0,
                lane: ScopeLaneKindDto::Analog,
                visible: true,
                y_scale: None,
                y_offset: None,
                color_rgba: None,
            }],
            sync_group_id: None,
            paused: false,
        })
        .expect("upsert scope 2");

    state.start_source("sim0".into()).expect("start sim0");

    let sim0_id = SourceId::new("sim0");
    let ch3_entry = ws.find_channel(&sim0_id, "ch3").expect("sim0/ch3");
    let warm_deadline = Instant::now() + Duration::from_secs(5);
    while ch3_entry.total_samples() < 2_000 {
        assert!(Instant::now() < warm_deadline, "warm-up timed out");
        let _ = state
            .step_and_push_ddsf_frame(host_monotonic_ns())
            .expect("step warm");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let _ = state
        .step_and_push_ddsf_frame(host_monotonic_ns())
        .expect("step armed");
    assert_eq!(
        ws.scope_summary(&ScopeId(1)).unwrap().runtime_state,
        TriggerRuntimeState::Armed
    );

    state
        .send_downlink(SendDownlinkRequestDto {
            source_id: "sim0".into(),
            payload: DownlinkPayloadDto::AsciiText {
                text: "GLITCH".into(),
                append_newline: true,
            },
            priority: TxPriorityDto::High,
            coalesce_key: None,
        })
        .expect("send GLITCH");

    let latch_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let _ = state
            .step_and_push_ddsf_frame(host_monotonic_ns())
            .expect("step latch");
        let s1 = ws.scope_summary(&ScopeId(1)).unwrap();
        if s1.runtime_state == TriggerRuntimeState::LatchedPinned {
            assert!(s1.is_pinned);
            assert!(s1.pinned_bytes > 0);
            break;
        }
        assert!(
            Instant::now() < latch_deadline,
            "scope1 did not reach LatchedPinned; state={:?}",
            s1.runtime_state
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    let roll_win_at_latch = ws.scope_summary(&ScopeId(2)).unwrap().window.unwrap();
    let trig_ts_before_rollover = ws.scope_summary(&ScopeId(1)).unwrap().trigger_time_ns.unwrap();

    state
        .send_downlink(SendDownlinkRequestDto {
            source_id: "sim0".into(),
            payload: DownlinkPayloadDto::AsciiText {
                text: "RATE=10000000".into(),
                append_newline: true,
            },
            priority: TxPriorityDto::High,
            coalesce_key: None,
        })
        .expect("send RATE=10000000");

    let rollover_deadline = Instant::now() + Duration::from_secs(20);
    while entry_evicted_samples(&ch3_entry) < 3_000_000 {
        assert!(
            Instant::now() < rollover_deadline,
            "timed out waiting for >= 3M evicted_samples (got {})",
            entry_evicted_samples(&ch3_entry)
        );
        let _ = state
            .step_and_push_ddsf_frame(host_monotonic_ns())
            .expect("step rollover");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let _ = state
        .step_and_push_ddsf_frame(host_monotonic_ns())
        .expect("step post-rollover");
    let roll_win_after = ws.scope_summary(&ScopeId(2)).unwrap().window.unwrap();
    assert!(roll_win_after.win_t_start_ns > roll_win_at_latch.win_t_start_ns);
    assert_eq!(
        ws.scope_summary(&ScopeId(1)).unwrap().trigger_time_ns,
        Some(trig_ts_before_rollover)
    );

    // Zoom 10,000x (20 ms -> 2,000 ns) via control-plane DTO while staying LatchedPinned.
    let mut zoomed_cfg = scope1_cfg;
    zoomed_cfg.time_span_ns = 2_000.0;
    state
        .upsert_scope_viewport(zoomed_cfg)
        .expect("zoom scope 1 to 2us");

    let _ = state
        .step_and_push_ddsf_frame(host_monotonic_ns())
        .expect("pack zoomed DDSF");
    let (_seq, zoomed_bytes) = sink.last_frame().expect("captured zoomed frame");
    let reader = DdsfZeroCopyReader::new(&zoomed_bytes).expect("parse zoomed DDSF");
    assert_ne!(reader.header().flags & DDSF_FLAG_PINNED_ACTIVE, 0);

    let mut found_ch3_glitch = false;
    for scope_view in reader.scopes() {
        if scope_view.header().scope_id != 1 {
            continue;
        }
        assert_eq!(
            scope_view.header().trigger_state,
            TriggerRuntimeState::LatchedPinned.as_u8()
        );
        assert_eq!(scope_view.header().win_t_span_ns, 2_000);
        for tr in scope_view.tracks() {
            if tr.header().channel_id == ch3 {
                let stats = tr.stats().expect("zoomed ch3 stats");
                assert!(stats.max > 95.0, "expected >95V glitch, got {}", stats.max);
                let pts = tr
                    .raw_points()
                    .expect("2us zoomed view must use AnalogRawXY");
                assert!(
                    pts.iter().any(|p| p.y_val > 95.0),
                    "AnalogRawXY points must contain >95V peak"
                );
                found_ch3_glitch = true;
            }
        }
    }
    assert!(found_ch3_glitch, "ch3 track missing from zoomed frame");

    state.rearm_scope(1).expect("rearm_scope(1)");
    let s1_rearmed = ws.scope_summary(&ScopeId(1)).unwrap();
    assert!(!s1_rearmed.is_pinned);
    assert_eq!(s1_rearmed.pinned_bytes, 0);
    assert_eq!(ws.quota_stats().pinned_bytes, 0);

    ws.shutdown().await.expect("workspace shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m4_t07_sync_group_dto_atomic_alignment_and_closed_loop_downlink_modulation() {
    let state = build_live_desktop_state(
        Handle::current(),
        DesktopRuntimeConfig {
            quota_bytes: 64 * 1024 * 1024,
            seed_default_sim_source: true,
        },
    )
    .expect("build_live_desktop_state");
    let ws = state.workspace().expect("live workspace");

    let tree = state.get_channel_tree().expect("get_channel_tree");
    let ch0 = tree
        .sources
        .iter()
        .find(|g| g.source.source_id == "sim0")
        .unwrap()
        .channels[0]
        .channel_id;

    for scope_id in [1_u32, 2_u32] {
        state
            .upsert_scope_viewport(ScopeViewportConfigDto {
                scope_id,
                title: Some(format!("Synced Scope {scope_id}")),
                canvas_width_px: 1024,
                time_span_ns: 20_000_000.0,
                pre_trigger_ratio: 0.5,
                pan_offset_ns: 0.0,
                sweep_mode: TriggerSweepModeDto::Roll,
                auto_timeout_ms: 100,
                trigger: None,
                tracks: vec![ScopeTrackBindingDto {
                    channel_id: ch0,
                    lane: ScopeLaneKindDto::Analog,
                    visible: true,
                    y_scale: None,
                    y_offset: None,
                    color_rgba: None,
                }],
                sync_group_id: Some(1),
                paused: false,
            })
            .expect("upsert synced scope");
    }

    let ch0_entry = ws.find_channel(&SourceId::new("sim0"), "ch0").expect("sim0/ch0");
    wait_until(
        || ch0_entry.total_samples() >= 2_000,
        Duration::from_secs(5),
        "sim0/ch0 accumulated >= 2,000 samples",
    )
    .await;
    let _ = ws.tick_scopes(host_monotonic_ns());

    state
        .update_sync_group(SyncGroupUpdateDto {
            group_id: 1,
            origin_scope_id: 1,
            time_span_ns: 500_000.0,
            center_offset_ns: -25_000.0,
            cursor_a_ratio: Some(0.2),
            cursor_b_ratio: Some(0.8),
        })
        .expect("update_sync_group");

    let sync_state = ws
        .sync_group_state(SyncGroupId(1))
        .expect("sync group 1 state");
    assert_eq!(sync_state.time_span_ns, 500_000);
    assert_eq!(sync_state.center_offset_ns, -25_000);
    assert_eq!(sync_state.cursor_a_ratio, Some(0.2));
    assert_eq!(sync_state.cursor_b_ratio, Some(0.8));

    let win1 = ws
        .scope_summary(&ScopeId(1))
        .unwrap()
        .window
        .expect("scope 1 window");
    let win2 = ws
        .scope_summary(&ScopeId(2))
        .unwrap()
        .window
        .expect("scope 2 window");
    assert_eq!(win1.win_t_span_ns, 500_000);
    assert_eq!(win2.win_t_span_ns, 500_000);
    assert_eq!(win1.win_t_start_ns, win2.win_t_start_ns);

    state
        .send_downlink(SendDownlinkRequestDto {
            source_id: "sim0".into(),
            payload: DownlinkPayloadDto::KeyValue {
                key: "kp".into(),
                value: 2.25,
            },
            priority: TxPriorityDto::Normal,
            coalesce_key: Some("param:kp".into()),
        })
        .expect("send kp=2.25");

    state
        .send_downlink(SendDownlinkRequestDto {
            source_id: "sim0".into(),
            payload: DownlinkPayloadDto::KeyValue {
                key: "freq_hz".into(),
                value: 250.0,
            },
            priority: TxPriorityDto::Normal,
            coalesce_key: Some("param:freq_hz".into()),
        })
        .expect("send freq_hz=250");

    let sim_ctrl = ws
        .sim_control(&SourceId::new("sim0"))
        .expect("sim_control lookup")
        .expect("SimControlHandle present");

    wait_until(
        || {
            let kp_ok = (sim_ctrl.kp_gain() - 2.25).abs() < 1e-4;
            let freq_ok = sim_ctrl
                .signal_freq_hz()
                .map(|f| (f - 250.0).abs() < 1e-6)
                .unwrap_or(false);
            kp_ok && freq_ok
        },
        Duration::from_secs(3),
        "sim_control reflected kp=2.25 and freq_hz=250.0",
    )
    .await;

    ws.shutdown().await.expect("workspace shutdown");
}
