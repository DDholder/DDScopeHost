//! Opaque-box E2E integration & concurrency tests for Phase 1 Track B Milestone TB1
//! (`ddscope_core::transport`).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use parking_lot::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::task::JoinHandle;

use ddscope_core::transport::{
    host_monotonic_ns, BufferPoolConfig, OutboundFrame, OutboundReceiver, PacketBufferPool,
    PacketSender, RawByteTap, RawTapDirection, RxBackpressurePolicy, RxPacketEmitter,
    ShutdownReceiver, Transport, TransportCaps, TransportKind, TransportMetrics,
    TransportRegistry, TransportRuntimeConfig, TransportScheduler, TransportState,
    TxCoalescingQueue, TxPriority, TxStreamConsumer, DEFAULT_SLAB_BYTES,
};
use ddscope_core::types::{CoreError, CoreResult, TimestampNs};

#[test]
fn t1_high_frequency_single_key_coalescing_100k_writes() {
    let mut queue = TxCoalescingQueue::new();
    for i in 0_u32..100_000 {
        let payload = Bytes::from(format!("kp={i}"));
        let receipt = queue
            .push(OutboundFrame::coalesced(payload, "uart1:kp"))
            .expect("coalescing push must succeed");
        assert_eq!(queue.len(), 1);
        assert_eq!(receipt.queue_len, 1);
        assert_eq!(receipt.coalesced_existing, i > 0);
        assert_eq!(receipt.effective_priority, TxPriority::Normal);
    }

    let final_frame = queue.pop_next().expect("final coalesced frame must pop");
    assert_eq!(&final_frame.payload[..], b"kp=99999");
    assert_eq!(final_frame.coalesce_key.as_deref(), Some("uart1:kp"));
    assert!(queue.pop_next().is_none());

    let stats = queue.stats();
    assert_eq!(stats.total_pushed, 100_000);
    assert_eq!(stats.coalesced_overwrites, 99_999);
    assert_eq!(stats.total_popped, 1);
}

#[test]
fn t2_interleaved_multi_key_and_unkeyed_fifo_preservation() {
    let mut queue = TxCoalescingQueue::new();
    queue
        .push(OutboundFrame::fire_and_forget(
            Bytes::from_static(b"CMD_A"),
            TxPriority::Normal,
        ))
        .unwrap();
    queue
        .push(OutboundFrame::coalesced(Bytes::from_static(b"kp=1"), "kp"))
        .unwrap();
    queue
        .push(OutboundFrame::fire_and_forget(
            Bytes::from_static(b"CMD_B"),
            TxPriority::Normal,
        ))
        .unwrap();
    queue
        .push(OutboundFrame::coalesced(Bytes::from_static(b"ki=10"), "ki"))
        .unwrap();
    queue
        .push(OutboundFrame::coalesced(Bytes::from_static(b"kp=2"), "kp"))
        .unwrap();
    queue
        .push(OutboundFrame::coalesced(
            Bytes::from_static(b"kd=100"),
            "kd",
        ))
        .unwrap();
    queue
        .push(OutboundFrame::coalesced(Bytes::from_static(b"ki=20"), "ki"))
        .unwrap();
    queue
        .push(OutboundFrame::coalesced(Bytes::from_static(b"kp=3"), "kp"))
        .unwrap();

    assert_eq!(queue.len(), 5);
    assert_eq!(queue.coalesced_key_count(), 3);

    let drained: Vec<Vec<u8>> = std::iter::from_fn(|| queue.pop_next())
        .map(|f| f.payload.to_vec())
        .collect();
    assert_eq!(
        drained,
        vec![
            b"CMD_A".to_vec(),
            b"kp=3".to_vec(),
            b"CMD_B".to_vec(),
            b"ki=20".to_vec(),
            b"kd=100".to_vec(),
        ]
    );
}

