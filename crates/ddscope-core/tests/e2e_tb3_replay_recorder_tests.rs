//! Opaque-box E2E integration tests for Milestone TB3 Part 2:
//! `RawFileReplayTransport` (`ReadOnlyMmap`, offline full-speed & paced/seekable modes)
//! and `RawByteRecorder` (`.ddraw` v1 & `.bin` non-blocking bypass tap + `DdrawReader`).

use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::task::JoinHandle;

use ddscope_core::transport::{
    create_replay_transport, host_monotonic_ns, new_shared_raw_tap, BufferPoolConfig, DdrawReader,
    OutboundFrame, OutboundReceiver, PacketSender, RawByteRecorder, RawByteTap,
    RawFileReplayConfig, RawFileReplayTransport, RawRecordFormat, RawRecorderConfig,
    RawTapDirection, ReadOnlyMmap, RxBackpressurePolicy, RxPacketEmitter, SharedRawTap,
    ShutdownReceiver, Transport, TransportCaps, TransportKind, TransportMetrics,
    TransportRegistry, TransportRuntimeConfig, TransportScheduler, TransportState, TxPriority,
    DDRAW_FILE_HEADER_BYTES, DDRAW_MAGIC, DDRAW_RECORD_HEADER_BYTES,
};
use ddscope_core::types::{CoreError, CoreResult};

static TEST_FILE_SEQ: AtomicU64 = AtomicU64::new(0);

struct TempFileGuard {
    path: PathBuf,
}

