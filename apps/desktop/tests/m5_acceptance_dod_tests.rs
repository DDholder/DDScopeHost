//! Phase 1 Milestone M5 (`T08` / Gate 4) Full-Stack Acceptance Tests (DoD #1 ~ #4)
//! Exercising `DesktopBridgeState` + `ZeroCopyDataBridge` + `CoreWorkspace`.

use std::collections::VecDeque;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::UdpSocket;
use tokio::runtime::Handle;

use ddscope_core::clock::{ClockMode, StreamClockSynchronizer};
use ddscope_core::ddsf::{DdsfBlockKind, DdsfZeroCopyReader, DDSF_FLAG_PINNED_ACTIVE};
use ddscope_core::protocol::{
    default_sim_state_labels, sim_ctrl_state_labels, FireWaterColumnSpec, FireWaterConfig,
    FireWaterCsvCodec, JUSTFLOAT_TAIL_MAGIC,
};
use ddscope_core::scope::TriggerRuntimeState;
use ddscope_core::storage::{ChannelEntry, ChannelStore};
use ddscope_core::transport::{
    host_monotonic_ns, SerialConfig, SerialIoProvider, SerialPortStreams, SerialTransport,
    TransportRuntimeConfig, VirtualSerialPair,
};
use ddscope_core::types::{ChannelDescriptor, ScalarSlice, ScalarType, ScopeId, SourceId, CHUNK_LEN};
use ddscope_core::workspace::{SourceRuntimeState, SourceSession, SourceSessionComponents};
use ddscope_desktop_lib::commands::{
    ClockConfigDto, CreateSourceRequestDto, DownlinkPayloadDto, EdgeSlopeDto, ProtocolConfigDto,
    ScopeLaneKindDto, ScopeTrackBindingDto, ScopeViewportConfigDto, SendDownlinkRequestDto,
    TransportConfigDto, TriggerConditionDto, TriggerSweepModeDto, TxPriorityDto,
};
use ddscope_desktop_lib::data_bridge::{DataBridgeError, DataBridgeModeDto, FrameSink};
use ddscope_desktop_lib::runtime::{build_live_desktop_state, DesktopRuntimeConfig};

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