#[test]
fn t3_dual_priority_preemption_cross_priority_coalescing_and_compaction() {
    let mut queue = TxCoalescingQueue::new();

    // High pops ahead of Normal.
    queue
        .push(OutboundFrame::fire_and_forget(
            Bytes::from_static(b"NORM"),
            TxPriority::Normal,
        ))
        .unwrap();
    queue
        .push(OutboundFrame::fire_and_forget(
            Bytes::from_static(b"HIGH"),
            TxPriority::High,
        ))
        .unwrap();
    assert_eq!(queue.pop_next().unwrap().payload, Bytes::from_static(b"HIGH"));
    assert_eq!(queue.pop_next().unwrap().payload, Bytes::from_static(b"NORM"));

    // Normal("kp=1") -> High("kp=0") promotes to High and removes stale Normal entry.
    queue
        .push(OutboundFrame::coalesced(Bytes::from_static(b"kp=1"), "kp"))
        .unwrap();
    queue
        .push(OutboundFrame::fire_and_forget(
            Bytes::from_static(b"OTHER_NORM"),
            TxPriority::Normal,
        ))
        .unwrap();
    let promo = queue
        .push(OutboundFrame::coalesced_with_priority(
            Bytes::from_static(b"kp=0"),
            "kp",
            TxPriority::High,
        ))
        .unwrap();
    assert!(promo.coalesced_existing);
    assert_eq!(promo.effective_priority, TxPriority::High);
    assert_eq!(queue.len(), 2);

    // High("kp=0") -> Normal("kp=2") updates payload while retaining High urgency.
    let keep_high = queue
        .push(
            OutboundFrame::coalesced(Bytes::from_static(b"kp=2"), "kp").with_transaction_id(77),
        )
        .unwrap();
    assert!(keep_high.coalesced_existing);
    assert_eq!(keep_high.effective_priority, TxPriority::High);
    assert_eq!(queue.len(), 2);

    let first = queue.pop_next().unwrap();
    assert_eq!(first.payload, Bytes::from_static(b"kp=2"));
    assert_eq!(first.priority, TxPriority::High);
    assert_eq!(first.transaction_id, Some(77));

    let second = queue.pop_next().unwrap();
    assert_eq!(second.payload, Bytes::from_static(b"OTHER_NORM"));
    assert!(queue.pop_next().is_none());

    // Repeated Normal -> High promotion cycles keep queue state clean and bounded.
    for cycle in 0_u32..5_000 {
        let key = format!("k_{}", cycle % 4);
        queue
            .push(OutboundFrame::coalesced(
                Bytes::from(format!("n={cycle}")),
                key.clone(),
            ))
            .unwrap();
        queue
            .push(OutboundFrame::coalesced_with_priority(
                Bytes::from(format!("h={cycle}")),
                key,
                TxPriority::High,
            ))
            .unwrap();
        let popped = queue.pop_next().expect("promoted frame must pop");
        assert_eq!(popped.priority, TxPriority::High);
        assert_eq!(popped.payload, Bytes::from(format!("h={cycle}")));
        assert!(queue.is_empty());
    }
}

