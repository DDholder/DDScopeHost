//! Cross-milestone end-to-end integration tests for Track B (TB1-TB5) + Track A storage (TA2-TA4).
//!
//! Verifies the full acquisition pipeline:
//! - Built-in `TransportRegistry` & `ProtocolRegistry` factories
//! - `SimSignalTransport` + `RawByteRecorder` (`.ddraw`) -> `SimPresetCodec` -> `StreamClockSynchronizer` (Tier 1 & Tier 2) -> `NativeScalarSeries` (M4 glitch retention) + `EventSeries`
//! - `RawFileReplayTransport` replaying recorded `.ddraw` captures with identical sample/glitch recovery
//! - `JustFloatCodec` over `VirtualSerialPair` + Tier 2 PLL (including split frames & TX downlink)
//! - `FireWaterCsvCodec` over `VirtualUdpHub` + Tier 2 adaptive PLL -> `NativeScalarSeries` + `EventSeries`

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};

use ddscope_core::clock::{ClockMode, StreamClockSynchronizer};
use ddscope_core::protocol::{
    default_sim_state_labels, DownlinkPayload, FireWaterColumnSpec, FireWaterConfig,
    FireWaterCsvCodec, JustFloatCodec, JustFloatConfig, ProtocolCodec, ProtocolRegistry,
    JUSTFLOAT_TAIL_MAGIC,
};
use ddscope_core::storage::{EventSeries, M4QueryResult, NativeScalarSeries};
use ddscope_core::transport::{
    host_monotonic_ns, DdrawReader, OutboundFrame, RawByteRecorder, RawByteTap,
    RawFileReplayConfig, RawFileReplayTransport, RawRecordFormat, RawTapDirection, SerialConfig,
    SerialFlowControl, SerialParity, SerialStopBits, SerialTransport, SimConfig, SimSignalTransport,
    SimWireFormat, TransportKind, TransportRegistry, TransportRuntimeConfig, TransportScheduler,
    TransportState, TxPriority, UdpConfig, UdpTransport, VirtualSerialPair, VirtualUdpHub,
    CTRL_STATE_ALIGN, CTRL_STATE_CLOSED_LOOP, CTRL_STATE_IDLE, CTRL_STATE_OCP_FAULT,
};
use ddscope_core::types::{
    ChannelId, DecodedBatch, DecodedColumnBuf, LinearScale, ScalarSlice, ScalarType, ScalarValue,
};

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

struct TempFileGuard {
    path: PathBuf,
}

