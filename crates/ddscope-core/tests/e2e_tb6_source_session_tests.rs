//! End-to-end integration tests for Phase 1 Track B Milestone `TB6`:
//! - `ChannelHub` concurrent lock-free multi-source registration & ingestion (`<source_id>/<local_path>`)
//! - `SourceSession` end-to-end IO -> Worker -> ProtocolCodec -> StreamClockSynchronizer -> Storage pipeline
//! - Single-sample M4 glitch preservation, digital transition edges, state ribbons, DDSF serialization
//! - Four concurrent heterogeneous sources (`Sim`, `Udp/JustFloat`, `Serial/FireWater`, `Tcp/FixedSchemaBinary`)
//! - Bypass `.ddraw` recording + `RawFileReplay` seek backwards / pause / resume / session restart monotonicity
//! - Full-duplex downlink encoding & `CoalesceKey` coalescing TX + High priority preemption
//! - Global `MemoryQuotaManager` FIFO rollover with active `PinnedGuard`
//! - `SourceConfig` serde round-trip & custom `TransportRegistry` / `ProtocolRegistry` wiring

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::{BufMut, BytesMut};
use parking_lot::Mutex;
use tokio::io::AsyncWriteExt;
use tokio::task::JoinHandle;

use ddscope_core::clock::{ClockMode, ClockSyncConfig, StreamClockSynchronizer};
use ddscope_core::ddsf::{
    DdsfFramePacker, DdsfZeroCopyReader, ScopeViewportPayload, TrackPayload,
};
use ddscope_core::protocol::{
    default_sim_state_labels, encode_downlink_default, ChecksumKind, ChecksumScope,
    DownlinkPayload, FireWaterColumnSpec, FireWaterConfig, FireWaterCsvCodec, FixedFieldSpec,
    FixedSchemaBinaryCodec, FixedSchemaBinaryConfig, JustFloatCodec, JustFloatConfig,
    ProtocolCodec, ProtocolRegistry, SimPresetConfig, TransactionResult, JUSTFLOAT_TAIL_MAGIC,
};
use ddscope_core::storage::{
    ChannelEntry, ChannelHub, ChannelStore, M4QueryResult, MemoryQuotaManager, WatermarkEvent,
    WatermarkListener,
};
use ddscope_core::transport::{
    DuplexTcpNetwork, OutboundReceiver, PacketSender, RawFileReplayConfig, SerialConfig,
    SerialTransport, ShutdownReceiver, SimConfig, TcpServerConfig, TcpServerTransport,
    TcpStreamConnector, Transport, TransportCaps, TransportKind, TransportRegistry,
    TransportRuntimeConfig, TxPriority, UdpConfig, UdpTransport, VirtualSerialPair, VirtualUdpHub,
};
use ddscope_core::types::{
    ChannelDescriptor, CoreError, CoreResult, DataChunk, DecodedBatch, DecodedColumnBuf,
    Endianness, EventDataChunk, ScalarSlice, ScalarType, ScalarValue, SourceId, StateLabel,
    TimestampNs, CHUNK_LEN,
};
use ddscope_core::workspace::{
    DownlinkRequest, SourceConfig, SourceProtocolConfig, SourceRuntimeState, SourceSession,
    SourceSessionComponents, SourceTransportConfig,
};

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

struct TempFileGuard {
    path: PathBuf,
}