#[test]
fn t4_full_capacity_rejection_vs_in_place_overwrite() {
    let mut queue = TxCoalescingQueue::with_capacity(4);
    queue
        .push(OutboundFrame::fire_and_forget(
            Bytes::from_static(b"U1"),
            TxPriority::Normal,
        ))
        .unwrap();
    queue
        .push(OutboundFrame::fire_and_forget(
            Bytes::from_static(b"U2"),
            TxPriority::High,
        ))
        .unwrap();
    queue
        .push(OutboundFrame::coalesced(Bytes::from_static(b"kp=1"), "kp"))
        .unwrap();
    queue
        .push(OutboundFrame::coalesced(Bytes::from_static(b"ki=1"), "ki"))
        .unwrap();
    assert_eq!(queue.len(), 4);

    let err_unkeyed = queue
        .push(OutboundFrame::fire_and_forget(
            Bytes::from_static(b"U3"),
            TxPriority::Normal,
        ))
        .unwrap_err();
    assert!(matches!(err_unkeyed, CoreError::Transport(_)));

    let err_new_key = queue
        .push(OutboundFrame::coalesced(Bytes::from_static(b"kd=1"), "kd"))
        .unwrap_err();
    assert!(matches!(err_new_key, CoreError::Transport(_)));
    assert_eq!(queue.stats().rejected_full, 2);

    // Overwriting existing keys at full capacity succeeds cleanly.
    let r_kp = queue
        .push(OutboundFrame::coalesced(Bytes::from_static(b"kp=42"), "kp"))
        .unwrap();
    assert!(r_kp.coalesced_existing);
    assert_eq!(r_kp.queue_len, 4);

    let r_ki = queue
        .push(OutboundFrame::coalesced(Bytes::from_static(b"ki=84"), "ki"))
        .unwrap();
    assert!(r_ki.coalesced_existing);
    assert_eq!(r_ki.queue_len, 4);

    assert_eq!(queue.pop_next().unwrap().payload, Bytes::from_static(b"U2"));
    assert_eq!(queue.pop_next().unwrap().payload, Bytes::from_static(b"U1"));
    assert_eq!(
        queue.pop_next().unwrap().payload,
        Bytes::from_static(b"kp=42")
    );
    assert_eq!(
        queue.pop_next().unwrap().payload,
        Bytes::from_static(b"ki=84")
    );
}

#[tokio::test]
async fn t5_buffer_pool_cross_thread_reclamation_and_async_read_buf() {
    let (tx, rx) = crossbeam_channel::bounded::<ddscope_core::transport::RawPacket>(1_024);
    let (ack_tx, ack_rx) = crossbeam_channel::bounded::<()>(16);

    let consumer = std::thread::spawn(move || {
        let mut prev_ts = 0_u64;
        let mut received = 0_u32;
        while let Ok(pkt) = rx.recv() {
            assert_eq!(pkt.bytes.len(), 1_024);
            let expected_byte = (received & 0xFF) as u8;
            assert!(pkt.bytes.iter().all(|&b| b == expected_byte));
            assert!(pkt.host_recv_ns >= prev_ts);
            prev_ts = pkt.host_recv_ns;
            drop(pkt);
            received += 1;
            if received % 64 == 0 {
                let _ = ack_tx.send(());
            }
        }
        received
    });

    let mut pool = PacketBufferPool::with_slab_size(DEFAULT_SLAB_BYTES, 4);
    for seq in 0_u32..2_000 {
        let payload = vec![(seq & 0xFF) as u8; 1_024];
        let pkt = pool.pack_slice(&payload, host_monotonic_ns(), None);
        tx.send(pkt).expect("ring send must succeed");
        if (seq + 1) % 64 == 0 {
            ack_rx
                .recv()
                .expect("consumer should drain slab promptly");
        }
    }
    drop(tx);

    let total_received = consumer.join().expect("consumer thread should succeed");
    assert_eq!(total_received, 2_000);

    let stats = pool.stats();
    assert_eq!(stats.total_packets_frozen, 2_000);
    assert_eq!(stats.total_bytes_frozen, 2_000 * 1_024);
    assert!(
        stats.slabs_reclaimed >= 28,
        "expected >= 28 reclaimed slabs, got {}",
        stats.slabs_reclaimed
    );
    assert_eq!(stats.fallback_allocations, 0);

    // Exercise acquire_limited_buf with tokio::io::AsyncReadExt::read_buf over duplex stream.
    let (mut writer, mut reader) = tokio::io::duplex(256);
    writer.write_all(b"DDSCOPE_DUPLEX_FRAME").await.unwrap();
    drop(writer);

    let mut limited_buf = pool.acquire_limited_buf(64);
    let n = reader.read_buf(&mut limited_buf).await.unwrap();
    assert_eq!(n, b"DDSCOPE_DUPLEX_FRAME".len());
    let frozen = pool
        .freeze_packet(host_monotonic_ns(), Some(Arc::from("duplex")))
        .expect("read_buf data should freeze");
    assert_eq!(&frozen.bytes[..], b"DDSCOPE_DUPLEX_FRAME");
}