impl TempFileGuard {
    fn new(prefix: &str, ext: &str) -> Self {
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("ddscope_e2e_{prefix}_{pid}_{seq}.{ext}"));
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

async fn wait_for_state<F>(mut get_state: F, target: TransportState, timeout: Duration)
where
    F: FnMut() -> TransportState,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if get_state() == target {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for state {target:?}, current={:?}",
            get_state()
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[test]
fn t1_builtin_transport_and_protocol_registries_construct_all_phase1_components() {
    let transports = TransportRegistry::with_builtin_transports();
    for kind in [
        TransportKind::Sim,
        TransportKind::Serial,
        TransportKind::Udp,
        TransportKind::TcpClient,
        TransportKind::TcpServer,
        TransportKind::RawFileReplay,
    ] {
        assert!(transports.contains(kind), "missing transport factory for {kind:?}");
    }

    let temp_bin = TempFileGuard::new("reg_replay", "bin");
    std::fs::write(temp_bin.path(), b"ABCD").unwrap();
    let replay_json = serde_json::json!({
        "file_path": temp_bin.path(),
        "full_speed_offline": true
    })
    .to_string();

    assert_eq!(
        transports
            .create(TransportKind::Sim, r#"{"sample_rate_hz":10000,"analog_channels":4}"#)
            .unwrap()
            .capabilities()
            .kind,
        TransportKind::Sim
    );
    assert_eq!(
        transports
            .create(
                TransportKind::Serial,
                r#"{"port_name":"COM1","baud_rate":115200,"data_bits":8}"#
            )
            .unwrap()
            .capabilities()
            .kind,
        TransportKind::Serial
    );
    assert_eq!(
        transports
            .create(TransportKind::Udp, r#"{"bind_addr":"127.0.0.1:0"}"#)
            .unwrap()
            .capabilities()
            .kind,
        TransportKind::Udp
    );
    assert_eq!(
        transports
            .create(TransportKind::TcpClient, r#"{"remote_addr":"127.0.0.1:9000"}"#)
            .unwrap()
            .capabilities()
            .kind,
        TransportKind::TcpClient
    );
    assert_eq!(
        transports
            .create(TransportKind::TcpServer, r#"{"listen_addr":"127.0.0.1:0"}"#)
            .unwrap()
            .capabilities()
            .kind,
        TransportKind::TcpServer
    );
    assert_eq!(
        transports
            .create(TransportKind::RawFileReplay, &replay_json)
            .unwrap()
            .capabilities()
            .kind,
        TransportKind::RawFileReplay
    );

    let protocols = ProtocolRegistry::with_builtin_codecs();
    for codec_id in ["sim_preset", "just_float", "fixed_schema_binary", "fire_water"] {
        assert!(protocols.contains(codec_id), "missing protocol codec `{codec_id}`");
    }
    assert_eq!(
        protocols.create("sim_preset", "").unwrap().protocol_name(),
        "SimPreset"
    );
    assert_eq!(
        protocols
            .create("just_float", r#"{"channel_names":["a","b"]}"#)
            .unwrap()
            .protocol_name(),
        "JustFloat"
    );
    assert_eq!(
        protocols
            .create("fire_water", r#"{"channel_names":["a","b"]}"#)
            .unwrap()
            .protocol_name(),
        "FireWater"
    );
}

#[tokio::test]
async fn t2_sim_ddraw_sim_preset_tier1_tier2_m4_glitch_and_event_series() {
    const SAMPLE_RATE_HZ: f64 = 250_000.0;
    const TOTAL_SAMPLES: usize = 70_000; // Exceeds CHUNK_LEN (65,536) to seal chunk & build M4 pyramid

    let ddraw_file = TempFileGuard::new("sim_pipeline", "ddraw");
    let recorder = Arc::new(RawByteRecorder::open(ddraw_file.path()).expect("open .ddraw"));
    assert_eq!(recorder.format(), RawRecordFormat::Ddraw);

    let sim_cfg = SimConfig {
        sample_rate_hz: SAMPLE_RATE_HZ,
        analog_channels: 4,
        enable_glitch: true,
        glitch_period_samples: 10_000,
        wire_format: SimWireFormat::SimPreset,
        glitch_amplitude: 80.0,
        noise_amplitude: 0.0,
        paced: true,
        max_samples: Some(TOTAL_SAMPLES as u64),
        ..Default::default()
    };
    let sim_transport = SimSignalTransport::new(sim_cfg).unwrap();
    let control = sim_transport.control_handle();
    control.inject_manual_glitch(2);

    let mut scheduler =
        TransportScheduler::new(Box::new(sim_transport), TransportRuntimeConfig::default());
    scheduler
        .set_raw_tap(Some(Arc::clone(&recorder) as Arc<dyn RawByteTap>))
        .unwrap();
    let rx = scheduler.start().unwrap();

    // Also submit a TX command so both RX and TX paths are exercised and logged in .ddraw.
    scheduler
        .submit_tx(OutboundFrame::fire_and_forget(
            Bytes::from_static(b"PING\n"),
            TxPriority::Normal,
        ))
        .unwrap();

    let protocols = ProtocolRegistry::with_builtin_codecs();
    let mut codec = protocols
        .create("sim_preset", r#"{"source_id":"sim0","analog_channels":4}"#)
        .unwrap();

    let mut sync_tier1 = StreamClockSynchronizer::new(ClockMode::Tier1HardwareCounter {
        tick_hz: SAMPLE_RATE_HZ,
        counter_bits: 32,
    })
    .unwrap();
    let mut sync_tier2 = StreamClockSynchronizer::new(ClockMode::Tier2SoftwarePll {
        nominal_sample_rate_hz: Some(SAMPLE_RATE_HZ),
    })
    .unwrap();

    let mut batch = DecodedBatch::from_descriptors(codec.declared_channels(), 4_096);
    let mut ch3_series = NativeScalarSeries::new(
        ChannelId(3),
        ScalarType::F32,
        LinearScale::identity(),
    );
    let mut ctrl_series = EventSeries::new(ChannelId(7));

    let mut decoded_rows = 0usize;
    let mut glitch_indices = Vec::new();
    let mut pwm_a_true_count = 0usize;
    let mut pwm_b_true_count = 0usize;
    let mut fault_pin_true_count = 0usize;
    let mut seen_states = [false; 4];
    let mut last_ctrl_state: Option<i32> = None;
    let mut prev_t1_ts = 0u64;
    let mut prev_t2_ts = 0u64;

    let deadline = Instant::now() + Duration::from_secs(10);
    while decoded_rows < TOTAL_SAMPLES && Instant::now() < deadline {
        let mut drained = false;
        while let Ok(pkt) = rx.try_recv() {
            drained = true;
            recorder.on_raw_bytes(RawTapDirection::Rx, pkt.host_recv_ns, &pkt.bytes);

            batch.clear();
            let rows = codec.feed_bytes(&pkt.bytes, pkt.host_recv_ns, &mut batch).unwrap();
            if rows == 0 {
                continue;
            }

            // Tier 1 hw_ticks anchoring
            sync_tier1.populate_batch_timestamps(&mut batch).unwrap();
            let t1_timestamps = batch.timestamps_ns.clone();
            assert_eq!(t1_timestamps.len(), rows);
            for &ts in &t1_timestamps {
                assert!(ts >= prev_t1_ts, "Tier 1 timestamps regressed: {ts} < {prev_t1_ts}");
                prev_t1_ts = ts;
            }

            // Tier 2 Software PLL on the same packet arrival timestamps
            let mut t2_timestamps = vec![0u64; rows];
            sync_tier2
                .assign_timestamps(pkt.host_recv_ns, None, rows, &mut t2_timestamps)
                .unwrap();
            for &ts in &t2_timestamps {
                assert!(ts >= prev_t2_ts, "Tier 2 timestamps regressed: {ts} < {prev_t2_ts}");
                prev_t2_ts = ts;
            }

            // Append ch3 (24V DC bus + 1-sample +80V glitches) to NativeScalarSeries
            let ch3_vals = match &batch.columns[3] {
                DecodedColumnBuf::F32(v) => v.as_slice(),
                other => panic!("expected F32 column for ch3, got {other:?}"),
            };
            for (i, &val) in ch3_vals.iter().enumerate() {
                if val >= 100.0 {
                    glitch_indices.push(decoded_rows + i);
                }
            }
            ch3_series
                .append(&t1_timestamps, ScalarSlice::F32(ch3_vals))
                .unwrap();

            // Inspect digital PWM & fault flags
            let pwm_a = match &batch.columns[4] {
                DecodedColumnBuf::Bool(v) => v.as_slice(),
                other => panic!("expected Bool column for pwm_a, got {other:?}"),
            };
            let pwm_b = match &batch.columns[5] {
                DecodedColumnBuf::Bool(v) => v.as_slice(),
                other => panic!("expected Bool column for pwm_b, got {other:?}"),
            };
            let fault_pin = match &batch.columns[6] {
                DecodedColumnBuf::Bool(v) => v.as_slice(),
                other => panic!("expected Bool column for fault_pin, got {other:?}"),
            };
            pwm_a_true_count += pwm_a.iter().filter(|&&b| b).count();
            pwm_b_true_count += pwm_b.iter().filter(|&&b| b).count();
            fault_pin_true_count += fault_pin.iter().filter(|&&b| b).count();

            // Extract ctrl_state transitions into EventSeries
            let ctrl_col = batch.columns[7]
                .as_state_slice()
                .expect("ctrl_state column must be StateEnum");
            let mut ev_ts = Vec::new();
            let mut ev_codes = Vec::new();
            for (&ts, &code) in t1_timestamps.iter().zip(ctrl_col.iter()) {
                if (0..=3).contains(&code) {
                    seen_states[code as usize] = true;
                }
                if last_ctrl_state != Some(code) {
                    ev_ts.push(ts);
                    ev_codes.push(code);
                    last_ctrl_state = Some(code);
                }
            }
            if !ev_ts.is_empty() {
                ctrl_series.append(&ev_ts, &ev_codes).unwrap();
            }

            decoded_rows += rows;
        }
        if !drained {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }

    scheduler.stop().await.unwrap();
    let rec_stats = recorder.stop_and_flush().unwrap();

    assert_eq!(decoded_rows, TOTAL_SAMPLES);
    assert_eq!(ch3_series.total_samples(), TOTAL_SAMPLES as u64);
    assert_eq!(
        ch3_series.sealed_chunk_count(),
        1,
        "70,000 samples must seal 1 chunk of 65,536 samples"
    );
    assert_eq!(rec_stats.records_dropped, 0);
    assert!(rec_stats.records_written >= 2);

    // 7 periodic glitches (10k..=70k) + 2 manual glitches = 9 isolated 1-sample glitches
    assert_eq!(glitch_indices.len(), 9, "glitches={glitch_indices:?}");
    for win in glitch_indices.windows(2) {
        assert!(win[1] - win[0] >= 2, "glitch must stay 1-sample wide: {glitch_indices:?}");
    }
    assert!(pwm_a_true_count > 0 && pwm_b_true_count > 0);
    assert!(fault_pin_true_count >= glitch_indices.len());
    assert_eq!(seen_states, [true; 4], "all 4 ctrl_state values must occur");

    // Verify Tier 1 & Tier 2 diagnostics are healthy
    let t1_diag = sync_tier1.diagnostics();
    let t2_diag = sync_tier2.diagnostics();
    assert!(t1_diag.locked);
    assert!(
        (t1_diag.estimated_rate_hz - SAMPLE_RATE_HZ).abs() < SAMPLE_RATE_HZ * 0.02,
        "Tier 1 estimated rate out of bounds: {:?}",
        t1_diag
    );
    assert!(t2_diag.estimated_rate_hz > 0.0);

    // Verify M4 pyramid query retains the 1-sample +80V glitch (24V -> 104V)!
    let (t_min, t_max) = ch3_series.time_range_ns().unwrap();
    let m4_res = ch3_series.query_m4_viewport(t_min, t_max, 500).unwrap();
    match m4_res {
        M4QueryResult::Columns(cols) => {
            assert!(!cols.is_empty());
            let global_max = cols
                .iter()
                .map(|c| c.v_max)
                .fold(f32::NEG_INFINITY, f32::max);
            let global_min = cols
                .iter()
                .map(|c| c.v_min)
                .fold(f32::INFINITY, f32::min);
            assert!(
                global_max >= 103.5,
                "M4 decimation must preserve 1-sample glitch (~104V), got v_max={global_max}"
            );
            assert!(
                (global_min - 24.0).abs() < 0.1,
                "expected 24V baseline, got v_min={global_min}"
            );
        }
        M4QueryResult::Raw(_) => panic!("70,000 samples over 500px must return M4 Columns"),
    }

    // Verify EventSeries captured ctrl_state transitions and ZOH lookup matches
    assert!(ctrl_series.total_samples() >= 4);
    assert_eq!(ctrl_series.get_state_at(t_min), Some(CTRL_STATE_IDLE as i32));
    assert_eq!(
        ctrl_series.get_transition_by_index(1).map(|(_, s)| s),
        Some(CTRL_STATE_ALIGN as i32)
    );
    assert_eq!(
        ctrl_series.get_transition_by_index(2).map(|(_, s)| s),
        Some(CTRL_STATE_CLOSED_LOOP as i32)
    );
    assert_eq!(
        ctrl_series.get_transition_by_index(3).map(|(_, s)| s),
        Some(CTRL_STATE_OCP_FAULT as i32)
    );
}

#[tokio::test]
async fn t3_replay_ddraw_recovers_identical_samples_and_glitches() {
    const SAMPLE_RATE_HZ: f64 = 200_000.0;
    const TOTAL_SAMPLES: usize = 10_000;

    let ddraw_file = TempFileGuard::new("replay_roundtrip", "ddraw");
    let recorder = Arc::new(RawByteRecorder::open(ddraw_file.path()).unwrap());

    let sim_cfg = SimConfig {
        sample_rate_hz: SAMPLE_RATE_HZ,
        analog_channels: 4,
        enable_glitch: true,
        glitch_period_samples: 2_000,
        wire_format: SimWireFormat::SimPreset,
        glitch_amplitude: 80.0,
        noise_amplitude: 0.0,
        paced: true,
        max_samples: Some(TOTAL_SAMPLES as u64),
        ..Default::default()
    };
    let sim_transport = SimSignalTransport::new(sim_cfg).unwrap();
    let control = sim_transport.control_handle();
    control.inject_manual_glitch(2);

    let mut live_sched =
        TransportScheduler::new(Box::new(sim_transport), TransportRuntimeConfig::default());
    live_sched
        .set_raw_tap(Some(Arc::clone(&recorder) as Arc<dyn RawByteTap>))
        .unwrap();
    let live_rx = live_sched.start().unwrap();
    live_sched
        .submit_tx(OutboundFrame::fire_and_forget(
            Bytes::from_static(b"RATE=100000\n"),
            TxPriority::High,
        ))
        .unwrap();

    let mut live_codec = ProtocolRegistry::with_builtin_codecs()
        .create("sim_preset", r#"{"source_id":"live","analog_channels":4}"#)
        .unwrap();
    let mut live_sync = StreamClockSynchronizer::new(ClockMode::Tier1HardwareCounter {
        tick_hz: SAMPLE_RATE_HZ,
        counter_bits: 32,
    })
    .unwrap();
    let mut live_batch = DecodedBatch::from_descriptors(live_codec.declared_channels(), 1024);

    let mut live_ch0 = Vec::with_capacity(TOTAL_SAMPLES);
    let mut live_ch3 = Vec::with_capacity(TOTAL_SAMPLES);
    let mut live_ticks = Vec::with_capacity(TOTAL_SAMPLES);
    let mut live_states = Vec::with_capacity(TOTAL_SAMPLES);

    let deadline = Instant::now() + Duration::from_secs(5);
    while live_ch3.len() < TOTAL_SAMPLES && Instant::now() < deadline {
        let mut drained = false;
        while let Ok(pkt) = live_rx.try_recv() {
            drained = true;
            recorder.on_raw_bytes(RawTapDirection::Rx, pkt.host_recv_ns, &pkt.bytes);
            live_batch.clear();
            let rows = live_codec
                .feed_bytes(&pkt.bytes, pkt.host_recv_ns, &mut live_batch)
                .unwrap();
            if rows > 0 {
                live_sync.populate_batch_timestamps(&mut live_batch).unwrap();
                if let DecodedColumnBuf::F32(c0) = &live_batch.columns[0] {
                    live_ch0.extend_from_slice(c0);
                }
                if let DecodedColumnBuf::F32(c3) = &live_batch.columns[3] {
                    live_ch3.extend_from_slice(c3);
                }
                live_ticks.extend_from_slice(&live_batch.hw_ticks);
                if let Some(st) = live_batch.columns[7].as_state_slice() {
                    live_states.extend_from_slice(st);
                }
            }
        }
        if !drained {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }
    while live_sched.is_running()
        && live_sched.metrics_snapshot().tx_dispatched < 1
        && Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    live_sched.stop().await.unwrap();
    let rec_stats = recorder.stop_and_flush().unwrap();
    assert_eq!(live_ch3.len(), TOTAL_SAMPLES);
    assert_eq!(rec_stats.records_dropped, 0);

    // Verify DdrawReader sees both RX and TX records on disk.
    let reader = DdrawReader::open(ddraw_file.path()).unwrap();
    let mut ddraw_rx_records = 0usize;
    let mut ddraw_tx_records = 0usize;
    for rec_res in reader {
        let rec = rec_res.unwrap();
        match rec.direction {
            RawTapDirection::Rx => ddraw_rx_records += 1,
            RawTapDirection::Tx => ddraw_tx_records += 1,
        }
    }
    assert!(ddraw_rx_records > 0);
    assert!(ddraw_tx_records >= 1);

    // Replay the .ddraw file via RawFileReplayTransport and verify exact recovery.
    let replay_transport =
        RawFileReplayTransport::new(RawFileReplayConfig::offline(ddraw_file.path()))
            .unwrap();
    let mut replay_sched =
        TransportScheduler::new(Box::new(replay_transport), TransportRuntimeConfig::default());
    let replay_rx = replay_sched.start().unwrap();

    let mut replay_codec = ProtocolRegistry::with_builtin_codecs()
        .create("sim_preset", r#"{"source_id":"replay","analog_channels":4}"#)
        .unwrap();
    let mut replay_sync = StreamClockSynchronizer::new(ClockMode::Tier1HardwareCounter {
        tick_hz: SAMPLE_RATE_HZ,
        counter_bits: 32,
    })
    .unwrap();
    let mut replay_batch = DecodedBatch::from_descriptors(replay_codec.declared_channels(), 1024);

    let mut rep_ch0 = Vec::with_capacity(TOTAL_SAMPLES);
    let mut rep_ch3 = Vec::with_capacity(TOTAL_SAMPLES);
    let mut rep_ticks = Vec::with_capacity(TOTAL_SAMPLES);
    let mut rep_states = Vec::with_capacity(TOTAL_SAMPLES);
    let mut rep_series = NativeScalarSeries::new(
        ChannelId(13),
        ScalarType::F32,
        LinearScale::identity(),
    );

    let packets: Vec<_> = tokio::task::spawn_blocking(move || replay_rx.iter().collect())
        .await
        .unwrap();
    replay_sched.stop().await.unwrap();

    for pkt in packets {
        replay_batch.clear();
        let rows = replay_codec
            .feed_bytes(&pkt.bytes, pkt.host_recv_ns, &mut replay_batch)
            .unwrap();
        if rows > 0 {
            replay_sync
                .populate_batch_timestamps(&mut replay_batch)
                .unwrap();
            if let DecodedColumnBuf::F32(c0) = &replay_batch.columns[0] {
                rep_ch0.extend_from_slice(c0);
            }
            if let DecodedColumnBuf::F32(c3) = &replay_batch.columns[3] {
                rep_ch3.extend_from_slice(c3);
                rep_series
                    .append(&replay_batch.timestamps_ns, ScalarSlice::F32(c3))
                    .unwrap();
            }
            rep_ticks.extend_from_slice(&replay_batch.hw_ticks);
            if let Some(st) = replay_batch.columns[7].as_state_slice() {
                rep_states.extend_from_slice(st);
            }
        }
    }

    assert_eq!(rep_ch0, live_ch0);
    assert_eq!(rep_ch3, live_ch3);
    assert_eq!(rep_ticks, live_ticks);
    assert_eq!(rep_states, live_states);
    assert_eq!(rep_series.total_samples(), TOTAL_SAMPLES as u64);

    let live_glitches: Vec<usize> = live_ch3
        .iter()
        .enumerate()
        .filter_map(|(i, &v)| (v >= 100.0).then_some(i))
        .collect();
    let rep_glitches: Vec<usize> = rep_ch3
        .iter()
        .enumerate()
        .filter_map(|(i, &v)| (v >= 100.0).then_some(i))
        .collect();
    assert_eq!(live_glitches.len(), 7); // 5 periodic (2k..=10k) + 2 manual
    assert_eq!(rep_glitches, live_glitches);
}

#[tokio::test]
async fn t4_justfloat_and_firewater_over_virtual_serial_and_udp_with_tier2_pll() {
    // Part A: JustFloat over VirtualSerialPair + Tier 2 PLL (known nominal fs = 10 kHz)
    let serial_pair = VirtualSerialPair::new("COM_E2E_JF");
    let mut serial_peer = serial_pair.take_peer().expect("serial peer");
    let serial_cfg = SerialConfig {
        port_name: "COM_E2E_JF".to_string(),
        baud_rate: 12_000_000,
        data_bits: 8,
        parity: SerialParity::None,
        stop_bits: SerialStopBits::One,
        flow_control: SerialFlowControl::None,
        dtr_on_open: true,
        auto_reconnect: false,
        reconnect_delay_ms: 20,
    };
    let serial_transport = SerialTransport::with_provider(serial_cfg, serial_pair.provider()).unwrap();
    let serial_state = serial_transport.subscribe_state();
    let mut serial_sched =
        TransportScheduler::new(Box::new(serial_transport), TransportRuntimeConfig::default());
    let serial_rx = serial_sched.start().unwrap();
    wait_for_state(|| *serial_state.borrow(), TransportState::Running, Duration::from_secs(2))
        .await;

    const JF_SAMPLES: usize = 500;
    let mut jf_codec = JustFloatCodec::new(JustFloatConfig::new(
        "uart0",
        vec!["ia".into(), "ib".into(), "vbus".into()],
    ))
    .unwrap();
    let mut jf_sync = StreamClockSynchronizer::new(ClockMode::Tier2SoftwarePll {
        nominal_sample_rate_hz: Some(10_000.0),
    })
    .unwrap();
    let mut jf_batch = DecodedBatch::from_descriptors(jf_codec.declared_channels(), 128);
    let mut jf_series_ia =
        NativeScalarSeries::new(ChannelId(20), ScalarType::F32, LinearScale::identity());

    // Encode downlink command via JustFloatCodec -> SerialTransport -> VirtualSerialPeer
    let mut dl_buf = BytesMut::new();
    jf_codec
        .encode_downlink(
            &DownlinkPayload::KeyValue {
                key: "kp".into(),
                value: ScalarValue::F64(1.25),
            },
            &mut dl_buf,
        )
        .unwrap();
    serial_sched
        .submit_tx(OutboundFrame::fire_and_forget(
            dl_buf.freeze(),
            TxPriority::High,
        ))
        .unwrap();
    let mut tx_rx_buf = [0u8; 64];
    let n = tokio::time::timeout(Duration::from_secs(2), serial_peer.recv_tx(&mut tx_rx_buf))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&tx_rx_buf[..n], b"kp=1.25\n");

    // Stream 500 JustFloat frames in bursty chunks, including an intentional split-frame boundary.
    let mut wire_jf = Vec::with_capacity(JF_SAMPLES * 16);
    for i in 0..JF_SAMPLES {
        let ia = (i as f32) * 0.25;
        let ib = -(i as f32) * 0.5;
        let vbus = 24.0_f32;
        wire_jf.extend_from_slice(&ia.to_le_bytes());
        wire_jf.extend_from_slice(&ib.to_le_bytes());
        wire_jf.extend_from_slice(&vbus.to_le_bytes());
        wire_jf.extend_from_slice(&JUSTFLOAT_TAIL_MAGIC);
    }
    let sender_task = tokio::spawn(async move {
        // Split across an odd byte offset first to exercise codec carry reassembly.
        serial_peer.send_rx(&wire_jf[..21]).await.unwrap();
        tokio::time::sleep(Duration::from_millis(2)).await;
        serial_peer.send_rx(&wire_jf[21..400]).await.unwrap();
        for chunk in wire_jf[400..].chunks(400) {
            tokio::time::sleep(Duration::from_millis(2)).await;
            serial_peer.send_rx(chunk).await.unwrap();
        }
        serial_peer
    });

    let mut jf_ia_values = Vec::with_capacity(JF_SAMPLES);
    let mut prev_jf_ts = 0u64;
    let jf_deadline = Instant::now() + Duration::from_secs(5);
    while jf_ia_values.len() < JF_SAMPLES && Instant::now() < jf_deadline {
        let mut drained = false;
        while let Ok(pkt) = serial_rx.try_recv() {
            drained = true;
            jf_batch.clear();
            let rows = jf_codec
                .feed_bytes(&pkt.bytes, pkt.host_recv_ns, &mut jf_batch)
                .unwrap();
            if rows > 0 {
                jf_sync.populate_batch_timestamps(&mut jf_batch).unwrap();
                for &ts in &jf_batch.timestamps_ns {
                    assert!(ts >= prev_jf_ts);
                    prev_jf_ts = ts;
                }
                if let DecodedColumnBuf::F32(col) = &jf_batch.columns[0] {
                    jf_ia_values.extend_from_slice(col);
                    jf_series_ia
                        .append(&jf_batch.timestamps_ns, ScalarSlice::F32(col))
                        .unwrap();
                }
            }
        }
        if !drained {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }
    let _serial_peer = sender_task.await.unwrap();
    serial_sched.stop().await.unwrap();

    assert_eq!(jf_ia_values.len(), JF_SAMPLES);
    assert_eq!(jf_series_ia.total_samples(), JF_SAMPLES as u64);
    assert_eq!(jf_ia_values[0], 0.0);
    assert_eq!(jf_ia_values[499], 499.0 * 0.25);

    // Part B: FireWaterCsvCodec over VirtualUdpHub + Tier 2 adaptive PLL (nominal = None)
    let hub = VirtualUdpHub::new();
    let udp_bind = "127.0.0.1:46200";
    let udp_cfg = UdpConfig {
        bind_addr: udp_bind.to_string(),
        multicast_group: None,
        recv_buffer_bytes: 1024 * 1024,
        remote_tx_addr: None,
        broadcast: false,
        max_datagram_bytes: 4096,
    };
    let udp_transport = UdpTransport::with_backend(udp_cfg, hub.backend()).unwrap();
    let udp_state = udp_transport.subscribe_state();
    let mut udp_sched =
        TransportScheduler::new(Box::new(udp_transport), TransportRuntimeConfig::default());
    let udp_rx = udp_sched.start().unwrap();
    wait_for_state(|| *udp_state.borrow(), TransportState::Running, Duration::from_secs(2)).await;

    let udp_sender = hub
        .bind_peer("127.0.0.1:0".parse().unwrap(), false, None)
        .unwrap();
    let target_addr: SocketAddr = udp_bind.parse().unwrap();

    let fw_cfg = FireWaterConfig::with_columns(
        "udp0",
        vec![
            FireWaterColumnSpec::f32("va"),
            FireWaterColumnSpec::f32("vb"),
            FireWaterColumnSpec::bool("fault"),
            FireWaterColumnSpec::state_enum("ctrl", default_sim_state_labels()),
        ],
    );
    let mut fw_codec = FireWaterCsvCodec::new(fw_cfg).unwrap();
    let mut fw_sync = StreamClockSynchronizer::new(ClockMode::Tier2SoftwarePll {
        nominal_sample_rate_hz: None,
    })
    .unwrap();
    let mut fw_batch = DecodedBatch::from_descriptors(fw_codec.declared_channels(), 64);
    let mut fw_va_series =
        NativeScalarSeries::new(ChannelId(30), ScalarType::F32, LinearScale::identity());
    let mut fw_ctrl_series = EventSeries::new(ChannelId(31));

    const FW_SAMPLES: usize = 600;
    let udp_task = tokio::spawn(async move {
        let state_names = ["IDLE", "ALIGN", "CLOSED_LOOP", "OCP_FAULT"];
        for burst_start in (0..FW_SAMPLES).step_by(20) {
            let mut datagram = String::new();
            for i in burst_start..(burst_start + 20) {
                let va = (i as f32) * 0.5;
                let vb = -12.0_f32;
                let fault = usize::from(i % 100 == 0);
                let state = state_names[(i / 50) % 4];
                datagram.push_str(&format!("{va:.2},{vb:.2},{fault},{state}\n"));
            }
            udp_sender.send_to(datagram.as_bytes(), target_addr).await.unwrap();
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    });

    let mut fw_decoded = 0usize;
    let mut prev_fw_ts = 0u64;
    let mut last_fw_state: Option<i32> = None;
    let fw_deadline = Instant::now() + Duration::from_secs(5);
    while fw_decoded < FW_SAMPLES && Instant::now() < fw_deadline {
        let mut drained = false;
        while let Ok(pkt) = udp_rx.try_recv() {
            drained = true;
            fw_batch.clear();
            let rows = fw_codec
                .feed_bytes(&pkt.bytes, pkt.host_recv_ns, &mut fw_batch)
                .unwrap();
            if rows > 0 {
                fw_sync.populate_batch_timestamps(&mut fw_batch).unwrap();
                for &ts in &fw_batch.timestamps_ns {
                    assert!(ts >= prev_fw_ts);
                    prev_fw_ts = ts;
                }
                if let DecodedColumnBuf::F32(va) = &fw_batch.columns[0] {
                    fw_va_series
                        .append(&fw_batch.timestamps_ns, ScalarSlice::F32(va))
                        .unwrap();
                }
                if let Some(states) = fw_batch.columns[3].as_state_slice() {
                    let mut ts_ev = Vec::new();
                    let mut code_ev = Vec::new();
                    for (&ts, &st) in fw_batch.timestamps_ns.iter().zip(states.iter()) {
                        if last_fw_state != Some(st) {
                            ts_ev.push(ts);
                            code_ev.push(st);
                            last_fw_state = Some(st);
                        }
                    }
                    if !ts_ev.is_empty() {
                        fw_ctrl_series.append(&ts_ev, &code_ev).unwrap();
                    }
                }
                fw_decoded += rows;
            }
        }
        if !drained {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }
    udp_task.await.unwrap();
    udp_sched.stop().await.unwrap();

    assert_eq!(fw_decoded, FW_SAMPLES);
    assert_eq!(fw_va_series.total_samples(), FW_SAMPLES as u64);
    assert_eq!(fw_va_series.get_sample_by_index(0).map(|(_, v)| v), Some(0.0));
    assert_eq!(
        fw_va_series.get_sample_by_index(599).map(|(_, v)| v),
        Some(599.0 * 0.5)
    );
    assert!(fw_ctrl_series.total_samples() >= 12);
    let fw_diag = fw_sync.diagnostics();
    assert!(fw_diag.locked, "adaptive PLL should acquire lock over ~60ms stream");
    assert!(fw_diag.estimated_rate_hz > 0.0);
    assert!(host_monotonic_ns() > 0);
}