impl TempFileGuard {
    fn new(prefix: &str, ext: &str) -> Self {
        let seq = TEST_FILE_SEQ.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let file_name = format!("ddscope_tb3_{prefix}_{pid}_{seq}.{ext}");
        let path = std::env::temp_dir().join(file_name);
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

struct MockRxTxSource {
    packets: Vec<(bool, u64, Vec<u8>)>, // (is_tx, delay_ms, bytes)
    shared_tap: Arc<parking_lot::Mutex<Option<SharedRawTap>>>,
    shared_metrics: Arc<parking_lot::Mutex<Option<Arc<TransportMetrics>>>>,
}

impl Transport for MockRxTxSource {
    fn capabilities(&self) -> TransportCaps {
        TransportCaps {
            kind: TransportKind::Sim,
            supports_tx: true,
            supports_seek: false,
            supports_raw_record: true,
            supports_auto_reconnect: false,
        }
    }

    fn start(
        &mut self,
        rx_sink: PacketSender,
        mut tx_rx: OutboundReceiver,
        mut shutdown_rx: ShutdownReceiver,
    ) -> CoreResult<JoinHandle<CoreResult<()>>> {
        let packets = self.packets.clone();
        let tap = self
            .shared_tap
            .lock()
            .clone()
            .unwrap_or_else(new_shared_raw_tap);
        let metrics = self
            .shared_metrics
            .lock()
            .clone()
            .unwrap_or_else(|| Arc::new(TransportMetrics::new()));
        Ok(tokio::spawn(async move {
            let mut emitter = RxPacketEmitter::with_shared_hooks(
                rx_sink,
                BufferPoolConfig::default(),
                RxBackpressurePolicy::Block,
                metrics,
                tap,
            );
            for (is_tx, delay_ms, data) in packets {
                if *shutdown_rx.borrow() {
                    break;
                }
                if delay_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                }
                if !is_tx {
                    emitter.emit_slice(&data, host_monotonic_ns(), None)?;
                }
            }
            loop {
                tokio::select! {
                    biased;
                    res = shutdown_rx.changed() => {
                        if res.is_err() || *shutdown_rx.borrow() {
                            break;
                        }
                    }
                    maybe_tx = tx_rx.recv() => {
                        if maybe_tx.is_none() {
                            break;
                        }
                    }
                }
            }
            Ok(())
        }))
    }
}

#[tokio::test]
async fn t1_one_gib_bin_mmap_full_speed_replay_under_2s() {
    const ONE_GIB: u64 = 1_u64 << 30;
    const MID_OFFSET: u64 = 1_u64 << 29;
    let head_marker = b"DDSCOPE_HEAD_MARKER_1GIB_0001";
    let mid_marker = b"DDSCOPE_MIDDLE_MARKER_512MIB_0002";
    let tail_marker = b"DDSCOPE_TAIL_MARKER_END_0003";
    let tail_offset = ONE_GIB - tail_marker.len() as u64;

    let temp = TempFileGuard::new("1gib_replay", "bin");
    {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(temp.path())
            .expect("create 1 GiB temp file");
        file.set_len(ONE_GIB).expect("set_len 1 GiB");
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(head_marker).unwrap();
        file.seek(SeekFrom::Start(MID_OFFSET)).unwrap();
        file.write_all(mid_marker).unwrap();
        file.seek(SeekFrom::Start(tail_offset)).unwrap();
        file.write_all(tail_marker).unwrap();
        file.flush().unwrap();
    }

    let config = RawFileReplayConfig::offline(temp.path());
    let transport = RawFileReplayTransport::new(config).expect("valid offline replay config");
    let control = transport.control_handle();
    let metrics = Arc::clone(transport.metrics());

    let runtime_cfg = TransportRuntimeConfig {
        rx_ring_capacity: 64,
        ..TransportRuntimeConfig::default()
    };
    let mut scheduler = TransportScheduler::new(Box::new(transport), runtime_cfg);

    let t0 = Instant::now();
    let rx = scheduler.start().expect("replay scheduler should start");

    let consumer = tokio::task::spawn_blocking(move || {
        let mut total_bytes: u64 = 0;
        let mut packet_count: u64 = 0;
        let mut saw_head = false;
        let mut saw_mid = false;
        let mut saw_tail = false;

        while let Ok(pkt) = rx.recv() {
            let chunk_start = total_bytes;
            let chunk_end = chunk_start + pkt.bytes.len() as u64;

            if chunk_start == 0 {
                assert_eq!(&pkt.bytes[..head_marker.len()], head_marker);
                saw_head = true;
            }
            if chunk_start <= MID_OFFSET && chunk_end >= MID_OFFSET + mid_marker.len() as u64 {
                let rel = (MID_OFFSET - chunk_start) as usize;
                assert_eq!(&pkt.bytes[rel..rel + mid_marker.len()], mid_marker);
                saw_mid = true;
            }
            if chunk_start <= tail_offset && chunk_end >= tail_offset + tail_marker.len() as u64 {
                let rel = (tail_offset - chunk_start) as usize;
                assert_eq!(&pkt.bytes[rel..rel + tail_marker.len()], tail_marker);
                saw_tail = true;
            }

            total_bytes = chunk_end;
            packet_count += 1;
        }

        (total_bytes, packet_count, saw_head, saw_mid, saw_tail)
    });

    let (total_bytes, packet_count, saw_head, saw_mid, saw_tail) =
        consumer.await.expect("consumer task should join");
    let elapsed = t0.elapsed();

    scheduler.stop().await.expect("clean stop");

    assert_eq!(total_bytes, ONE_GIB);
    assert_eq!(packet_count, 1024);
    assert!(saw_head, "head marker missing");
    assert!(saw_mid, "middle marker missing");
    assert!(saw_tail, "tail marker missing");
    assert!(
        elapsed < Duration::from_secs(2),
        "1 GiB mmap replay took {:?}, expected < 2s",
        elapsed
    );

    let snap = metrics.snapshot();
    assert_eq!(snap.dropped_packets, 0);
    assert_eq!(snap.rx_bytes, ONE_GIB);
    assert_eq!(snap.rx_packets, 1024);
    assert_eq!(control.bytes_replayed(), ONE_GIB);
    assert_eq!(control.total_bytes(), ONE_GIB);
    assert!((control.progress_ratio() - 1.0).abs() < 1e-9);
}

#[tokio::test]
async fn t2_zero_byte_file_mmap_safe_offline_and_paced() {
    let temp = TempFileGuard::new("zero_len", "bin");
    File::create(temp.path()).expect("create empty file");

    let mmap = ReadOnlyMmap::open(temp.path()).expect("0-byte mmap must succeed");
    assert_eq!(mmap.len(), 0);
    assert!(mmap.is_empty());
    assert!(mmap.as_slice().is_empty());
    drop(mmap);

    for full_speed in [true, false] {
        let mut cfg = if full_speed {
            RawFileReplayConfig::offline(temp.path())
        } else {
            RawFileReplayConfig::paced(temp.path(), Some(10_000.0))
        };
        cfg.loop_playback = true; // Must not spin forever on empty file.
        let transport = RawFileReplayTransport::new(cfg).unwrap();
        let control = transport.control_handle();
        let mut scheduler =
            TransportScheduler::new(Box::new(transport), TransportRuntimeConfig::default());
        let rx = scheduler.start().unwrap();
        let pkts: Vec<_> = tokio::task::spawn_blocking(move || rx.iter().collect())
            .await
            .unwrap();
        assert!(pkts.is_empty());
        assert!((control.progress_ratio() - 1.0).abs() < 1e-9);
        scheduler.stop().await.unwrap();
    }
}

#[tokio::test]
async fn t3_raw_byte_recorder_ddraw_format_and_reader_roundtrip() {
    let temp = TempFileGuard::new("capture", "ddraw");
    let recorder = Arc::new(RawByteRecorder::open(temp.path()).expect("open ddraw recorder"));
    assert_eq!(recorder.format(), RawRecordFormat::Ddraw);

    let shared_tap = Arc::new(parking_lot::Mutex::new(None));
    let shared_metrics = Arc::new(parking_lot::Mutex::new(None));
    let mock = MockRxTxSource {
        packets: vec![
            (false, 0, b"RX_PKT_ONE".to_vec()),
            (false, 5, b"RX_PKT_TWO_LONGER".to_vec()),
        ],
        shared_tap: Arc::clone(&shared_tap),
        shared_metrics: Arc::clone(&shared_metrics),
    };
    let mut scheduler =
        TransportScheduler::new(Box::new(mock), TransportRuntimeConfig::default());
    *shared_tap.lock() = Some(Arc::clone(scheduler.raw_tap()));
    *shared_metrics.lock() = Some(Arc::clone(scheduler.metrics()));
    scheduler
        .set_raw_tap(Some(Arc::clone(&recorder) as Arc<dyn RawByteTap>))
        .expect("attach raw tap");

    let rx = scheduler.start().unwrap();

    // Send TX downlink so recorder captures both RX and TX with direction tags.
    scheduler
        .submit_tx(OutboundFrame::fire_and_forget(
            Bytes::from_static(b"TX_CMD_ACK"),
            TxPriority::High,
        ))
        .unwrap();

    let mut rx_seen = Vec::new();
    for _ in 0..2 {
        let pkt = tokio::task::spawn_blocking({
            let rx = rx.clone();
            move || rx.recv_timeout(Duration::from_secs(2))
        })
        .await
        .unwrap()
        .expect("RX packet should arrive");
        rx_seen.push(pkt.bytes.to_vec());
    }
    assert_eq!(
        rx_seen,
        vec![b"RX_PKT_ONE".to_vec(), b"RX_PKT_TWO_LONGER".to_vec()]
    );

    // Give TX pump a brief moment to dispatch, then flush and inspect on-disk file while running.
    tokio::time::sleep(Duration::from_millis(25)).await;
    recorder.flush().expect("flush should succeed");

    scheduler.stop().await.unwrap();
    let stats = recorder.stop_and_flush().expect("stop_and_flush");
    // Second stop_and_flush must be idempotent.
    let stats2 = recorder.stop_and_flush().expect("idempotent stop");
    assert_eq!(stats, stats2);

    assert_eq!(stats.records_dropped, 0);
    assert_eq!(stats.records_written, 3);
    let expected_payload_bytes =
        (b"RX_PKT_ONE".len() + b"RX_PKT_TWO_LONGER".len() + b"TX_CMD_ACK".len()) as u64;
    assert_eq!(stats.payload_bytes_written, expected_payload_bytes);
    assert_eq!(
        stats.file_bytes_written,
        DDRAW_FILE_HEADER_BYTES as u64
            + 3 * DDRAW_RECORD_HEADER_BYTES as u64
            + expected_payload_bytes
    );

    // Verify exact raw 32-byte header layout.
    let raw_bytes = std::fs::read(temp.path()).unwrap();
    assert_eq!(&raw_bytes[0..4], &DDRAW_MAGIC);
    assert_eq!(u16::from_le_bytes([raw_bytes[4], raw_bytes[5]]), 1);
    assert_eq!(u16::from_le_bytes([raw_bytes[6], raw_bytes[7]]), 0);
    assert_eq!(&raw_bytes[16..32], &[0u8; 16]);

    // Verify DdrawReader iterates all 3 records with valid CRC32 and timestamps.
    let reader = DdrawReader::open(temp.path()).expect("open DdrawReader");
    assert_eq!(reader.header().version, 1);
    let decoded: Vec<_> = reader.collect::<Result<Vec<_>, _>>().expect("valid ddraw");
    assert_eq!(decoded.len(), 3);

    let rx_records: Vec<_> = decoded
        .iter()
        .filter(|r| r.direction == RawTapDirection::Rx)
        .collect();
    let tx_records: Vec<_> = decoded
        .iter()
        .filter(|r| r.direction == RawTapDirection::Tx)
        .collect();
    assert_eq!(rx_records.len(), 2);
    assert_eq!(tx_records.len(), 1);
    assert_eq!(&rx_records[0].payload[..], b"RX_PKT_ONE");
    assert_eq!(&rx_records[1].payload[..], b"RX_PKT_TWO_LONGER");
    assert_eq!(&tx_records[0].payload[..], b"TX_CMD_ACK");
    assert!(rx_records[1].timestamp_ns >= rx_records[0].timestamp_ns);

    // Corrupt a payload byte and verify DdrawReader fails with CRC32 error.
    let bad_temp = TempFileGuard::new("corrupt", "ddraw");
    let mut corrupted = raw_bytes.clone();
    let last_idx = corrupted.len() - 1;
    corrupted[last_idx] ^= 0xFF;
    std::fs::write(bad_temp.path(), &corrupted).unwrap();
    let bad_reader = DdrawReader::open(bad_temp.path()).unwrap();
    let results: Vec<_> = bad_reader.collect();
    assert!(matches!(results.last(), Some(Err(CoreError::Storage(_)))));

    // Corrupt magic and verify DdrawReader::open fails immediately.
    let bad_magic = TempFileGuard::new("badmagic", "ddraw");
    let mut bad_hdr = raw_bytes;
    bad_hdr[0] = b'X';
    std::fs::write(bad_magic.path(), &bad_hdr).unwrap();
    assert!(matches!(
        DdrawReader::open(bad_magic.path()),
        Err(CoreError::Storage(_))
    ));
}

#[test]
fn t4_raw_rx_bin_and_nonblocking_backpressure_drops() {
    let temp_bin = TempFileGuard::new("rx_only", "bin");
    let rec = RawByteRecorder::open(temp_bin.path()).unwrap();
    assert_eq!(rec.format(), RawRecordFormat::RawRxBin);

    rec.on_raw_bytes(RawTapDirection::Rx, 100, &Bytes::from_static(b"AAA"));
    rec.on_raw_bytes(RawTapDirection::Tx, 200, &Bytes::from_static(b"IGNORED_TX"));
    rec.on_raw_bytes(RawTapDirection::Rx, 300, &Bytes::from_static(b"BBBB"));

    let stats = rec.stop_and_flush().unwrap();
    assert_eq!(stats.records_written, 2);
    assert_eq!(stats.payload_bytes_written, 7);
    assert_eq!(stats.file_bytes_written, 7);
    assert_eq!(std::fs::read(temp_bin.path()).unwrap(), b"AAABBBB");

    // Non-blocking hot path under bounded channel saturation.
    let temp_flood = TempFileGuard::new("flood", "ddraw");
    let cfg = RawRecorderConfig::new(temp_flood.path())
        .with_format(RawRecordFormat::Ddraw)
        .with_channel_capacity(2)
        .with_writer_buffer_bytes(4096);
    let flood_rec = RawByteRecorder::with_config(cfg).unwrap();
    let payload = Bytes::from(vec![0xA5u8; 4096]);
    const TOTAL_EVENTS: u64 = 10_000;
    let t0 = Instant::now();
    for i in 0..TOTAL_EVENTS {
        flood_rec.on_raw_bytes(RawTapDirection::Rx, i * 1_000, &payload);
    }
    let hot_elapsed = t0.elapsed();
    let flood_stats = flood_rec.stop_and_flush().unwrap();

    assert!(
        hot_elapsed < Duration::from_secs(1),
        "hot path blocked: took {:?}",
        hot_elapsed
    );
    assert_eq!(
        flood_stats.records_written + flood_stats.records_dropped,
        TOTAL_EVENTS
    );
    assert!(
        flood_stats.records_written > 0,
        "at least some records must be persisted"
    );
}

#[tokio::test]
async fn t5_ddraw_auto_detect_offline_and_paced_replay() {
    let temp = TempFileGuard::new("ddraw_replay", "ddraw");
    {
        let rec = RawByteRecorder::open(temp.path()).unwrap();
        let t0 = 1_000_000_000_u64;
        rec.on_raw_bytes(RawTapDirection::Rx, t0, &Bytes::from_static(b"FRAME_0"));
        rec.on_raw_bytes(
            RawTapDirection::Tx,
            t0 + 10_000_000,
            &Bytes::from_static(b"TX_SKIP"),
        );
        rec.on_raw_bytes(
            RawTapDirection::Rx,
            t0 + 40_000_000,
            &Bytes::from_static(b"FRAME_1"),
        );
        rec.on_raw_bytes(
            RawTapDirection::Rx,
            t0 + 80_000_000,
            &Bytes::from_static(b"FRAME_2"),
        );
        rec.stop_and_flush().unwrap();
    }

    // 1. Offline full-speed replay of .ddraw auto-detects DDRW and emits only RX payloads.
    let offline_transport =
        RawFileReplayTransport::new(RawFileReplayConfig::offline(temp.path())).unwrap();
    let mut offline_sched = TransportScheduler::new(
        Box::new(offline_transport),
        TransportRuntimeConfig::default(),
    );
    let rx = offline_sched.start().unwrap();
    let packets: Vec<_> = tokio::task::spawn_blocking(move || rx.iter().collect())
        .await
        .unwrap();
    offline_sched.stop().await.unwrap();

    assert_eq!(packets.len(), 3);
    assert_eq!(&packets[0].bytes[..], b"FRAME_0");
    assert_eq!(&packets[1].bytes[..], b"FRAME_1");
    assert_eq!(&packets[2].bytes[..], b"FRAME_2");
    assert_eq!(packets[1].host_recv_ns - packets[0].host_recv_ns, 40_000_000);
    assert_eq!(packets[2].host_recv_ns - packets[1].host_recv_ns, 40_000_000);

    // 2. Paced replay at 2.0x speed compresses the 80 ms span to ~40 ms.
    let mut paced_cfg = RawFileReplayConfig::paced(temp.path(), None);
    paced_cfg.playback_speed = 2.0;
    let paced_transport = RawFileReplayTransport::new(paced_cfg).unwrap();
    let mut paced_sched = TransportScheduler::new(
        Box::new(paced_transport),
        TransportRuntimeConfig::default(),
    );
    let t0 = Instant::now();
    let paced_rx = paced_sched.start().unwrap();
    let paced_packets: Vec<_> = tokio::task::spawn_blocking(move || paced_rx.iter().collect())
        .await
        .unwrap();
    let elapsed = t0.elapsed();
    paced_sched.stop().await.unwrap();

    assert_eq!(paced_packets.len(), 3);
    assert_eq!(&paced_packets[0].bytes[..], b"FRAME_0");
    assert_eq!(&paced_packets[1].bytes[..], b"FRAME_1");
    assert_eq!(&paced_packets[2].bytes[..], b"FRAME_2");
    assert!(
        elapsed >= Duration::from_millis(25) && elapsed < Duration::from_millis(250),
        "2x paced .ddraw replay took {:?} for 80ms span",
        elapsed
    );
}

#[tokio::test]
async fn t6_paced_replay_controls_seek_pause_speed_and_loop() {
    let temp = TempFileGuard::new("paced_bin", "bin");
    // 100 KB numbered blocks so we can inspect exact byte offsets after seeking.
    let mut raw = Vec::with_capacity(100_000);
    for i in 0_u32..25_000 {
        raw.extend_from_slice(&i.to_le_bytes());
    }
    std::fs::write(temp.path(), &raw).unwrap();

    // Validate config bounds when paced.
    let mut bad_speed = RawFileReplayConfig::paced(temp.path(), Some(10_000.0));
    bad_speed.playback_speed = 0.05;
    assert!(matches!(
        RawFileReplayTransport::new(bad_speed.clone()),
        Err(CoreError::InvalidConfig(_))
    ));
    bad_speed.playback_speed = 20.5;
    assert!(matches!(
        RawFileReplayTransport::new(bad_speed.clone()),
        Err(CoreError::InvalidConfig(_))
    ));
    bad_speed.playback_speed = f64::NAN;
    assert!(matches!(
        RawFileReplayTransport::new(bad_speed),
        Err(CoreError::InvalidConfig(_))
    ));

    let mut cfg = RawFileReplayConfig::paced(temp.path(), Some(500_000.0));
    cfg.playback_speed = 1.0;
    cfg.loop_playback = true;
    cfg.chunk_bytes = 4_096;

    let transport = RawFileReplayTransport::new(cfg).unwrap();
    let control = transport.control_handle();

    assert!(matches!(
        control.set_playback_speed(0.09),
        Err(CoreError::InvalidConfig(_))
    ));
    assert!(matches!(
        control.set_playback_speed(20.1),
        Err(CoreError::InvalidConfig(_))
    ));
    control.set_playback_speed(20.0).unwrap();
    assert!((control.playback_speed() - 20.0).abs() < 1e-9);
    control.set_playback_speed(0.1).unwrap();
    assert!((control.playback_speed() - 0.1).abs() < 1e-9);
    control.set_playback_speed(2.0).unwrap();

    // Pause before starting so we can test deterministic seek clamping and resume.
    control.set_paused(true);
    assert!(control.is_paused());

    let mut scheduler =
        TransportScheduler::new(Box::new(transport), TransportRuntimeConfig::default());
    let rx = scheduler.start().unwrap();

    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(rx.is_empty(), "paused replay must not emit packets");

    // Seek clamping checks.
    control.seek(1.8);
    assert!((control.progress_ratio() - 1.0).abs() < 1e-9);
    assert_eq!(control.seek_epoch(), 1);

    control.seek(-0.25);
    assert!((control.progress_ratio() - 0.0).abs() < 1e-9);
    assert_eq!(control.seek_epoch(), 2);

    // Seek directly to 50% (50_000 bytes -> u32 index 12_500).
    control.seek(0.5);
    assert_eq!(control.seek_epoch(), 3);
    assert!((control.progress_ratio() - 0.5).abs() < 1e-9);

    control.set_paused(false);
    let first_after_seek = tokio::task::spawn_blocking({
        let rx = rx.clone();
        move || rx.recv_timeout(Duration::from_secs(2))
    })
    .await
    .unwrap()
    .expect("packet must arrive after unpause");

    let first_word = u32::from_le_bytes([
        first_after_seek.bytes[0],
        first_after_seek.bytes[1],
        first_after_seek.bytes[2],
        first_after_seek.bytes[3],
    ]);
    assert_eq!(
        first_word, 12_500,
        "seek(0.5) should resume at exact byte offset 50_000 (word 12_500)"
    );

    // Let loop playback wrap at least once (> 100 KB total replayed).
    let mut total_after = first_after_seek.bytes.len() as u64;
    while total_after <= raw.len() as u64 {
        let pkt = tokio::task::spawn_blocking({
            let rx = rx.clone();
            move || rx.recv_timeout(Duration::from_secs(2))
        })
        .await
        .unwrap()
        .expect("looping replay must keep emitting");
        total_after += pkt.bytes.len() as u64;
    }

    // Disable loop_playback and drain until clean EOF completion.
    control.set_loop_playback(false);
    let remaining: Vec<_> = tokio::task::spawn_blocking(move || rx.iter().collect())
        .await
        .unwrap();
    assert!(!remaining.is_empty() || (control.progress_ratio() - 1.0).abs() < 1e-9);

    scheduler.stop().await.unwrap();
    assert_eq!(scheduler.state(), TransportState::Stopped);
}

#[tokio::test]
async fn t7_replay_factory_capabilities_and_dto_json_compat() {
    let temp = TempFileGuard::new("factory", "bin");
    std::fs::write(temp.path(), b"HELLO_FACTORY").unwrap();

    // JSON matching TransportConfigDto::RawFileReplay (including `kind` tag and no `chunk_bytes`).
    let dto_json = serde_json::json!({
        "kind": "raw_file_replay",
        "file_path": temp.path(),
        "full_speed_offline": true,
        "playback_speed": 1.0,
        "loop_playback": false,
        "nominal_byte_rate": null
    })
    .to_string();

    let mut registry = TransportRegistry::new();
    registry.register(TransportKind::RawFileReplay, create_replay_transport);

    let transport = registry
        .create(TransportKind::RawFileReplay, &dto_json)
        .expect("factory should parse DTO JSON");
    assert_eq!(
        transport.capabilities(),
        TransportCaps {
            kind: TransportKind::RawFileReplay,
            supports_tx: false,
            supports_seek: true,
            supports_raw_record: false,
            supports_auto_reconnect: false,
        }
    );

    let mut scheduler = TransportScheduler::new(transport, TransportRuntimeConfig::default());
    let rec_temp = TempFileGuard::new("unsupported_tap", "ddraw");
    let rec = Arc::new(RawByteRecorder::open(rec_temp.path()).unwrap());
    assert!(matches!(
        scheduler.set_raw_tap(Some(rec)),
        Err(CoreError::Unsupported(_))
    ));

    let rx = scheduler.start().unwrap();
    assert!(matches!(
        scheduler.submit_tx(OutboundFrame::fire_and_forget(
            Bytes::from_static(b"NO_TX"),
            TxPriority::Normal
        )),
        Err(CoreError::Unsupported(_))
    ));

    let packets: Vec<_> = tokio::task::spawn_blocking(move || rx.iter().collect())
        .await
        .unwrap();
    assert_eq!(packets.len(), 1);
    assert_eq!(&packets[0].bytes[..], b"HELLO_FACTORY");
    scheduler.stop().await.unwrap();
}

#[tokio::test]
async fn t8_ddraw_seek_error_handling_and_backpressure_shutdown() {
    // 1. Seek inside a .ddraw file in paced mode.
    let ddraw_temp = TempFileGuard::new("ddraw_seek", "ddraw");
    {
        let rec = RawByteRecorder::open(ddraw_temp.path()).unwrap();
        for i in 0_u32..10 {
            let payload = format!("REC_{i:02}");
            rec.on_raw_bytes(
                RawTapDirection::Rx,
                (i as u64 + 1) * 5_000_000,
                &Bytes::from(payload),
            );
        }
        rec.stop_and_flush().unwrap();
    }

    let mut cfg = RawFileReplayConfig::paced(ddraw_temp.path(), None);
    cfg.playback_speed = 10.0;
    let transport = RawFileReplayTransport::new(cfg).unwrap();
    let control = transport.control_handle();
    control.seek(0.5); // 50% of 10 uniform records -> REC_05

    let mut sched = TransportScheduler::new(Box::new(transport), TransportRuntimeConfig::default());
    let rx = sched.start().unwrap();
    let tail_pkts: Vec<_> = tokio::task::spawn_blocking(move || rx.iter().collect())
        .await
        .unwrap();
    sched.stop().await.unwrap();
    assert_eq!(tail_pkts.len(), 5);
    assert_eq!(&tail_pkts[0].bytes[..], b"REC_05");
    assert_eq!(&tail_pkts[4].bytes[..], b"REC_09");

    // 2. Corrupted .ddraw rejected at start().
    let bad_ddraw = TempFileGuard::new("bad_ddraw_start", "ddraw");
    let mut raw = std::fs::read(ddraw_temp.path()).unwrap();
    let last = raw.len() - 1;
    raw[last] ^= 0x55;
    std::fs::write(bad_ddraw.path(), &raw).unwrap();
    let bad_transport =
        RawFileReplayTransport::new(RawFileReplayConfig::offline(bad_ddraw.path())).unwrap();
    let mut bad_sched =
        TransportScheduler::new(Box::new(bad_transport), TransportRuntimeConfig::default());
    assert!(matches!(bad_sched.start(), Err(CoreError::Storage(_))));

    // 3. Non-existent file rejected at start() with CoreError::Io.
    let missing_path = std::env::temp_dir().join("ddscope_definitely_missing_file_999999.bin");
    let missing_transport =
        RawFileReplayTransport::new(RawFileReplayConfig::offline(&missing_path)).unwrap();
    let mut missing_sched = TransportScheduler::new(
        Box::new(missing_transport),
        TransportRuntimeConfig::default(),
    );
    assert!(matches!(missing_sched.start(), Err(CoreError::Io(_))));

    // 4. Saturated rx_ring with live consumer handle does not deadlock on scheduler.stop().
    let sat_bin = TempFileGuard::new("sat_stop", "bin");
    std::fs::write(sat_bin.path(), vec![0xABu8; 64 * 1024]).unwrap();
    let mut sat_cfg = RawFileReplayConfig::offline(sat_bin.path());
    sat_cfg.chunk_bytes = 1024;
    let sat_transport = RawFileReplayTransport::new(sat_cfg).unwrap();
    let mut sat_sched = TransportScheduler::new(
        Box::new(sat_transport),
        TransportRuntimeConfig {
            rx_ring_capacity: 2,
            ..TransportRuntimeConfig::default()
        },
    );
    let _held_rx_without_draining = sat_sched.start().unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    tokio::time::timeout(Duration::from_secs(2), sat_sched.stop())
        .await
        .expect("stop() must not deadlock when rx_ring is saturated")
        .unwrap();
}