struct RecordingTap {
    events: Mutex<Vec<(RawTapDirection, TimestampNs, Vec<u8>)>>,
}

impl RecordingTap {
    fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
        }
    }

    fn snapshot(&self) -> Vec<(RawTapDirection, TimestampNs, Vec<u8>)> {
        self.events.lock().clone()
    }
}

impl RawByteTap for RecordingTap {
    fn on_raw_bytes(&self, direction: RawTapDirection, timestamp_ns: TimestampNs, bytes: &Bytes) {
        self.events
            .lock()
            .push((direction, timestamp_ns, bytes.to_vec()));
    }
}

struct PacedFullDuplexTransport {
    tx_pace: Duration,
    backpressure: RxBackpressurePolicy,
    shared_metrics: Option<Arc<TransportMetrics>>,
    shared_tap: Arc<Mutex<Option<ddscope_core::transport::SharedRawTap>>>,
    delivered_tx: Arc<Mutex<Vec<Vec<u8>>>>,
    burst_on_start: usize,
    burst_done_tx: Option<crossbeam_channel::Sender<()>>,
}

impl PacedFullDuplexTransport {
    fn new(tx_pace: Duration, backpressure: RxBackpressurePolicy) -> Self {
        Self {
            tx_pace,
            backpressure,
            shared_metrics: None,
            shared_tap: Arc::new(Mutex::new(None)),
            delivered_tx: Arc::new(Mutex::new(Vec::new())),
            burst_on_start: 0,
            burst_done_tx: None,
        }
    }
}

impl Transport for PacedFullDuplexTransport {
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
        tx_rx: OutboundReceiver,
        mut shutdown_rx: ShutdownReceiver,
    ) -> CoreResult<JoinHandle<CoreResult<()>>> {
        let tx_pace = self.tx_pace;
        let backpressure = self.backpressure;
        let shared_metrics = self.shared_metrics.clone();
        let shared_tap = self.shared_tap.lock().clone();
        let delivered_tx = Arc::clone(&self.delivered_tx);
        let burst_on_start = self.burst_on_start;
        let burst_done_tx = self.burst_done_tx.take();

        Ok(tokio::spawn(async move {
            let mut emitter = RxPacketEmitter::with_shared_hooks(
                rx_sink,
                BufferPoolConfig::default(),
                backpressure,
                shared_metrics.unwrap_or_else(|| Arc::new(TransportMetrics::new())),
                shared_tap.unwrap_or_else(ddscope_core::transport::new_shared_raw_tap),
            );

            if burst_on_start > 0 {
                for idx in 0..burst_on_start {
                    let msg = format!("BURST_{idx}");
                    emitter.emit_slice(msg.as_bytes(), host_monotonic_ns(), None)?;
                }
                if let Some(done) = burst_done_tx {
                    let _ = done.send(());
                }
            }

            let mut consumer = TxStreamConsumer::new(tx_rx);
            loop {
                tokio::select! {
                    biased;
                    res = shutdown_rx.changed() => {
                        if res.is_err() || *shutdown_rx.borrow() {
                            break;
                        }
                    }
                    maybe_frame = consumer.recv_next() => {
                        match maybe_frame {
                            Some(frame) => {
                                delivered_tx.lock().push(frame.payload.to_vec());
                                let _ = emitter.emit_slice(&frame.payload, host_monotonic_ns(), None)?;
                                if !tx_pace.is_zero() {
                                    tokio::time::sleep(tx_pace).await;
                                }
                            }
                            None => break,
                        }
                    }
                }
            }
            Ok(())
        }))
    }
}