impl TempFileGuard {
    fn new(prefix: &str, ext: &str) -> Self {
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("ddscope_tb6_{prefix}_{pid}_{seq}.{ext}"));
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

#[derive(Default)]
struct RecordingWatermarkListener {
    events: Mutex<Vec<WatermarkEvent>>,
}

impl WatermarkListener for RecordingWatermarkListener {
    fn on_watermark_advanced(&self, events: &[WatermarkEvent]) {
        self.events.lock().extend_from_slice(events);
    }
}

async fn wait_until<F>(mut cond: F, timeout: Duration, label: &str)
where
    F: FnMut() -> bool,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if cond() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for condition: {label}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn verify_channel_monotonic_timestamps(entry: &ChannelEntry) -> u64 {
    match entry.store() {
        ChannelStore::Scalar(lock) => {
            let series = lock.read();
            let mut prev: Option<TimestampNs> = None;
            let mut count = 0u64;
            for handle in series.sealed_chunks() {
                if let Some(arc) = handle.storage.as_in_memory() {
                    if let DataChunk::Scalar(sc) = arc.as_ref() {
                        for i in 0..sc.len() {
                            let ts = sc.timestamps.get_ns(i).expect("timestamp in sealed chunk");
                            if let Some(p) = prev {
                                assert!(
                                    ts >= p,
                                    "non-monotonic sealed timestamp on `{}`: {} < {}",
                                    entry.path(),
                                    ts,
                                    p
                                );
                            }
                            prev = Some(ts);
                            count += 1;
                        }
                    }
                }
            }
            for &ts in series.active_chunk().timestamps() {
                if let Some(p) = prev {
                    assert!(
                        ts >= p,
                        "non-monotonic active timestamp on `{}`: {} < {}",
                        entry.path(),
                        ts,
                        p
                    );
                }
                prev = Some(ts);
                count += 1;
            }
            assert_eq!(
                count + series.evicted_samples(),
                series.total_samples(),
                "sample accounting mismatch on `{}`",
                entry.path()
            );
            count
        }
        ChannelStore::Event(lock) => {
            let series = lock.read();
            let mut prev: Option<TimestampNs> = None;
            let mut count = 0u64;
            for handle in series.sealed_chunks() {
                if let Some(arc) = handle.storage.as_in_memory() {
                    if let DataChunk::Event(EventDataChunk::StateTransitions { timestamps_ns, .. }) =
                        arc.as_ref()
                    {
                        for &ts in timestamps_ns.as_slice() {
                            if let Some(p) = prev {
                                assert!(
                                    ts >= p,
                                    "non-monotonic event timestamp on `{}`: {} < {}",
                                    entry.path(),
                                    ts,
                                    p
                                );
                            }
                            prev = Some(ts);
                            count += 1;
                        }
                    }
                }
            }
            if let EventDataChunk::StateTransitions { timestamps_ns, .. } = series.active_chunk() {
                for &ts in timestamps_ns.as_slice() {
                    if let Some(p) = prev {
                        assert!(
                            ts >= p,
                            "non-monotonic active event timestamp on `{}`: {} < {}",
                            entry.path(),
                            ts,
                            p
                        );
                    }
                    prev = Some(ts);
                    count += 1;
                }
            }
            count
        }
        ChannelStore::Frame { .. } => 0,
    }
}

#[tokio::test]
async fn tb6_single_sim_source_m4_glitch_digital_edges_ribbons_and_snapshots() {
    let hub = Arc::new(ChannelHub::new());
    let wm_listener = Arc::new(RecordingWatermarkListener::default());
    hub.register_watermark_listener(Arc::clone(&wm_listener) as Arc<dyn WatermarkListener>);

    let sim_cfg = SimConfig {
        sample_rate_hz: 500_000.0,
        analog_channels: 4,
        enable_glitch: true,
        glitch_period_samples: 20_000,
        glitch_amplitude: 50.0,
        noise_amplitude: 0.0,
        paced: false,
        ..Default::default()
    };

    let cfg = SourceConfig::sim("sim0", sim_cfg).with_display_name("Sim Mixed-Signal");
    let mut session = SourceSession::new(cfg, Arc::clone(&hub)).expect("create sim SourceSession");
    assert_eq!(session.channels().len(), 8);
    assert_eq!(session.runtime_state(), SourceRuntimeState::Stopped);

    let summary = session.start().expect("start sim SourceSession");
    assert_eq!(summary.transport_kind, TransportKind::Sim);

    wait_until(
        || {
            hub.get_by_path_str("sim0/ch0")
                .is_some_and(|e| e.total_samples() >= 150_000)
        },
        Duration::from_secs(10),
        "sim0/ch0 >= 150_000 samples",
    )
    .await;

    let stop_summary = session.stop().await.expect("stop sim SourceSession");
    assert_eq!(stop_summary.state, SourceRuntimeState::Stopped);
    assert_eq!(session.telemetry_snapshot().decode_errors, 0);

    // Verify canonical <source_id>/<local_path> registration for all 8 channels
    for path in [
        "sim0/ch0",
        "sim0/ch1",
        "sim0/ch2",
        "sim0/ch3",
        "sim0/pwm_a",
        "sim0/pwm_b",
        "sim0/fault_pin",
        "sim0/ctrl_state",
    ] {
        let entry = hub
            .get_by_path_str(path)
            .unwrap_or_else(|| panic!("missing channel `{path}`"));
        assert!(entry.latest_value().is_some(), "`{path}` has no latest value");
        let _ = verify_channel_monotonic_timestamps(&entry);
    }

    let ch3 = hub.get_by_path_str("sim0/ch3").unwrap();
    assert!(ch3.sealed_chunk_count() >= 2, "expected >= 2 sealed chunks");
    let (t_min, t_max) = ch3.time_range_ns().unwrap();
    assert!(t_max > t_min);

    // M4 single-sample glitch preservation across multiple sealed chunks + active buffer
    let extract = ch3.query_m4_viewport_with_stats(t_min, t_max, 1_000).unwrap();
    assert!(
        extract.stats.max >= 70.0,
        "expected >= 70.0 glitch peak preserved in stats, got {}",
        extract.stats.max
    );
    let m4_cols = match extract.result {
        M4QueryResult::Columns(cols) => cols,
        M4QueryResult::Raw(_) => panic!("expected decimated M4 columns for >= 150k samples"),
    };
    let peak_col = m4_cols
        .iter()
        .map(|c| c.v_max)
        .fold(f32::NEG_INFINITY, f32::max);
    assert!(
        peak_col >= 70.0,
        "expected >= 70.0 glitch peak in M4 column envelope, got {peak_col}"
    );

    // Digital transition edges across sealed + active boundaries
    let pwm = hub.get_by_path_str("sim0/pwm_a").unwrap();
    let (init_lvl, edges) = pwm.extract_digital_edges(t_min, t_max).unwrap();
    assert!(init_lvl <= 1);
    assert!(!edges.is_empty(), "expected non-empty digital transitions");
    assert!(edges.iter().all(|&x| (0.0..=1.0).contains(&x)));
    for w in edges.windows(2) {
        assert!(w[1] >= w[0], "digital edge x_norm not sorted: {} > {}", w[0], w[1]);
    }

    // State machine transition ribbons
    let fsm = hub.get_by_path_str("sim0/ctrl_state").unwrap();
    let ribbons = fsm.extract_state_ribbons(t_min, t_max).unwrap();
    assert!(
        !ribbons.is_empty(),
        "expected state machine ribbons across sealed + active window"
    );
    for r in &ribbons {
        assert!(r.x1_norm > r.x0_norm);
        assert!(r.x0_norm >= 0.0 && r.x1_norm <= 1.0);
    }

    // Watermark listener received events for committed batches
    assert!(
        !wm_listener.events.lock().is_empty(),
        "expected watermark listener notifications"
    );

    // Pack DDSF v1 frame with M4, DigitalEdges, StateRibbons, and Hub LatestSnapshots
    let snapshots = hub.collect_latest_snapshots();
    assert_eq!(snapshots.len(), 8);

    let mut packer = DdsfFramePacker::default();
    let scope_payload = ScopeViewportPayload {
        scope_id: 1,
        trigger_state: 0,
        mode: 0,
        win_t_start_ns: t_min,
        win_t_span_ns: t_max - t_min,
        subpixel_offset_ns: 0.0,
        tracks: vec![
            TrackPayload::new_m4(ch3.id().0, m4_cols, Some(extract.stats)),
            TrackPayload::new_digital_edges(pwm.id().0, init_lvl, edges),
            TrackPayload::new_state_ribbons(fsm.id().0, ribbons),
        ],
    };
    let bytes = packer
        .pack_frame(1, t_max, 0, &[scope_payload], &snapshots)
        .expect("pack DDSF v1 frame");
    let reader = DdsfZeroCopyReader::new(bytes).expect("parse packed DDSF v1 frame");
    assert_eq!(reader.header().scope_count, 1);
    assert_eq!(reader.snapshots().unwrap().len(), 8);
    let parsed_scope = reader.scopes().next().unwrap();
    assert_eq!(parsed_scope.tracks().count(), 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tb6_four_heterogeneous_sources_concurrent_lockfree_ingestion_and_queries() {
    let hub = Arc::new(ChannelHub::new());
    let rt_handle = tokio::runtime::Handle::current();

    // Source 1: SimSignalTransport + SimPresetCodec + Tier 1
    let sim_cfg = SimConfig {
        sample_rate_hz: 200_000.0,
        analog_channels: 2,
        enable_glitch: false,
        paced: false,
        max_samples: Some(25_000),
        ..Default::default()
    };
    let mut sim_session = SourceSession::new(
        SourceConfig::sim("sim0", sim_cfg),
        Arc::clone(&hub),
    )
    .unwrap();

    // Source 2: VirtualUdpHub + JustFloatCodec + Tier 2 PLL (known 100 kHz)
    let udp_hub = VirtualUdpHub::new();
    let udp_bind: SocketAddr = "127.0.0.1:41001".parse().unwrap();
    let udp_peer_addr: SocketAddr = "127.0.0.1:41002".parse().unwrap();
    let udp_cfg = UdpConfig {
        bind_addr: udp_bind.to_string(),
        remote_tx_addr: Some(udp_peer_addr.to_string()),
        ..Default::default()
    };
    let udp_transport = UdpTransport::with_backend(udp_cfg, udp_hub.backend()).unwrap();
    let udp_peer = udp_hub.bind_peer(udp_peer_addr, false, None).unwrap();
    let jf_codec = JustFloatCodec::new(JustFloatConfig::new(
        "udp0",
        vec!["va".into(), "vb".into(), "vc".into()],
    ))
    .unwrap();
    let mut udp_session = SourceSession::from_components(
        SourceSessionComponents {
            source_id: SourceId::from("udp0"),
            display_name: Some("UDP JustFloat".into()),
            transport: Box::new(udp_transport),
            clock: StreamClockSynchronizer::new(ClockMode::Tier2SoftwarePll {
                nominal_sample_rate_hz: Some(100_000.0),
            })
            .unwrap(),
            codec: Box::new(jf_codec),
            runtime_config: TransportRuntimeConfig::default(),
            sim_control: None,
            replay_control: None,
        },
        Arc::clone(&hub),
    )
    .unwrap();

    // Source 3: VirtualSerialPair + FireWaterCsvCodec + Tier 2 Adaptive PLL (None)
    let serial_pair = VirtualSerialPair::new("VCOM6");
    let mut ser_peer = serial_pair.take_peer().expect("virtual serial peer");
    let ser_cfg = SerialConfig {
        port_name: "VCOM6".into(),
        baud_rate: 2_000_000,
        ..Default::default()
    };
    let ser_transport = SerialTransport::with_provider(ser_cfg, serial_pair.provider()).unwrap();
    let fw_codec = FireWaterCsvCodec::new(FireWaterConfig::with_columns(
        "ser0",
        vec![
            FireWaterColumnSpec::f32("temp"),
            FireWaterColumnSpec::state_enum("mode", default_sim_state_labels()),
        ],
    ))
    .unwrap();
    let mut ser_session = SourceSession::from_components(
        SourceSessionComponents {
            source_id: SourceId::from("ser0"),
            display_name: Some("Serial FireWater".into()),
            transport: Box::new(ser_transport),
            clock: StreamClockSynchronizer::new(ClockMode::Tier2SoftwarePll {
                nominal_sample_rate_hz: None,
            })
            .unwrap(),
            codec: Box::new(fw_codec),
            runtime_config: TransportRuntimeConfig::default(),
            sim_control: None,
            replay_control: None,
        },
        Arc::clone(&hub),
    )
    .unwrap();

    // Source 4: DuplexTcpNetwork (TcpServerTransport) + FixedSchemaBinaryCodec + Tier 1 HW counter
    let tcp_net = DuplexTcpNetwork::new();
    let tcp_addr: SocketAddr = "127.0.0.1:42001".parse().unwrap();
    let tcp_srv_transport = TcpServerTransport::with_acceptor(
        TcpServerConfig {
            listen_addr: tcp_addr.to_string(),
            tcp_nodelay: true,
        },
        tcp_net.acceptor(),
    )
    .unwrap();
    let mut bin_labels = BTreeMap::new();
    bin_labels.insert(
        0,
        StateLabel {
            name: "INIT".into(),
            color_rgba: None,
        },
    );
    bin_labels.insert(
        1,
        StateLabel {
            name: "RUN".into(),
            color_rgba: None,
        },
    );
    let bin_cfg = FixedSchemaBinaryConfig {
        source_id: "bin0".into(),
        header_magic: vec![0xA5, 0x5A],
        tail_magic: None,
        checksum: ChecksumKind::None,
        checksum_scope: ChecksumScope::PayloadOnly,
        checksum_endian: Endianness::Little,
        has_hw_tick_first_col: true,
        fields: vec![
            FixedFieldSpec::scalar("hw_cnt", ScalarType::U32),
            FixedFieldSpec::scalar("cur", ScalarType::I16),
            FixedFieldSpec::state_enum("fsm", ScalarType::U8, bin_labels),
        ],
        max_carry_bytes: 64 * 1024,
    };
    let bin_codec = FixedSchemaBinaryCodec::new(bin_cfg).unwrap();
    let mut tcp_session = SourceSession::from_components(
        SourceSessionComponents {
            source_id: SourceId::from("bin0"),
            display_name: Some("TCP Binary".into()),
            transport: Box::new(tcp_srv_transport),
            clock: StreamClockSynchronizer::new(ClockMode::Tier1HardwareCounter {
                tick_hz: 1_000_000.0,
                counter_bits: 32,
            })
            .unwrap(),
            codec: Box::new(bin_codec),
            runtime_config: TransportRuntimeConfig::default(),
            sim_control: None,
            replay_control: None,
        },
        Arc::clone(&hub),
    )
    .unwrap();

    // Verify all 13 channels across 4 sources are registered cleanly without collisions
    assert_eq!(hub.channel_count(), 13);

    sim_session.start_on(&rt_handle).unwrap();
    udp_session.start_on(&rt_handle).unwrap();
    ser_session.start_on(&rt_handle).unwrap();
    tcp_session.start_on(&rt_handle).unwrap();

    // Allow spawned UDP/TCP transport tasks a brief moment to bind sockets
    tokio::time::sleep(Duration::from_millis(15)).await;

    // Concurrent reader task continuously querying snapshots & viewports while 4 writers run
    let stop_reader = Arc::new(AtomicBool::new(false));
    let reader_hub = Arc::clone(&hub);
    let reader_flag = Arc::clone(&stop_reader);
    let reader_task = tokio::task::spawn_blocking(move || {
        let mut polls = 0usize;
        while !reader_flag.load(Ordering::Acquire) {
            let snaps = reader_hub.collect_latest_snapshots();
            assert!(snaps.len() <= 13);
            if let Some(ch) = reader_hub.get_by_path_str("sim0/ch0") {
                if let Some((t0, t1)) = ch.time_range_ns() {
                    if t1 > t0 {
                        let res = ch.query_m4_viewport(t0, t1, 256).unwrap();
                        match res {
                            M4QueryResult::Raw(pts) => assert!(!pts.is_empty()),
                            M4QueryResult::Columns(cols) => assert!(!cols.is_empty()),
                        }
                    }
                }
            }
            polls += 1;
            std::thread::sleep(Duration::from_millis(2));
        }
        polls
    });

    // Pump JustFloat frames over virtual UDP (400 samples per channel)
    let udp_sender = tokio::spawn(async move {
        let mut buf = BytesMut::with_capacity(4 * 16);
        for batch_idx in 0..40u32 {
            buf.clear();
            for s in 0..10u32 {
                let v = (batch_idx * 10 + s) as f32;
                buf.put_f32_le(v * 0.5);
                buf.put_f32_le(v * -0.5);
                buf.put_f32_le((v * 0.1).sin());
                buf.extend_from_slice(&JUSTFLOAT_TAIL_MAGIC);
            }
            udp_peer.send_to(buf.as_ref(), udp_bind).await.unwrap();
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    });

    // Pump FireWater CSV lines over virtual serial (300 lines)
    let ser_sender = tokio::spawn(async move {
        for idx in 0..300u32 {
            let line = format!("fw:{:.2},{}\n", 25.0 + (idx as f32) * 0.1, idx % 4);
            ser_peer.writer.write_all(line.as_bytes()).await.unwrap();
            if idx % 20 == 19 {
                ser_peer.writer.flush().await.unwrap();
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        }
        ser_peer.writer.flush().await.unwrap();
        ser_peer
    });

    // Pump FixedSchemaBinary frames over DuplexTcpNetwork client
    let tcp_addr_str = tcp_addr.to_string();
    let mut tcp_client = {
        let mut connected = None;
        for _ in 0..20 {
            match TcpStreamConnector::connect(&tcp_net, &tcp_addr_str, true).await {
                Ok(c) => {
                    connected = Some(c);
                    break;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(5)).await,
            }
        }
        connected.expect("connect to virtual TCP server")
    };
    let tcp_sender = tokio::spawn(async move {
        let mut frame = Vec::with_capacity(9);
        for idx in 0..500u32 {
            frame.clear();
            frame.extend_from_slice(&[0xA5, 0x5A]);
            frame.extend_from_slice(&(idx * 100).to_le_bytes()); // 10 kHz steps at 1 MHz tick
            frame.extend_from_slice(&((idx as i16) - 100).to_le_bytes());
            frame.push((idx % 2) as u8);
            tcp_client.writer.write_all(&frame).await.unwrap();
            if idx % 50 == 49 {
                tcp_client.writer.flush().await.unwrap();
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        }
        tcp_client.writer.flush().await.unwrap();
        tcp_client
    });

    udp_sender.await.unwrap();
    let _ser_peer = ser_sender.await.unwrap();
    let _tcp_client = tcp_sender.await.unwrap();

    wait_until(
        || {
            hub.get_by_path_str("sim0/ch0")
                .is_some_and(|e| e.total_samples() >= 5_000)
                && hub
                    .get_by_path_str("udp0/va")
                    .is_some_and(|e| e.total_samples() >= 400)
                && hub
                    .get_by_path_str("ser0/temp")
                    .is_some_and(|e| e.total_samples() >= 300)
                && hub
                    .get_by_path_str("bin0/cur")
                    .is_some_and(|e| e.total_samples() >= 500)
        },
        Duration::from_secs(10),
        "all 4 heterogeneous sources commit expected samples to ChannelHub",
    )
    .await;

    stop_reader.store(true, Ordering::Release);
    let poll_count = reader_task.await.unwrap();
    assert!(poll_count > 0);

    sim_session.stop().await.unwrap();
    udp_session.stop().await.unwrap();
    ser_session.stop().await.unwrap();
    tcp_session.stop().await.unwrap();

    assert_eq!(sim_session.telemetry_snapshot().decode_errors, 0);
    assert_eq!(udp_session.telemetry_snapshot().decode_errors, 0);
    assert_eq!(ser_session.telemetry_snapshot().decode_errors, 0);
    assert_eq!(tcp_session.telemetry_snapshot().decode_errors, 0);

    // Every channel across all 4 sources has monotonic timestamps and non-empty latest snapshots
    for entry in hub.all_channels() {
        assert!(entry.latest_value().is_some(), "missing latest on `{}`", entry.path());
        verify_channel_monotonic_timestamps(&entry);
    }
    assert_eq!(hub.collect_latest_snapshots().len(), 13);
}

#[tokio::test]
async fn tb6_bypass_ddraw_recording_and_replay_seek_restart_monotonicity() {
    let ddraw_guard = TempFileGuard::new("session_rec", "ddraw");
    let rec_hub = Arc::new(ChannelHub::new());

    // Step A: Record live SimSignalTransport session into .ddraw
    let mut rec_session = SourceSession::new(
        SourceConfig::sim(
            "rec_src",
            SimConfig {
                sample_rate_hz: 250_000.0,
                analog_channels: 2,
                paced: true,
                enable_glitch: true,
                glitch_period_samples: 10_000,
                glitch_amplitude: 33.0,
                ..Default::default()
            },
        ),
        Arc::clone(&rec_hub),
    )
    .unwrap();

    rec_session
        .set_raw_recorder(true, Some(ddraw_guard.path()))
        .expect("enable raw byte recorder");
    assert_eq!(rec_session.raw_recording_path(), Some(ddraw_guard.path()));

    rec_session.start().unwrap();
    wait_until(
        || rec_session.total_decoded_samples() >= 50_000,
        Duration::from_secs(10),
        "live recording >= 50_000 samples",
    )
    .await;

    rec_session.disable_raw_recorder().unwrap();
    assert!(rec_session.raw_recording_path().is_none());
    rec_session.stop().await.unwrap();

    let file_len = std::fs::metadata(ddraw_guard.path()).unwrap().len();
    assert!(file_len > 64, "recorded .ddraw file is too small ({file_len} bytes)");

    // Step B: Replay offline full-speed twice on the same SourceSession (verifying restart monotonicity)
    let replay_hub = Arc::new(ChannelHub::new());
    let rep_cfg = SourceConfig::new(
        "rep0",
        SourceTransportConfig::RawFileReplay(RawFileReplayConfig::offline(ddraw_guard.path())),
        ClockMode::Tier1HardwareCounter {
            tick_hz: 250_000.0,
            counter_bits: 64,
        },
        SourceProtocolConfig::SimPreset(SimPresetConfig {
            source_id: "rep0".into(),
            analog_channels: 2,
            ..Default::default()
        }),
    );

    let mut rep_session = SourceSession::new(rep_cfg, Arc::clone(&replay_hub)).unwrap();
    rep_session.start().unwrap();
    rep_session
        .wait_for_completion(Duration::from_secs(10))
        .await
        .unwrap();
    rep_session.stop().await.unwrap();
    let pass1_samples = rep_session.total_decoded_samples();
    assert!(pass1_samples >= 30_000, "expected >= 30k replayed samples, got {pass1_samples}");

    // Second run on same session rewinds .ddraw file timestamps back to initial recording epoch
    rep_session.start().unwrap();
    rep_session
        .wait_for_completion(Duration::from_secs(10))
        .await
        .unwrap();
    rep_session.stop().await.unwrap();
    let pass2_samples = rep_session.total_decoded_samples();
    assert_eq!(
        pass2_samples,
        pass1_samples * 2,
        "second replay pass should decode identical sample count"
    );
    assert_eq!(rep_session.telemetry_snapshot().decode_errors, 0);

    // Step C: Paced replay session with backward seek + pause + resume under Tier 2 PLL
    let paced_cfg = SourceConfig::new(
        "rep_paced",
        SourceTransportConfig::RawFileReplay(RawFileReplayConfig {
            file_path: ddraw_guard.path().to_path_buf(),
            full_speed_offline: false,
            playback_speed: 1.0,
            loop_playback: false,
            nominal_byte_rate: Some(2_000_000.0),
            chunk_bytes: 16 * 1024,
        }),
        ClockMode::Tier2SoftwarePll {
            nominal_sample_rate_hz: Some(250_000.0),
        },
        SourceProtocolConfig::SimPreset(SimPresetConfig {
            source_id: "rep_paced".into(),
            analog_channels: 2,
            ..Default::default()
        }),
    );
    let mut paced_session = SourceSession::new(paced_cfg, Arc::clone(&replay_hub)).unwrap();
    paced_session.start().unwrap();
    wait_until(
        || paced_session.total_decoded_samples() >= 5_000,
        Duration::from_secs(10),
        "paced replay streamed >= 5_000 samples before backward seek",
    )
    .await;

    // Seek backwards to 0.0 while pausing briefly
    paced_session
        .seek_replay(Some(0.0), Some(true), None)
        .expect("pause and seek backward to 0.0");
    tokio::time::sleep(Duration::from_millis(25)).await;
    let paused_count = paced_session.total_decoded_samples();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(
        paced_session.total_decoded_samples(),
        paused_count,
        "paused replay must not emit additional samples"
    );

    // Resume at 20x speed and drain to completion
    paced_session
        .seek_replay(None, Some(false), Some(20.0))
        .expect("resume paced replay at 20x");
    paced_session
        .wait_for_completion(Duration::from_secs(10))
        .await
        .unwrap();
    paced_session.stop().await.unwrap();
    assert!(paced_session.total_decoded_samples() > paused_count);
    assert_eq!(paced_session.telemetry_snapshot().decode_errors, 0);

    for entry in replay_hub.all_channels() {
        verify_channel_monotonic_timestamps(&entry);
    }
}

#[tokio::test]
async fn tb6_full_duplex_downlink_encoding_and_coalescing_tx() {
    let hub = Arc::new(ChannelHub::new());
    let udp_hub = VirtualUdpHub::new();
    let host_addr: SocketAddr = "127.0.0.1:43001".parse().unwrap();
    let peer_addr: SocketAddr = "127.0.0.1:43002".parse().unwrap();

    let transport = UdpTransport::with_backend(
        UdpConfig {
            bind_addr: host_addr.to_string(),
            remote_tx_addr: Some(peer_addr.to_string()),
            ..Default::default()
        },
        udp_hub.backend(),
    )
    .unwrap();
    let peer = udp_hub.bind_peer(peer_addr, false, None).unwrap();

    let session = SourceSession::from_components(
        SourceSessionComponents {
            source_id: SourceId::from("tx0"),
            display_name: Some("Full-Duplex TX".into()),
            transport: Box::new(transport),
            clock: StreamClockSynchronizer::new(ClockMode::Tier3HostArrival).unwrap(),
            codec: Box::new(
                FireWaterCsvCodec::new(FireWaterConfig::from_channel_names(
                    "tx0",
                    vec!["a".into()],
                ))
                .unwrap(),
            ),
            runtime_config: TransportRuntimeConfig::default(),
            sim_control: None,
            replay_control: None,
        },
        Arc::clone(&hub),
    )
    .unwrap();

    let estop_req = DownlinkRequest::from_ascii("ESTOP", true, TxPriority::High);

    // Submitting downlink before the transport starts must fail cleanly
    let not_running_err = session.send_downlink(&estop_req).unwrap_err();
    assert!(matches!(not_running_err, CoreError::Transport(_)));

    // Unsupported TX check on read-only transport
    struct NoTxTransport;
    impl Transport for NoTxTransport {
        fn capabilities(&self) -> TransportCaps {
            TransportCaps {
                kind: TransportKind::CanFd,
                supports_tx: false,
                supports_seek: false,
                supports_raw_record: false,
                supports_auto_reconnect: false,
            }
        }
        fn start(
            &mut self,
            _rx_sink: PacketSender,
            _tx_rx: OutboundReceiver,
            _shutdown_rx: ShutdownReceiver,
        ) -> CoreResult<JoinHandle<CoreResult<()>>> {
            Ok(tokio::spawn(async { Ok(()) }))
        }
    }
    let ro_session = SourceSession::from_components(
        SourceSessionComponents {
            source_id: SourceId::from("ro_src"),
            display_name: None,
            transport: Box::new(NoTxTransport),
            clock: StreamClockSynchronizer::new(ClockMode::Tier3HostArrival).unwrap(),
            codec: Box::new(
                FireWaterCsvCodec::new(FireWaterConfig::from_channel_names(
                    "ro_src",
                    vec!["a".into()],
                ))
                .unwrap(),
            ),
            runtime_config: TransportRuntimeConfig::default(),
            sim_control: None,
            replay_control: None,
        },
        Arc::clone(&hub),
    )
    .unwrap();
    let err = ro_session.send_downlink(&estop_req).unwrap_err();
    assert!(matches!(err, CoreError::Unsupported(_)));

    // Start session, then immediately enqueue burst before yielding to executor so coalescing
    // and High-priority preemption order are deterministic on this current_thread runtime.
    let mut session = session;
    session.start().unwrap();

    let mut coalesced_hits = 0usize;
    for step in 0..10 {
        let req = DownlinkRequest::from_key_value(
            "kp",
            ScalarValue::F32(1.0 + step as f32),
            TxPriority::Normal,
        )
        .with_coalesce_key("pid.kp");
        let r = session.send_downlink(&req).unwrap();
        assert!(r.accepted);
        assert_eq!(r.result, TransactionResult::SentFireAndForget);
        if r.coalesced_existing {
            coalesced_hits += 1;
        }
    }
    assert_eq!(
        coalesced_hits, 9,
        "10 updates on same coalesce_key must coalesce 9 times"
    );

    // Also enqueue a hex raw command and a High priority ASCII emergency stop
    let hex_req = DownlinkRequest::from_hex("AA 55 01 FF", TxPriority::Normal).unwrap();
    let hex_r = session.send_downlink(&hex_req).unwrap();
    assert_eq!(hex_r.encoded_bytes, 4);

    let estop_r = session.send_downlink(&estop_req).unwrap();
    assert_eq!(estop_r.effective_priority, TxPriority::High);

    let mut received_payloads = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while received_payloads.len() < 3 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for 3 UDP TX datagrams"
        );
        if let Some((data, src)) = tokio::time::timeout(Duration::from_millis(500), peer.recv())
            .await
            .unwrap()
        {
            assert_eq!(src, host_addr);
            received_payloads.push(data);
        }
    }

    session.stop().await.unwrap();

    // First transmitted datagram should be the High priority ESTOP command
    assert_eq!(received_payloads[0].as_ref(), b"ESTOP\n");
    // Coalesced pid.kp must carry ONLY the latest value ("kp=10\n" or "kp=10.0\n") and not stale 1..9
    let all_text: Vec<Vec<u8>> = received_payloads.iter().map(|b| b.to_vec()).collect();
    assert!(all_text.iter().any(|b| b.as_slice() == [0xAA, 0x55, 0x01, 0xFF]));
    assert!(
        all_text
            .iter()
            .any(|b| std::str::from_utf8(b).is_ok_and(|s| s.starts_with("kp=10"))),
        "expected latest coalesced value kp=10, got {:?}",
        all_text
    );

    let snap = session.telemetry_snapshot();
    assert_eq!(snap.total_tx_submitted, 12);
    assert_eq!(snap.total_tx_coalesced, 9);
    assert_eq!(snap.total_tx_dispatched, 3);
}

#[tokio::test]
async fn tb6_global_memory_quota_fifo_eviction_and_pinned_guard_rollover() {
    // Tight 2.8 MiB quota (~3-4 f32 chunks) shared across multiple sources
    let quota_mgr = Arc::new(MemoryQuotaManager::new(2_800_000));
    let hub = Arc::new(ChannelHub::with_quota_manager(Arc::clone(&quota_mgr)));

    // Pre-seed chunk #0 on `qa/sig` with a known sentinel value at sample 777 and pin it
    let qa_entry = hub
        .register_or_get(ChannelDescriptor::new_scalar("qa", "sig", ScalarType::F32))
        .unwrap();
    let mut ts = Vec::with_capacity(CHUNK_LEN);
    let mut vals = vec![1.0_f32; CHUNK_LEN];
    vals[777] = 9999.0;
    for i in 0..CHUNK_LEN {
        ts.push(((i + 1) as u64) * 1_000);
    }
    hub.append_scalar_batch(qa_entry.id(), &ts, ScalarSlice::F32(&vals))
        .unwrap();
    assert_eq!(qa_entry.sealed_chunk_count(), 1);

    let guard = hub
        .pin_channel_range(qa_entry.id(), 1_000, (CHUNK_LEN as u64) * 1_000)
        .unwrap();
    assert_eq!(guard.chunk_count(), 1);
    assert!(quota_mgr.is_pinned_active());

    let cfg_a = SourceConfig::sim(
        "qa",
        SimConfig {
            sample_rate_hz: 1_000_000.0,
            analog_channels: 1,
            paced: false,
            ..Default::default()
        },
    );
    let cfg_b = SourceConfig::sim(
        "qb",
        SimConfig {
            sample_rate_hz: 1_000_000.0,
            analog_channels: 1,
            paced: false,
            ..Default::default()
        },
    );

    let mut sess_a = SourceSession::new(cfg_a, Arc::clone(&hub)).unwrap();
    let mut sess_b = SourceSession::new(cfg_b, Arc::clone(&hub)).unwrap();

    // Advance qa/sig directly as well so qa/sig rolls past its pinned chunk 0
    for c in 1..=6u64 {
        let mut t_vec = Vec::with_capacity(CHUNK_LEN);
        let base = c * (CHUNK_LEN as u64) * 1_000 + 1_000;
        for i in 0..CHUNK_LEN {
            t_vec.push(base + (i as u64) * 1_000);
        }
        vals.fill(c as f32);
        hub.append_scalar_batch(qa_entry.id(), &t_vec, ScalarSlice::F32(&vals))
            .unwrap();
    }

    sess_a.start().unwrap();
    sess_b.start().unwrap();

    wait_until(
        || {
            hub.get_by_path_str("qa/ch0")
                .is_some_and(|e| e.total_samples() >= (CHUNK_LEN as u64) * 3)
                && hub
                    .get_by_path_str("qb/ch0")
                    .is_some_and(|e| e.total_samples() >= (CHUNK_LEN as u64) * 3)
        },
        Duration::from_secs(10),
        "qa and qb stream >= 3 sealed chunks each under tight global quota",
    )
    .await;

    sess_a.stop().await.unwrap();
    sess_b.stop().await.unwrap();

    // Global quota must have evicted old unpinned chunks across sources without exceeding ceiling wildly
    let stats = hub.quota_stats();
    assert!(
        stats.allocated_bytes <= stats.quota_bytes + stats.pinned_bytes + 1_500_000,
        "allocated bytes grew unboundedly: {stats:?}"
    );
    // Pinned chunk 0 sentinel must remain 100% intact inside PinnedGuard
    match guard.chunks()[0].as_ref() {
        DataChunk::Scalar(sc) => {
            assert_eq!(sc.values.get_scalar_value(777), Some(ScalarValue::F32(9999.0)));
        }
        _ => panic!("expected scalar chunk in PinnedGuard"),
    }

    drop(guard);
    assert_eq!(hub.quota_stats().pinned_bytes, 0);
}

struct DummyCustomTransport;

impl Transport for DummyCustomTransport {
    fn capabilities(&self) -> TransportCaps {
        TransportCaps {
            kind: TransportKind::CanFd,
            supports_tx: true,
            supports_seek: false,
            supports_raw_record: true,
            supports_auto_reconnect: false,
        }
    }

    fn start(
        &mut self,
        _rx_sink: PacketSender,
        _tx_rx: OutboundReceiver,
        mut shutdown_rx: ShutdownReceiver,
    ) -> CoreResult<JoinHandle<CoreResult<()>>> {
        Ok(tokio::spawn(async move {
            while !*shutdown_rx.borrow() {
                if shutdown_rx.changed().await.is_err() {
                    break;
                }
            }
            Ok(())
        }))
    }
}

struct DummyCustomCodec {
    channels: Vec<ChannelDescriptor>,
}

impl ProtocolCodec for DummyCustomCodec {
    fn protocol_name(&self) -> &'static str {
        "MockTb6"
    }

    fn declared_channels(&self) -> &[ChannelDescriptor] {
        &self.channels
    }

    fn feed_bytes(
        &mut self,
        bytes: &[u8],
        host_recv_ns: TimestampNs,
        batch_out: &mut DecodedBatch,
    ) -> CoreResult<usize> {
        batch_out.packet_recv_ns = host_recv_ns;
        if let DecodedColumnBuf::F32(v) = &mut batch_out.columns[0] {
            for chunk in bytes.chunks_exact(4) {
                v.push(f32::from_le_bytes(chunk.try_into().unwrap()));
                batch_out.sample_rows += 1;
            }
        }
        Ok(batch_out.sample_rows)
    }

    fn reset_state(&mut self) {}

    fn encode_downlink(&mut self, req: &DownlinkPayload, out: &mut BytesMut) -> CoreResult<()> {
        encode_downlink_default(req, out)
    }
}

#[tokio::test]
async fn tb6_config_serde_validation_and_custom_registry_wiring() {
    // Verify SourceConfig round-trip (especially Custom variant without serde tag collision)
    let custom_cfg = SourceConfig::new(
        "cust0",
        SourceTransportConfig::Custom {
            transport_kind: TransportKind::CanFd,
            json_config: "{}".into(),
        },
        ClockMode::Tier3HostArrival,
        SourceProtocolConfig::Custom {
            codec_id: "mock_codec".into(),
            json_config: "{}".into(),
        },
    )
    .with_auto_start(true)
    .with_clock_config(ClockSyncConfig::default());

    let serialized = serde_json::to_string(&custom_cfg).expect("serialize SourceConfig");
    let parsed: SourceConfig = serde_json::from_str(&serialized).expect("deserialize SourceConfig");
    assert_eq!(parsed, custom_cfg);

    // Custom TransportRegistry + ProtocolRegistry instantiation
    let mut t_reg = TransportRegistry::with_builtin_transports();
    t_reg.register(TransportKind::CanFd, |_json| {
        Ok(Box::new(DummyCustomTransport))
    });

    let mut p_reg = ProtocolRegistry::with_builtin_codecs();
    p_reg.register("mock_codec", |_json| {
        Ok(Box::new(DummyCustomCodec {
            channels: vec![ChannelDescriptor::new_scalar("cust0", "x", ScalarType::F32)],
        }))
    });

    let hub = Arc::new(ChannelHub::new());
    let mut session =
        SourceSession::with_registries(custom_cfg, Arc::clone(&hub), &t_reg, &p_reg).unwrap();
    assert_eq!(session.protocol_name(), "MockTb6");
    assert_eq!(session.channels().len(), 1);
    assert_eq!(session.channels()[0].path().as_str(), "cust0/x");

    session.start().unwrap();
    wait_until(
        || session.runtime_state() == SourceRuntimeState::Running,
        Duration::from_secs(2),
        "custom SourceSession running",
    )
    .await;
    session.stop().await.unwrap();
    assert_eq!(session.runtime_state(), SourceRuntimeState::Stopped);

    // Channel type conflict across sources on the same `<source_id>/<local_path>` is rejected
    let conflict = hub.register_or_get(ChannelDescriptor::new_scalar(
        "cust0",
        "x",
        ScalarType::Bool,
    ));
    assert!(matches!(conflict, Err(CoreError::InvalidConfig(_))));
}