fn entry_evicted_samples(entry: &ChannelEntry) -> u64 {
    match entry.store() {
        ChannelStore::Scalar(s) => s.read().evicted_samples(),
        ChannelStore::Event(e) => e.read().evicted_samples(),
        ChannelStore::Frame { .. } => 0,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dod_1_concurrent_multi_source_sim_udp_and_serial_firewater_pll() {
    let state = build_live_desktop_state(
        Handle::current(),
        DesktopRuntimeConfig {
            quota_bytes: 64 * 1024 * 1024,
            seed_default_sim_source: true,
        },
    )
    .expect("build_live_desktop_state");
    let ws = state.workspace().expect("live workspace");

    // Source 2: Real loopback 127.0.0.1 UDP JustFloat source via DesktopBridgeState DTO.
    let mut udp_addr: Option<SocketAddr> = None;
    for _ in 0..5 {
        let probe = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind UDP probe");
        let addr = probe.local_addr().unwrap();
        drop(probe);
        if state
            .create_source(CreateSourceRequestDto {
                source_id: "udp1".into(),
                display_name: Some("Loopback UDP Telemetry".into()),
                transport: TransportConfigDto::Udp {
                    bind_addr: addr.to_string(),
                    multicast_group: None,
                    recv_buffer_bytes: 262_144,
                    remote_tx_addr: None,
                },
                clock: ClockConfigDto::Tier2SoftwarePll {
                    nominal_sample_rate_hz: Some(25_000.0),
                },
                protocol: ProtocolConfigDto::JustFloat {
                    channel_names: vec!["vdc".into(), "idc".into()],
                },
                auto_start: true,
            })
            .is_ok()
        {
            udp_addr = Some(addr);
            break;
        }
    }
    let udp_addr = udp_addr.expect("create udp1 loopback source");

    // Source 3: VirtualSerialPair + FireWaterCsvCodec + Tier2SoftwarePll under bursty traffic.
    let serial_pair = VirtualSerialPair::new("VCOM_DOD1");
    let mut ser_peer = serial_pair.take_peer().expect("virtual serial peer");
    let ser_transport = SerialTransport::with_provider(
        SerialConfig {
            port_name: "VCOM_DOD1".into(),
            baud_rate: 2_000_000,
            ..Default::default()
        },
        serial_pair.provider(),
    )
    .expect("construct SerialTransport");
    let fw_codec = FireWaterCsvCodec::new(FireWaterConfig::with_columns(
        "uart1",
        vec![
            FireWaterColumnSpec::f32("temp"),
            FireWaterColumnSpec::state_enum("fsm", default_sim_state_labels()),
        ],
    ))
    .expect("construct FireWaterCsvCodec");
    let uart_session = SourceSession::from_components(
        SourceSessionComponents {
            source_id: SourceId::new("uart1"),
            display_name: Some("Serial FireWater PLL".into()),
            transport: Box::new(ser_transport),
            clock: StreamClockSynchronizer::new(ClockMode::Tier2SoftwarePll {
                nominal_sample_rate_hz: Some(10_000.0),
            })
            .expect("Tier2 PLL"),
            codec: Box::new(fw_codec),
            runtime_config: TransportRuntimeConfig::default(),
            sim_control: None,
            replay_control: None,
        },
        Arc::clone(ws.hub()),
    )
    .expect("uart1 SourceSession");
    ws.register_source_session(uart_session)
        .expect("register uart1");
    state.start_source("uart1".into()).expect("start uart1");

    wait_until(
        || {
            let sources = ws.list_sources();
            let sim_ok = sources
                .iter()
                .any(|s| s.source_id.as_str() == "sim0" && s.state == SourceRuntimeState::Running);
            let udp_ok = sources
                .iter()
                .any(|s| s.source_id.as_str() == "udp1" && s.state == SourceRuntimeState::Running);
            let uart_ok = sources
                .iter()
                .any(|s| s.source_id.as_str() == "uart1" && s.state == SourceRuntimeState::Running);
            sim_ok && udp_ok && uart_ok
        },
        Duration::from_secs(3),
        "all 3 heterogeneous sources running concurrently",
    )
    .await;

    let sender = UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("bind loopback UDP sender");

    // Pump bursty JustFloat datagrams over OS loopback AND bursty FireWater CSV packets simultaneously.
    for burst in 0..20_u32 {
        let mut dgram = Vec::with_capacity(12 * 10);
        for k in 0..10_u32 {
            let vdc = 400.0 + (burst * 10 + k) as f32 * 0.25;
            let idc = 10.0 + (k as f32) * 0.5;
            dgram.extend_from_slice(&encode_justfloat_datagram(&[vdc, idc]));
        }
        sender
            .send_to(&dgram, udp_addr)
            .await
            .expect("UDP loopback send_to");

        let mut csv_burst = String::new();
        for k in 0..15_u32 {
            let idx = burst * 15 + k;
            let temp = 25.0 + (idx as f32) * 0.1;
            let fsm = idx % 4;
            csv_burst.push_str(&format!("telemetry:{temp:.2},{fsm}\n"));
        }
        ser_peer
            .writer
            .write_all(csv_burst.as_bytes())
            .await
            .expect("serial burst write");
        ser_peer.writer.flush().await.expect("serial burst flush");
        tokio::time::sleep(Duration::from_millis(3)).await;
    }

    let sim_ch0 = ws.find_channel(&SourceId::new("sim0"), "ch0").unwrap();
    let udp_vdc = ws.find_channel(&SourceId::new("udp1"), "vdc").unwrap();
    let uart_temp = ws.find_channel(&SourceId::new("uart1"), "temp").unwrap();
    let uart_fsm = ws.find_channel(&SourceId::new("uart1"), "fsm").unwrap();

    wait_until(
        || {
            sim_ch0.total_samples() >= 2_000
                && udp_vdc.total_samples() >= 200
                && uart_temp.total_samples() >= 300
                && uart_fsm.total_samples() > 0
        },
        Duration::from_secs(5),
        "all 3 sources ingested expected samples",
    )
    .await;

    // Verify channel tree discovery across all 3 heterogeneous sources without cross-talk.
    let tree = state.get_channel_tree().expect("get_channel_tree");
    assert_eq!(tree.sources.len(), 3);
    assert!(tree.sources.iter().any(|s| s.source.source_id == "sim0"));
    assert!(tree.sources.iter().any(|s| s.source.source_id == "udp1"));
    assert!(tree.sources.iter().any(|s| s.source.source_id == "uart1"));

    // Verify strictly monotonic reconstructed timestamps under bursty Tier2 Software PLL on uart1/temp.
    let temp_count = uart_temp.total_samples();
    uart_temp
        .with_scalar_read(|series| {
            let mut prev = 0_u64;
            for i in 0..temp_count {
                let (ts, val) = series.get_sample_by_index(i).unwrap();
                assert!(ts >= prev, "PLL regression at {i}: {ts} < {prev}");
                assert!((val - (25.0 + i as f64 * 0.1)).abs() < 0.05);
                prev = ts;
            }
        })
        .unwrap();

    ws.shutdown().await.expect("workspace shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dod_2_mso_20s_three_lane_glitch_and_state_ribbon_alignment() {
    let state = build_live_desktop_state(
        Handle::current(),
        DesktopRuntimeConfig {
            quota_bytes: 512 * 1024 * 1024,
            seed_default_sim_source: false,
        },
    )
    .expect("build_live_desktop_state");
    let ws = state.workspace().expect("live workspace");
    let hub = ws.hub();

    let ch3 = hub
        .register_or_get(ChannelDescriptor::new_scalar(
            "mso_dod2",
            "v_bus",
            ScalarType::F32,
        ))
        .unwrap();
    let fault_pin = hub
        .register_or_get(ChannelDescriptor::new_bool("mso_dod2", "fault_pin"))
        .unwrap();
    let ctrl_state = hub
        .register_or_get(ChannelDescriptor::new_state_machine(
            "mso_dod2",
            "ctrl_state",
            sim_ctrl_state_labels(),
        ))
        .unwrap();

    // Populate >20 s @ 2 MHz (40,042,496 samples) with synchronized fault at sample 35,000,000 (17.5s).
    const TOTAL_CHUNKS: usize = 611;
    const DT_NS: u64 = 500;
    const T0_NS: u64 = 1_000_000_000_000;
    const FAULT_IDX: usize = 35_000_000;
    const SPAN_20S: u64 = 20_000_000_000;

    let mut ts_buf = vec![0_u64; CHUNK_LEN];
    let mut analog_buf = vec![24.0_f32; CHUNK_LEN];
    let mut digital_buf = vec![false; CHUNK_LEN];
    let dense_pwm: Vec<bool> = (0..CHUNK_LEN).map(|i| (i / 8) % 2 == 1).collect();

    for c in 0..TOTAL_CHUNKS {
        let base_idx = c * CHUNK_LEN;
        for (i, ts) in ts_buf.iter_mut().enumerate() {
            *ts = T0_NS + ((base_idx + i) as u64) * DT_NS;
        }
        analog_buf.fill(24.0);
        if base_idx < 30_000_000 {
            digital_buf.copy_from_slice(&dense_pwm);
        } else {
            digital_buf.fill(false);
        }
        if FAULT_IDX >= base_idx && FAULT_IDX < base_idx + CHUNK_LEN {
            let local = FAULT_IDX - base_idx;
            analog_buf[local] = 104.0;
            digital_buf[local] = true;
        }
        ch3.append_scalar_batch(&ts_buf, ScalarSlice::F32(&analog_buf))
            .unwrap();
        fault_pin
            .append_scalar_batch(&ts_buf, ScalarSlice::Bool(&digital_buf))
            .unwrap();
    }

    let t_fault = T0_NS + (FAULT_IDX as u64) * DT_NS;
    ctrl_state
        .append_state_events(&[T0_NS, T0_NS + 1_000_000_000, t_fault], &[0, 2, 3])
        .unwrap();
    let last_ts = T0_NS + ((TOTAL_CHUNKS * CHUNK_LEN - 1) as u64) * DT_NS;

    let sink = Arc::new(CapturingSink::new());
    state
        .data_bridge()
        .attach_sink(Arc::clone(&sink) as Arc<dyn FrameSink>);

    state
        .upsert_scope_viewport(ScopeViewportConfigDto {
            scope_id: 1,
            title: Some("20s MSO Full-Stack".into()),
            canvas_width_px: 1280,
            time_span_ns: SPAN_20S as f64,
            pre_trigger_ratio: 0.5,
            pan_offset_ns: 0.0,
            sweep_mode: TriggerSweepModeDto::Roll,
            auto_timeout_ms: 100,
            trigger: None,
            tracks: vec![
                ScopeTrackBindingDto {
                    channel_id: ch3.id().0,
                    lane: ScopeLaneKindDto::Analog,
                    visible: true,
                    y_scale: None,
                    y_offset: None,
                    color_rgba: None,
                },
                ScopeTrackBindingDto {
                    channel_id: fault_pin.id().0,
                    lane: ScopeLaneKindDto::Digital,
                    visible: true,
                    y_scale: None,
                    y_offset: None,
                    color_rgba: None,
                },
                ScopeTrackBindingDto {
                    channel_id: ctrl_state.id().0,
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
        .expect("upsert 3-lane MSO viewport");

    state
        .step_and_push_ddsf_frame(last_ts)
        .expect("step_and_push_ddsf_frame");
    let (_, frame_bytes) = sink.last_frame().expect("DDSF frame pushed to sink");
    let reader = DdsfZeroCopyReader::new(&frame_bytes).expect("parse DDSF frame");
    let scope = reader.scopes().next().expect("scope 1 in DDSF");
    let win_start = scope.header().win_t_start_ns;
    let fault_x_norm = ((t_fault - win_start) as f64 / SPAN_20S as f64) as f32;

    let mut analog_peak_x: Option<f32> = None;
    let mut digital_glitch_x: Option<f32> = None;
    let mut ribbon_fault_x: Option<f32> = None;

    for tr in scope.tracks() {
        match DdsfBlockKind::from_u8(tr.header().block_kind) {
            Some(DdsfBlockKind::AnalogM4Column) => {
                let cols = tr.m4_columns().unwrap();
                let peak_col = cols.iter().find(|c| c.v_max > 95.0).expect("+104V analog spike column");
                analog_peak_x = Some(peak_col.x_norm);
            }
            Some(DdsfBlockKind::DigitalEdges) => {
                let edges = tr.digital_edges().unwrap();
                assert_eq!(
                    tr.header().digital_init_level ^ ((edges.len() & 1) as u8),
                    0,
                    "final logic parity must match false end state"
                );
                let g = edges
                    .iter()
                    .copied()
                    .find(|x| (*x - fault_x_norm).abs() <= 0.002)
                    .expect("1-sample digital fault glitch edge preserved");
                digital_glitch_x = Some(g);
            }
            Some(DdsfBlockKind::StateRibbons) => {
                let ribbons = tr.state_ribbons().unwrap();
                let closed = ribbons.iter().find(|r| r.state_id == 2);
                let fault = ribbons.iter().find(|r| r.state_id == 3);
                assert!(closed.is_some() && fault.is_some(), "expected [CLOSED_LOOP]->[OCP_FAULT]");
                ribbon_fault_x = Some(fault.unwrap().x0_norm);
            }
            _ => {}
        }
    }

    let ax = analog_peak_x.expect("analog peak x");
    let dx = digital_glitch_x.expect("digital glitch x");
    let rx = ribbon_fault_x.expect("state fault x");
    assert!(
        (ax - rx).abs() < 2e-3 && (dx - rx).abs() < 2e-3,
        "MSO 3-lane fault coordinates misaligned: analog={ax}, digital={dx}, state={rx}"
    );

    ws.shutdown().await.expect("workspace shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dod_3_independent_single_latch_across_2x_quota_rollover_10000x_zoom_and_rearm() {
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
            display_name: Some("High-Speed FOC".into()),
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
        .unwrap();
    let ch_id = |name: &str| {
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
    let ctrl_state = ch_id("ctrl_state");

    let sink = Arc::new(CapturingSink::new());
    state
        .data_bridge()
        .attach_sink(Arc::clone(&sink) as Arc<dyn FrameSink>);

    let scope1_cfg = ScopeViewportConfigDto {
        scope_id: 1,
        title: Some("Scope 1 Single Glitch Pin".into()),
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
    };
    state
        .upsert_scope_viewport(scope1_cfg.clone())
        .expect("upsert Scope 1");
    state
        .upsert_scope_viewport(ScopeViewportConfigDto {
            scope_id: 2,
            title: Some("Scope 2 Independent Roll".into()),
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
        .expect("upsert Scope 2");

    state.start_source("sim0".into()).expect("start sim0");
    let ch3_entry = ws.find_channel(&SourceId::new("sim0"), "ch3").unwrap();
    wait_until(
        || ch3_entry.total_samples() >= 2_000,
        Duration::from_secs(5),
        "sim0 warmed up",
    )
    .await;
    let _ = state.step_and_push_ddsf_frame(host_monotonic_ns()).unwrap();

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
        .expect("inject GLITCH");

    wait_until(
        || {
            let _ = state.step_and_push_ddsf_frame(host_monotonic_ns());
            ws.scope_summary(&ScopeId(1))
                .is_ok_and(|s| s.runtime_state == TriggerRuntimeState::LatchedPinned)
        },
        Duration::from_secs(5),
        "Scope 1 latched & pinned",
    )
    .await;

    let roll_win_before = ws.scope_summary(&ScopeId(2)).unwrap().window.unwrap();
    let pinned_trig_ts = ws.scope_summary(&ScopeId(1)).unwrap().trigger_time_ns.unwrap();

    // Boost sample rate to 10 MHz and drive >2x quota (32+ MiB) rollover while Scope 1 stays pinned.
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
        .expect("boost RATE=10000000");

    let rollover_deadline = Instant::now() + Duration::from_secs(20);
    while entry_evicted_samples(&ch3_entry) < 4_500_000 {
        assert!(
            Instant::now() < rollover_deadline,
            "timed out waiting for >2x rollover eviction (evicted={})",
            entry_evicted_samples(&ch3_entry)
        );
        let _ = state.step_and_push_ddsf_frame(host_monotonic_ns()).unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let _ = state.step_and_push_ddsf_frame(host_monotonic_ns()).unwrap();
    let roll_win_after = ws.scope_summary(&ScopeId(2)).unwrap().window.unwrap();
    assert!(
        roll_win_after.win_t_start_ns > roll_win_before.win_t_start_ns,
        "Scope 2 Roll must continue advancing independently while Scope 1 is pinned"
    );
    assert_eq!(
        ws.scope_summary(&ScopeId(1)).unwrap().trigger_time_ns,
        Some(pinned_trig_ts)
    );

    // 10,000x zoom (20 ms -> 2 us) around T0 on pinned Scope 1.
    let mut zoomed_cfg = scope1_cfg;
    zoomed_cfg.time_span_ns = 2_000.0;
    state
        .upsert_scope_viewport(zoomed_cfg)
        .expect("10,000x zoom Scope 1");
    let _ = state.step_and_push_ddsf_frame(host_monotonic_ns()).unwrap();
    let (_, zoomed_bytes) = sink.last_frame().expect("captured zoomed frame");
    let reader = DdsfZeroCopyReader::new(&zoomed_bytes).expect("parse zoomed DDSF");
    assert_ne!(reader.header().flags & DDSF_FLAG_PINNED_ACTIVE, 0);

    let s1_view = reader
        .scopes()
        .find(|s| s.header().scope_id == 1)
        .expect("Scope 1 present");
    let mut found_ch3_peak = false;
    for tr in s1_view.tracks() {
        if tr.header().channel_id == ch3 {
            let pts = tr.raw_points().expect("2us zoomed view uses AnalogRawXY");
            assert!(pts.iter().any(|p| p.y_val > 95.0));
            found_ch3_peak = true;
        }
    }
    assert!(found_ch3_peak);

    // Rearm Scope 1 -> release all pinned bytes.
    state.rearm_scope(1).expect("rearm Scope 1");
    assert_eq!(ws.quota_stats().pinned_bytes, 0);

    ws.shutdown().await.expect("workspace shutdown");
}

struct PacedFifoShared {
    fifo: VecDeque<u8>,
    received_bytes: AtomicUsize,
    lines: Vec<(Instant, String)>,
    current_line: Vec<u8>,
    write_waker: Option<Waker>,
    flush_waker: Option<Waker>,
    shutdown: AtomicBool,
}

#[derive(Clone)]
struct Paced115200SerialProvider {
    shared: Arc<Mutex<PacedFifoShared>>,
    opened: Arc<AtomicBool>,
}

impl Paced115200SerialProvider {
    fn new() -> Self {
        let shared = Arc::new(Mutex::new(PacedFifoShared {
            fifo: VecDeque::with_capacity(16),
            received_bytes: AtomicUsize::new(0),
            lines: Vec::new(),
            current_line: Vec::new(),
            write_waker: None,
            flush_waker: None,
            shutdown: AtomicBool::new(false),
        }));

        // Dedicated sub-ms paced UART TX FIFO drainer at 115200 bps 8N1 (~86.8 us/byte).
        let bg = Arc::clone(&shared);
        std::thread::spawn(move || {
            const BYTE_INTERVAL_NS: u64 = 86_805;
            let mut next_due = Instant::now();
            loop {
                let byte_opt = {
                    let mut st = bg.lock();
                    if st.shutdown.load(Ordering::Acquire) {
                        break;
                    }
                    let b = st.fifo.pop_front();
                    if let Some(byte) = b {
                        st.received_bytes.fetch_add(1, Ordering::Release);
                        if byte == b'\n' {
                            let s = String::from_utf8_lossy(&st.current_line).into_owned();
                            st.current_line.clear();
                            st.lines.push((Instant::now(), s));
                        } else {
                            st.current_line.push(byte);
                        }
                        if st.fifo.len() < 16 {
                            if let Some(w) = st.write_waker.take() {
                                w.wake();
                            }
                        }
                        if st.fifo.is_empty() {
                            if let Some(w) = st.flush_waker.take() {
                                w.wake();
                            }
                        }
                    }
                    b
                };

                if byte_opt.is_some() {
                    next_due += Duration::from_nanos(BYTE_INTERVAL_NS);
                    let now = Instant::now();
                    if next_due > now {
                        while Instant::now() < next_due {
                            std::hint::spin_loop();
                        }
                    } else {
                        next_due = now;
                    }
                } else {
                    std::thread::sleep(Duration::from_micros(200));
                    next_due = Instant::now();
                }
            }
        });

        Self {
            shared,
            opened: Arc::new(AtomicBool::new(false)),
        }
    }

    fn received_bytes(&self) -> usize {
        self.shared.lock().received_bytes.load(Ordering::Acquire)
    }

    fn lines(&self) -> Vec<(Instant, String)> {
        self.shared.lock().lines.clone()
    }
}

impl Drop for Paced115200SerialProvider {
    fn drop(&mut self) {
        self.shared.lock().shutdown.store(true, Ordering::Release);
    }
}

struct IdleSerialReader(Arc<AtomicBool>);

impl AsyncRead for IdleSerialReader {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.0.load(Ordering::Acquire) {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    }
}

struct Paced115200Writer(Arc<Mutex<PacedFifoShared>>);

impl AsyncWrite for Paced115200Writer {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        let mut st = self.0.lock();
        if st.fifo.len() >= 16 {
            st.write_waker = Some(cx.waker().clone());
            return Poll::Pending;
        }
        let room = 16 - st.fifo.len();
        let take = room.min(buf.len());
        st.fifo.extend(&buf[..take]);
        Poll::Ready(Ok(take))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut st = self.0.lock();
        if st.fifo.is_empty() {
            Poll::Ready(Ok(()))
        } else {
            st.flush_waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.poll_flush(cx)
    }
}

impl SerialIoProvider for Paced115200SerialProvider {
    fn open<'a>(
        &'a self,
        config: &'a SerialConfig,
    ) -> Pin<Box<dyn Future<Output = ddscope_core::types::CoreResult<SerialPortStreams>> + Send + 'a>>
    {
        let shared = Arc::clone(&self.shared);
        let opened = Arc::clone(&self.opened);
        let port = config.port_name.clone();
        Box::pin(async move {
            opened.store(true, Ordering::Release);
            Ok(SerialPortStreams {
                reader: Box::new(IdleSerialReader(Arc::new(AtomicBool::new(false)))),
                writer: Box::new(Paced115200Writer(shared)),
                source_tag: Some(Arc::from(port)),
            })
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dod_4_closed_loop_downlink_tx_coalescing_under_115200_baud_backpressure() {
    let state = build_live_desktop_state(
        Handle::current(),
        DesktopRuntimeConfig {
            quota_bytes: 32 * 1024 * 1024,
            seed_default_sim_source: false,
        },
    )
    .expect("build_live_desktop_state");
    let ws = state.workspace().expect("live workspace");

    let paced_serial = Paced115200SerialProvider::new();
    let transport = SerialTransport::with_provider(
        SerialConfig {
            port_name: "COM_PACED_115200".into(),
            baud_rate: 115_200,
            ..Default::default()
        },
        Arc::new(paced_serial.clone()),
    )
    .expect("SerialTransport");
    let codec = FireWaterCsvCodec::new(FireWaterConfig::with_columns(
        "uart_ctrl",
        vec![FireWaterColumnSpec::f32("meas")],
    ))
    .expect("FireWaterCsvCodec");

    let session = SourceSession::from_components(
        SourceSessionComponents {
            source_id: SourceId::new("uart_ctrl"),
            display_name: Some("115200 bps Closed-Loop UART".into()),
            transport: Box::new(transport),
            clock: StreamClockSynchronizer::new(ClockMode::Tier3HostArrival).unwrap(),
            codec: Box::new(codec),
            runtime_config: TransportRuntimeConfig::default(),
            sim_control: None,
            replay_control: None,
        },
        Arc::clone(ws.hub()),
    )
    .expect("uart_ctrl SourceSession");
    ws.register_source_session(session)
        .expect("register uart_ctrl");
    state.start_source("uart_ctrl".into()).expect("start uart_ctrl");

    wait_until(
        || {
            ws.list_sources().iter().any(|s| {
                s.source_id.as_str() == "uart_ctrl" && s.state == SourceRuntimeState::Running
            })
        },
        Duration::from_secs(2),
        "uart_ctrl Running",
    )
    .await;

    // Kick off a 33-byte banner (~2.87 ms wire time at 115200 bps) to hold the UART TX task busy while bursting.
    state
        .send_downlink(SendDownlinkRequestDto {
            source_id: "uart_ctrl".into(),
            payload: DownlinkPayloadDto::AsciiText {
                text: "INIT_CONTROLLER_READY_BANNER_001".into(),
                append_newline: true,
            },
            priority: TxPriorityDto::Normal,
            coalesce_key: None,
        })
        .expect("send warm banner");

    wait_until(
        || paced_serial.received_bytes() >= 1,
        Duration::from_secs(1),
        "UART TX busy flushing warm banner",
    )
    .await;

    // Rapid synchronous burst: 100 slider dragging kp updates (Normal + coalesce_key) + urgent High STOP.
    for step in 1..=100_u32 {
        let kp_val = 1.0 + (step as f64) * 0.01;
        state
            .send_downlink(SendDownlinkRequestDto {
                source_id: "uart_ctrl".into(),
                payload: DownlinkPayloadDto::KeyValue {
                    key: "kp".into(),
                    value: kp_val,
                },
                priority: TxPriorityDto::Normal,
                coalesce_key: Some("param:kp".into()),
            })
            .expect("send coalesced kp");
    }
    state
        .send_downlink(SendDownlinkRequestDto {
            source_id: "uart_ctrl".into(),
            payload: DownlinkPayloadDto::AsciiText {
                text: "STOP".into(),
                append_newline: true,
            },
            priority: TxPriorityDto::High,
            coalesce_key: None,
        })
        .expect("send urgent High STOP");
    let t_burst_done = Instant::now();

    wait_until(
        || {
            let lines = paced_serial.lines();
            lines.iter().any(|(_, l)| l == "STOP") && lines.iter().any(|(_, l)| l.starts_with("kp=2"))
        },
        Duration::from_secs(2),
        "final kp=2 and STOP drained across 115200 bps FIFO",
    )
    .await;

    let lines = paced_serial.lines();
    let stop_pos = lines.iter().position(|(_, l)| l == "STOP").unwrap();
    let final_kp_pos = lines
        .iter()
        .rposition(|(_, l)| l.starts_with("kp=2"))
        .unwrap();
    assert!(
        stop_pos < final_kp_pos,
        "High-priority STOP must preempt Normal-priority coalesced kp=2 (lines={lines:?})"
    );

    let convergence = lines[final_kp_pos]
        .0
        .saturating_duration_since(t_burst_done);
    assert!(
        convergence < Duration::from_millis(15),
        "expected final kp=2 convergence < 15 ms with TX coalescing under 115200 bps, got {convergence:?}"
    );

    let tel = ws
        .telemetry_snapshot()
        .sources
        .into_iter()
        .find(|s| s.source_id.as_str() == "uart_ctrl")
        .unwrap();
    assert!(
        tel.total_tx_coalesced >= 85,
        "expected >= 85 coalesced intermediate kp frames, got {}",
        tel.total_tx_coalesced
    );

    ws.shutdown().await.expect("workspace shutdown");
}