#[test]
fn t6_scheduler_slow_tx_coalescing_raw_tap_backpressure_and_restart() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();

    // Part A: Slow TX coalescing + Priority + RawByteTap + Restart started from sync thread via start_on.
    let transport =
        PacedFullDuplexTransport::new(Duration::from_millis(5), RxBackpressurePolicy::DropNewest);
    let delivered_tx = Arc::clone(&transport.delivered_tx);
    let tap_hook = Arc::clone(&transport.shared_tap);

    let cfg = TransportRuntimeConfig {
        rx_ring_capacity: 64,
        tx_queue_capacity: 1_024,
        tx_handoff_capacity: 1,
        buffer_pool: BufferPoolConfig::default(),
        rx_backpressure: RxBackpressurePolicy::DropNewest,
    };
    let mut scheduler = TransportScheduler::new(Box::new(transport), cfg.clone());
    *tap_hook.lock() = Some(scheduler.raw_tap().clone());
    assert_eq!(scheduler.state(), TransportState::Stopped);

    let tap = Arc::new(RecordingTap::new());
    scheduler.set_raw_tap(Some(tap.clone())).unwrap();

    let rx = scheduler.start_on(rt.handle()).unwrap();
    assert_eq!(scheduler.state(), TransportState::Running);

    // Flood 5,000 slider updates + 1 High emergency command from synchronous caller thread.
    for i in 0_u32..5_000 {
        let _ = scheduler
            .submit_tx(OutboundFrame::coalesced(
                Bytes::from(format!("kp={i}")),
                "uart1:kp",
            ))
            .unwrap();
    }
    scheduler
        .submit_tx(OutboundFrame::fire_and_forget(
            Bytes::from_static(b"ESTOP"),
            TxPriority::High,
        ))
        .unwrap();

    // Wait until both ESTOP and final kp=4999 are observed in the RX echo stream.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut seen_estop = false;
    let mut seen_final_kp = false;
    while std::time::Instant::now() < deadline && !(seen_estop && seen_final_kp) {
        if let Ok(pkt) = rx.recv_timeout(Duration::from_millis(50)) {
            if &pkt.bytes[..] == b"ESTOP" {
                seen_estop = true;
            } else if &pkt.bytes[..] == b"kp=4999" {
                seen_final_kp = true;
            }
        }
    }
    assert!(seen_estop, "High priority ESTOP must be delivered");
    assert!(seen_final_kp, "final coalesced kp=4999 must be delivered");

    let snap = scheduler.metrics_snapshot();
    assert!(
        snap.tx_coalesced >= 4_990,
        "expected heavy coalescing, got {}",
        snap.tx_coalesced
    );
    let sent = delivered_tx.lock().clone();
    assert!(sent.len() <= 20, "slow TX must not drain 5000 stale frames (got {})", sent.len());
    assert_eq!(sent.last().map(Vec::as_slice), Some(b"kp=4999".as_slice()));

    // Raw tap observed both TX and RX frames; detach tap and verify no further tap records arrive.
    let tap_snapshot = tap.snapshot();
    let tap_count_before_detach = tap_snapshot.len();
    assert!(tap_count_before_detach > 0);
    assert!(tap_snapshot
        .iter()
        .any(|(dir, _, data)| *dir == RawTapDirection::Tx && data == b"ESTOP"));
    assert!(tap_snapshot
        .iter()
        .any(|(dir, _, data)| *dir == RawTapDirection::Rx && data == b"ESTOP"));
    scheduler.set_raw_tap(None).unwrap();

    scheduler
        .submit_tx(OutboundFrame::fire_and_forget(
            Bytes::from_static(b"POST_DETACH"),
            TxPriority::Normal,
        ))
        .unwrap();
    let post_pkt = rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(&post_pkt.bytes[..], b"POST_DETACH");
    assert_eq!(tap.snapshot().len(), tap_count_before_detach);

    // Stop and restart cleanly.
    rt.block_on(scheduler.stop()).unwrap();
    assert_eq!(scheduler.state(), TransportState::Stopped);
    assert!(!scheduler.is_running());

    let rx2 = scheduler.start_on(rt.handle()).unwrap();
    assert_eq!(scheduler.state(), TransportState::Running);
    scheduler
        .submit_tx(OutboundFrame::fire_and_forget(
            Bytes::from_static(b"RESTART_OK"),
            TxPriority::High,
        ))
        .unwrap();
    let restarted = rx2.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(&restarted.bytes[..], b"RESTART_OK");
    rt.block_on(scheduler.stop()).unwrap();

    // Part B: DropNewest backpressure drops overflow without blocking IO.
    let (burst_tx, burst_rx) = crossbeam_channel::bounded(1);
    let shared_metrics = Arc::new(TransportMetrics::new());
    let mut drop_t =
        PacedFullDuplexTransport::new(Duration::ZERO, RxBackpressurePolicy::DropNewest);
    drop_t.burst_on_start = 40;
    drop_t.burst_done_tx = Some(burst_tx);
    drop_t.shared_metrics = Some(Arc::clone(&shared_metrics));

    let mut drop_sched = TransportScheduler::new(
        Box::new(drop_t),
        TransportRuntimeConfig {
            rx_ring_capacity: 8,
            rx_backpressure: RxBackpressurePolicy::DropNewest,
            ..TransportRuntimeConfig::default()
        },
    );
    let drop_rx = drop_sched.start_on(rt.handle()).unwrap();
    burst_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(drop_rx.len(), 8);
    assert_eq!(shared_metrics.snapshot().dropped_packets, 32);
    assert_eq!(shared_metrics.snapshot().rx_packets, 8);
    rt.block_on(drop_sched.stop()).unwrap();

    // Part C: Block backpressure delivers 100% of packets with 0 drops.
    let (block_tx, block_rx) = crossbeam_channel::bounded(1);
    let block_metrics = Arc::new(TransportMetrics::new());
    let mut block_t = PacedFullDuplexTransport::new(Duration::ZERO, RxBackpressurePolicy::Block);
    block_t.burst_on_start = 32;
    block_t.burst_done_tx = Some(block_tx);
    block_t.shared_metrics = Some(Arc::clone(&block_metrics));

    let mut block_sched = TransportScheduler::new(
        Box::new(block_t),
        TransportRuntimeConfig {
            rx_ring_capacity: 4,
            rx_backpressure: RxBackpressurePolicy::Block,
            ..TransportRuntimeConfig::default()
        },
    );
    let block_out = block_sched.start_on(rt.handle()).unwrap();
    let mut received = 0usize;
    while received < 32 {
        let _pkt = block_out.recv_timeout(Duration::from_secs(2)).unwrap();
        received += 1;
    }
    block_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(block_metrics.snapshot().rx_packets, 32);
    assert_eq!(block_metrics.snapshot().dropped_packets, 0);
    rt.block_on(block_sched.stop()).unwrap();
}

#[test]
fn t7_transport_registry_dispatch_and_reserved_extension_hooks() {
    static CREATED_COUNT: AtomicU64 = AtomicU64::new(0);

    fn create_sim(json_cfg: &str) -> CoreResult<Box<dyn Transport>> {
        if !json_cfg.contains("sim") {
            return Err(CoreError::InvalidConfig("expected sim config".into()));
        }
        CREATED_COUNT.fetch_add(1, Ordering::Relaxed);
        Ok(Box::new(PacedFullDuplexTransport::new(
            Duration::ZERO,
            RxBackpressurePolicy::DropNewest,
        )))
    }

    let mut registry = TransportRegistry::new();
    registry.register(TransportKind::Sim, create_sim);
    assert!(registry.contains(TransportKind::Sim));
    assert_eq!(registry.registered_kinds(), vec![TransportKind::Sim]);

    let sim = registry
        .create(TransportKind::Sim, r#"{"kind":"sim"}"#)
        .expect("registered kind must instantiate");
    assert_eq!(sim.capabilities().kind, TransportKind::Sim);
    assert_eq!(CREATED_COUNT.load(Ordering::Relaxed), 1);

    for reserved in [
        TransportKind::UsbBulk,
        TransportKind::DebugProbeRtt,
        TransportKind::CanFd,
    ] {
        let err = registry.create(reserved, "{}");
        assert!(matches!(err, Err(CoreError::Unsupported(_))));
    }
}
