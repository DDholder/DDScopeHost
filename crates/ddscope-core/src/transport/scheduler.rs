//! Async transport lifecycle scheduler, monotonic clock, RX/TX metrics, bypass raw tap,
//! and static transport factory registry.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use arc_swap::ArcSwap;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;

use super::buffer_pool::{BufferPoolConfig, BufferPoolStats, PacketBufferPool};
use super::tx_queue::{
    SharedTxQueue, TxEnqueueReceipt, DEFAULT_TX_QUEUE_CAPACITY,
};
use super::{
    OutboundFrame, PacketReceiver, PacketSender, RawPacket, ShutdownSender, Transport,
    TransportCaps, TransportKind, TransportState,
};
use crate::plugin::TransportFactoryFn;
use crate::types::{CoreError, CoreResult, TimestampNs};

/// Default bounded SPSC/MPMC ring capacity between transport IO and protocol workers.
pub const DEFAULT_RX_RING_CAPACITY: usize = 1_024;

/// Default handoff channel depth between [`SharedTxQueue`] and the transport IO task.
///
/// Keeping this at `1` maximizes the window for in-place coalescing inside [`SharedTxQueue`].
pub const DEFAULT_TX_HANDOFF_CAPACITY: usize = 1;

static HOST_EPOCH: OnceLock<Instant> = OnceLock::new();

/// Returns monotonic nanoseconds elapsed since the process-wide host acquisition epoch.
#[inline]
pub fn host_monotonic_ns() -> TimestampNs {
    let elapsed = HOST_EPOCH.get_or_init(Instant::now).elapsed().as_nanos();
    elapsed.min(u64::MAX as u128) as u64
}

/// Lightweight monotonic timestamp helper anchored to an explicit or shared epoch.
#[derive(Copy, Clone, Debug)]
pub struct MonotonicClock {
    origin: Instant,
}

impl Default for MonotonicClock {
    fn default() -> Self {
        Self::new()
    }
}

impl MonotonicClock {
    /// Creates a clock anchored to the process-wide monotonic epoch.
    pub fn new() -> Self {
        Self {
            origin: *HOST_EPOCH.get_or_init(Instant::now),
        }
    }

    /// Returns monotonic nanoseconds elapsed since `origin`.
    #[inline]
    pub fn now_ns(&self) -> TimestampNs {
        let elapsed = self.origin.elapsed().as_nanos();
        elapsed.min(u64::MAX as u128) as u64
    }
}

/// Direction tag for bypass raw byte capture taps (FR-IO-05).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RawTapDirection {
    Rx,
    Tx,
}

/// Non-blocking bypass observer hook invoked with raw RX/TX wire bytes and host timestamps.
pub trait RawByteTap: Send + Sync {
    /// Observes a raw byte slice flowing through a transport without blocking the IO hot path.
    fn on_raw_bytes(&self, direction: RawTapDirection, timestamp_ns: TimestampNs, bytes: &Bytes);
}

/// Lock-free hot-swappable handle for attaching or detaching a [`RawByteTap`] at runtime.
pub type SharedRawTap = Arc<ArcSwap<Option<Arc<dyn RawByteTap>>>>;

/// Creates an initially detached [`SharedRawTap`] handle.
pub fn new_shared_raw_tap() -> SharedRawTap {
    Arc::new(ArcSwap::from_pointee(None))
}

/// Behavior when the bounded RX packet ring between IO and protocol workers is full.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RxBackpressurePolicy {
    /// Drop the newest packet and increment `dropped_packets` so IO never stalls.
    #[default]
    DropNewest,
    /// Block the producer until space becomes available (useful for offline/file replay).
    Block,
}

/// Runtime tuning parameters for [`TransportScheduler`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportRuntimeConfig {
    pub rx_ring_capacity: usize,
    pub tx_queue_capacity: usize,
    pub tx_handoff_capacity: usize,
    pub buffer_pool: BufferPoolConfig,
    pub rx_backpressure: RxBackpressurePolicy,
}

impl Default for TransportRuntimeConfig {
    fn default() -> Self {
        Self {
            rx_ring_capacity: DEFAULT_RX_RING_CAPACITY,
            tx_queue_capacity: DEFAULT_TX_QUEUE_CAPACITY,
            tx_handoff_capacity: DEFAULT_TX_HANDOFF_CAPACITY,
            buffer_pool: BufferPoolConfig::default(),
            rx_backpressure: RxBackpressurePolicy::default(),
        }
    }
}

/// Point-in-time snapshot of [`TransportMetrics`].
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportMetricsSnapshot {
    pub state: TransportState,
    pub rx_packets: u64,
    pub rx_bytes: u64,
    pub dropped_packets: u64,
    pub tx_submitted: u64,
    pub tx_coalesced: u64,
    pub tx_dispatched: u64,
    pub tx_bytes: u64,
    pub tx_rejected: u64,
}

/// Lock-free atomic telemetry counters maintained per transport instance.
#[derive(Debug)]
pub struct TransportMetrics {
    state: AtomicU8,
    rx_packets: AtomicU64,
    rx_bytes: AtomicU64,
    dropped_packets: AtomicU64,
    tx_submitted: AtomicU64,
    tx_coalesced: AtomicU64,
    tx_dispatched: AtomicU64,
    tx_bytes: AtomicU64,
    tx_rejected: AtomicU64,
}

impl Default for TransportMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl TransportMetrics {
    const STATE_STOPPED: u8 = 0;
    const STATE_CONNECTING: u8 = 1;
    const STATE_RUNNING: u8 = 2;
    const STATE_RECONNECTING: u8 = 3;
    const STATE_ERROR: u8 = 4;

    /// Creates zeroed metrics in [`TransportState::Stopped`].
    pub fn new() -> Self {
        Self {
            state: AtomicU8::new(Self::STATE_STOPPED),
            rx_packets: AtomicU64::new(0),
            rx_bytes: AtomicU64::new(0),
            dropped_packets: AtomicU64::new(0),
            tx_submitted: AtomicU64::new(0),
            tx_coalesced: AtomicU64::new(0),
            tx_dispatched: AtomicU64::new(0),
            tx_bytes: AtomicU64::new(0),
            tx_rejected: AtomicU64::new(0),
        }
    }

    #[inline]
    fn encode_state(state: TransportState) -> u8 {
        match state {
            TransportState::Stopped => Self::STATE_STOPPED,
            TransportState::Connecting => Self::STATE_CONNECTING,
            TransportState::Running => Self::STATE_RUNNING,
            TransportState::Reconnecting => Self::STATE_RECONNECTING,
            TransportState::Error => Self::STATE_ERROR,
        }
    }

    #[inline]
    fn decode_state(code: u8) -> TransportState {
        match code {
            Self::STATE_CONNECTING => TransportState::Connecting,
            Self::STATE_RUNNING => TransportState::Running,
            Self::STATE_RECONNECTING => TransportState::Reconnecting,
            Self::STATE_ERROR => TransportState::Error,
            _ => TransportState::Stopped,
        }
    }

    /// Returns the current [`TransportState`].
    #[inline]
    pub fn state(&self) -> TransportState {
        Self::decode_state(self.state.load(Ordering::Acquire))
    }

    /// Updates the current [`TransportState`].
    #[inline]
    pub fn set_state(&self, state: TransportState) {
        self.state.store(Self::encode_state(state), Ordering::Release);
    }

    /// Records a successfully queued RX packet.
    #[inline]
    pub fn record_rx(&self, byte_len: usize) {
        self.rx_packets.fetch_add(1, Ordering::Relaxed);
        self.rx_bytes.fetch_add(byte_len as u64, Ordering::Relaxed);
    }

    /// Records an RX packet dropped due to ring backpressure.
    #[inline]
    pub fn record_rx_drop(&self) {
        self.dropped_packets.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a TX frame accepted by [`SharedTxQueue`].
    #[inline]
    pub fn record_tx_submit(&self, coalesced: bool) {
        self.tx_submitted.fetch_add(1, Ordering::Relaxed);
        if coalesced {
            self.tx_coalesced.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Records a TX frame rejected due to queue saturation.
    #[inline]
    pub fn record_tx_reject(&self) {
        self.tx_rejected.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a TX frame handed off to the underlying transport IO task.
    #[inline]
    pub fn record_tx_dispatch(&self, byte_len: usize) {
        self.tx_dispatched.fetch_add(1, Ordering::Relaxed);
        self.tx_bytes.fetch_add(byte_len as u64, Ordering::Relaxed);
    }

    /// Returns a consistent-field snapshot of current counters and state.
    pub fn snapshot(&self) -> TransportMetricsSnapshot {
        TransportMetricsSnapshot {
            state: self.state(),
            rx_packets: self.rx_packets.load(Ordering::Relaxed),
            rx_bytes: self.rx_bytes.load(Ordering::Relaxed),
            dropped_packets: self.dropped_packets.load(Ordering::Relaxed),
            tx_submitted: self.tx_submitted.load(Ordering::Relaxed),
            tx_coalesced: self.tx_coalesced.load(Ordering::Relaxed),
            tx_dispatched: self.tx_dispatched.load(Ordering::Relaxed),
            tx_bytes: self.tx_bytes.load(Ordering::Relaxed),
            tx_rejected: self.tx_rejected.load(Ordering::Relaxed),
        }
    }

    /// Resets all throughput and drop counters while preserving the current [`TransportState`].
    pub fn reset_counters(&self) {
        self.rx_packets.store(0, Ordering::Relaxed);
        self.rx_bytes.store(0, Ordering::Relaxed);
        self.dropped_packets.store(0, Ordering::Relaxed);
        self.tx_submitted.store(0, Ordering::Relaxed);
        self.tx_coalesced.store(0, Ordering::Relaxed);
        self.tx_dispatched.store(0, Ordering::Relaxed);
        self.tx_bytes.store(0, Ordering::Relaxed);
        self.tx_rejected.store(0, Ordering::Relaxed);
    }
}

/// Transport-side RX emitter combining [`PacketBufferPool`], bypass raw tap dispatch,
/// backpressure enforcement, and atomic RX metrics.
pub struct RxPacketEmitter {
    rx_sink: PacketSender,
    pool: PacketBufferPool,
    backpressure: RxBackpressurePolicy,
    metrics: Arc<TransportMetrics>,
    raw_tap: SharedRawTap,
}

impl std::fmt::Debug for RxPacketEmitter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RxPacketEmitter")
            .field("backpressure", &self.backpressure)
            .field("pool_stats", &self.pool.stats())
            .field("metrics", &self.metrics.snapshot())
            .finish()
    }
}

impl RxPacketEmitter {
    /// Creates an emitter with standalone metrics and detached raw tap.
    pub fn new(
        rx_sink: PacketSender,
        pool_config: BufferPoolConfig,
        backpressure: RxBackpressurePolicy,
    ) -> Self {
        Self::with_shared_hooks(
            rx_sink,
            pool_config,
            backpressure,
            Arc::new(TransportMetrics::new()),
            new_shared_raw_tap(),
        )
    }

    /// Creates an emitter sharing `metrics` and `raw_tap` with a scheduler or session.
    pub fn with_shared_hooks(
        rx_sink: PacketSender,
        pool_config: BufferPoolConfig,
        backpressure: RxBackpressurePolicy,
        metrics: Arc<TransportMetrics>,
        raw_tap: SharedRawTap,
    ) -> Self {
        Self {
            rx_sink,
            pool: PacketBufferPool::with_config(pool_config),
            backpressure,
            metrics,
            raw_tap,
        }
    }

    /// Returns an immutable reference to the underlying [`PacketBufferPool`].
    #[inline]
    pub fn pool(&self) -> &PacketBufferPool {
        &self.pool
    }

    /// Returns a mutable reference to the underlying [`PacketBufferPool`].
    #[inline]
    pub fn pool_mut(&mut self) -> &mut PacketBufferPool {
        &mut self.pool
    }

    /// Returns current buffer pool utilization and reclamation statistics.
    #[inline]
    pub fn pool_stats(&self) -> BufferPoolStats {
        self.pool.stats()
    }

    /// Returns the shared [`TransportMetrics`] handle.
    #[inline]
    pub fn metrics(&self) -> &Arc<TransportMetrics> {
        &self.metrics
    }

    /// Emits an already-frozen [`RawPacket`] to the RX ring according to `backpressure`.
    ///
    /// Returns `Ok(true)` if delivered, `Ok(false)` if dropped under `DropNewest`, or
    /// `Err(CoreError::ChannelClosed)` if the consumer receiver has been dropped.
    pub fn emit_packet(&self, packet: RawPacket) -> CoreResult<bool> {
        let guard = self.raw_tap.load();
        if let Some(ref tap) = **guard {
            tap.on_raw_bytes(RawTapDirection::Rx, packet.host_recv_ns, &packet.bytes);
        }

        let byte_len = packet.bytes.len();
        match self.backpressure {
            RxBackpressurePolicy::DropNewest => match self.rx_sink.try_send(packet) {
                Ok(()) => {
                    self.metrics.record_rx(byte_len);
                    Ok(true)
                }
                Err(crossbeam_channel::TrySendError::Full(_)) => {
                    self.metrics.record_rx_drop();
                    Ok(false)
                }
                Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                    Err(CoreError::ChannelClosed)
                }
            },
            RxBackpressurePolicy::Block => match self.rx_sink.send(packet) {
                Ok(()) => {
                    self.metrics.record_rx(byte_len);
                    Ok(true)
                }
                Err(crossbeam_channel::SendError(_)) => Err(CoreError::ChannelClosed),
            },
        }
    }

    /// Packs `data` into the slab pool and emits it to the RX ring.
    pub fn emit_slice(
        &mut self,
        data: &[u8],
        host_recv_ns: TimestampNs,
        source_tag: Option<Arc<str>>,
    ) -> CoreResult<bool> {
        let packet = self.pool.pack_slice(data, host_recv_ns, source_tag);
        self.emit_packet(packet)
    }

    /// Reads directly from a synchronous source closure into a pooled slab and emits the
    /// resulting [`RawPacket`], returning `Ok(None)` on EOF (`0` bytes read).
    pub fn read_into_and_emit<F>(
        &mut self,
        max_bytes: usize,
        source_tag: Option<Arc<str>>,
        read_fn: F,
    ) -> CoreResult<Option<usize>>
    where
        F: FnOnce(&mut [u8]) -> std::io::Result<usize>,
    {
        let packet_opt = self
            .pool
            .read_into_packet(max_bytes, host_monotonic_ns(), source_tag, read_fn)?;
        match packet_opt {
            None => Ok(None),
            Some(packet) => {
                let len = packet.bytes.len();
                let _accepted = self.emit_packet(packet)?;
                Ok(Some(len))
            }
        }
    }
}

#[derive(Debug)]
struct ActiveTransportRun {
    shutdown_tx: ShutdownSender,
    tx_pump_handle: JoinHandle<()>,
    io_handle: JoinHandle<CoreResult<()>>,
    finished_flag: Arc<AtomicBool>,
}

/// Async lifecycle orchestrator wrapping a [`Transport`] with a [`SharedTxQueue`] pump,
/// bounded RX packet ring, atomic metrics, and runtime-swappable bypass raw tap.
pub struct TransportScheduler {
    transport: Box<dyn Transport>,
    config: TransportRuntimeConfig,
    tx_queue: SharedTxQueue,
    metrics: Arc<TransportMetrics>,
    raw_tap: SharedRawTap,
    active_run: Option<ActiveTransportRun>,
}

impl std::fmt::Debug for TransportScheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransportScheduler")
            .field("capabilities", &self.transport.capabilities())
            .field("config", &self.config)
            .field("state", &self.state())
            .field("is_running", &self.is_running())
            .finish()
    }
}

impl TransportScheduler {
    /// Creates a new scheduler managing `transport` with `config`.
    pub fn new(transport: Box<dyn Transport>, config: TransportRuntimeConfig) -> Self {
        let tx_queue = SharedTxQueue::new(config.tx_queue_capacity);
        Self {
            transport,
            config,
            tx_queue,
            metrics: Arc::new(TransportMetrics::new()),
            raw_tap: new_shared_raw_tap(),
            active_run: None,
        }
    }

    /// Returns static capabilities of the managed [`Transport`].
    #[inline]
    pub fn capabilities(&self) -> TransportCaps {
        self.transport.capabilities()
    }

    /// Returns the scheduler's runtime configuration.
    #[inline]
    pub fn config(&self) -> &TransportRuntimeConfig {
        &self.config
    }

    /// Returns the current [`TransportState`].
    #[inline]
    pub fn state(&self) -> TransportState {
        self.metrics.state()
    }

    /// Returns `true` when the managed transport IO task is currently active.
    #[inline]
    pub fn is_running(&self) -> bool {
        self.active_run
            .as_ref()
            .is_some_and(|run| !run.finished_flag.load(Ordering::Acquire))
    }

    /// Returns the shared atomic [`TransportMetrics`] handle.
    #[inline]
    pub fn metrics(&self) -> &Arc<TransportMetrics> {
        &self.metrics
    }

    /// Returns a snapshot of current transport metrics.
    #[inline]
    pub fn metrics_snapshot(&self) -> TransportMetricsSnapshot {
        self.metrics.snapshot()
    }

    /// Returns a reference to the coalescing [`SharedTxQueue`].
    #[inline]
    pub fn tx_queue(&self) -> &SharedTxQueue {
        &self.tx_queue
    }

    /// Returns the shared bypass [`SharedRawTap`] handle.
    #[inline]
    pub fn raw_tap(&self) -> &SharedRawTap {
        &self.raw_tap
    }

    /// Starts the managed transport on the current Tokio runtime and returns the RX receiver.
    pub fn start(&mut self) -> CoreResult<PacketReceiver> {
        let handle = tokio::runtime::Handle::try_current().map_err(|_| {
            CoreError::Transport(
                "TransportScheduler::start requires an active Tokio runtime (use start_on outside async context)"
                    .into(),
            )
        })?;
        self.start_on(&handle)
    }

    /// Starts the managed transport on the provided Tokio runtime `handle` and returns the RX receiver.
    pub fn start_on(&mut self, handle: &tokio::runtime::Handle) -> CoreResult<PacketReceiver> {
        if let Some(ref run) = self.active_run {
            if !run.finished_flag.load(Ordering::Acquire) {
                return Err(CoreError::Transport("transport is already running".into()));
            }
            if let Some(stale) = self.active_run.take() {
                stale.tx_pump_handle.abort();
                stale.io_handle.abort();
            }
        }

        self.metrics.set_state(TransportState::Connecting);
        let (rx_tx, rx_rx) = crossbeam_channel::bounded(self.config.rx_ring_capacity.max(1));
        let (tx_handoff_tx, tx_handoff_rx) =
            tokio::sync::mpsc::channel(self.config.tx_handoff_capacity.max(1));
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        self.tx_queue.clear();
        self.tx_queue.reopen();

        let inner_io_handle = {
            let _enter = handle.enter();
            match self.transport.start(rx_tx, tx_handoff_rx, shutdown_rx.clone()) {
                Ok(join) => join,
                Err(err) => {
                    self.metrics.set_state(TransportState::Error);
                    self.tx_queue.close();
                    return Err(err);
                }
            }
        };

        self.metrics.set_state(TransportState::Running);

        let pump_queue = self.tx_queue.clone();
        let pump_metrics = Arc::clone(&self.metrics);
        let pump_tap = Arc::clone(&self.raw_tap);
        let mut pump_shutdown_rx = shutdown_rx;
        let tx_pump_handle = handle.spawn(async move {
            loop {
                if !pump_queue.wait_until_non_empty(&mut pump_shutdown_rx).await {
                    break;
                }
                let permit = tokio::select! {
                    biased;
                    res = pump_shutdown_rx.changed() => {
                        if res.is_err() || *pump_shutdown_rx.borrow() {
                            break;
                        }
                        continue;
                    }
                    res = tx_handoff_tx.reserve() => {
                        match res {
                            Ok(permit) => permit,
                            Err(_) => break,
                        }
                    }
                };
                if let Some(frame) = pump_queue.pop_next() {
                    let tap_guard = pump_tap.load();
                    if let Some(ref tap) = **tap_guard {
                        tap.on_raw_bytes(RawTapDirection::Tx, host_monotonic_ns(), &frame.payload);
                    }
                    pump_metrics.record_tx_dispatch(frame.payload.len());
                    permit.send(frame);
                }
            }
        });

        let finished_flag = Arc::new(AtomicBool::new(false));
        let io_finished = Arc::clone(&finished_flag);
        let io_metrics = Arc::clone(&self.metrics);
        let io_tx_queue = self.tx_queue.clone();
        let io_shutdown_tx = shutdown_tx.clone();
        let io_handle = handle.spawn(async move {
            let outcome = match inner_io_handle.await {
                Ok(res) => res,
                Err(join_err) => {
                    if join_err.is_cancelled() {
                        Ok(())
                    } else {
                        Err(CoreError::Transport(format!(
                            "transport IO task panicked: {join_err}"
                        )))
                    }
                }
            };
            io_finished.store(true, Ordering::Release);
            match &outcome {
                Ok(()) => io_metrics.set_state(TransportState::Stopped),
                Err(_) => io_metrics.set_state(TransportState::Error),
            }
            io_tx_queue.close();
            let _ = io_shutdown_tx.send(true);
            outcome
        });

        self.active_run = Some(ActiveTransportRun {
            shutdown_tx,
            tx_pump_handle,
            io_handle,
            finished_flag,
        });

        Ok(rx_rx)
    }

    /// Submits an outbound downlink frame into the coalescing TX queue.
    pub fn submit_tx(&self, frame: OutboundFrame) -> CoreResult<TxEnqueueReceipt> {
        if !self.capabilities().supports_tx {
            return Err(CoreError::Unsupported(
                "transport does not support outbound TX".into(),
            ));
        }
        if !self.is_running() {
            return Err(CoreError::Transport("transport is not running".into()));
        }
        match self.tx_queue.submit(frame) {
            Ok(receipt) => {
                self.metrics.record_tx_submit(receipt.coalesced_existing);
                Ok(receipt)
            }
            Err(err) => {
                self.metrics.record_tx_reject();
                Err(err)
            }
        }
    }

    /// Gracefully signals shutdown, drains the TX pump and IO tasks, and returns the task result.
    pub async fn stop(&mut self) -> CoreResult<()> {
        if let Some(run) = self.active_run.take() {
            let _ = run.shutdown_tx.send(true);
            self.tx_queue.close();
            let _ = run.tx_pump_handle.await;
            let io_result = match run.io_handle.await {
                Ok(res) => res,
                Err(join_err) => Err(CoreError::Transport(format!(
                    "transport supervisor task failed: {join_err}"
                ))),
            };
            self.tx_queue.clear();
            if io_result.is_ok() {
                self.metrics.set_state(TransportState::Stopped);
            } else {
                self.metrics.set_state(TransportState::Error);
            }
            io_result
        } else {
            self.tx_queue.clear();
            Ok(())
        }
    }

    /// Attaches or detaches a bypass [`RawByteTap`] observer at runtime.
    pub fn set_raw_tap(&self, tap: Option<Arc<dyn RawByteTap>>) -> CoreResult<()> {
        if tap.is_some() && !self.capabilities().supports_raw_record {
            return Err(CoreError::Unsupported(
                "transport does not support bypass raw recording".into(),
            ));
        }
        self.raw_tap.store(Arc::new(tap));
        Ok(())
    }
}

impl Drop for TransportScheduler {
    fn drop(&mut self) {
        self.tx_queue.close();
        if let Some(run) = self.active_run.take() {
            let _ = run.shutdown_tx.send(true);
            run.tx_pump_handle.abort();
            run.io_handle.abort();
        }
    }
}

/// Static factory registry decoupling transport instantiation from concrete implementations.
#[derive(Default)]
pub struct TransportRegistry {
    factories: HashMap<TransportKind, TransportFactoryFn>,
}

impl std::fmt::Debug for TransportRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut kinds: Vec<_> = self.factories.keys().copied().collect();
        kinds.sort_by_key(|k| format!("{k:?}"));
        f.debug_struct("TransportRegistry")
            .field("registered_kinds", &kinds)
            .finish()
    }
}

impl TransportRegistry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers or replaces the factory function for `kind`.
    pub fn register(&mut self, kind: TransportKind, factory: TransportFactoryFn) {
        self.factories.insert(kind, factory);
    }

    /// Removes any factory registered for `kind`.
    pub fn unregister(&mut self, kind: TransportKind) -> Option<TransportFactoryFn> {
        self.factories.remove(&kind)
    }

    /// Returns `true` if a factory is registered for `kind`.
    #[inline]
    pub fn contains(&self, kind: TransportKind) -> bool {
        self.factories.contains_key(&kind)
    }

    /// Returns all currently registered [`TransportKind`] entries.
    pub fn registered_kinds(&self) -> Vec<TransportKind> {
        self.factories.keys().copied().collect()
    }

    /// Instantiates a [`Transport`] of `kind` from serialized JSON configuration.
    pub fn create(&self, kind: TransportKind, json_config: &str) -> CoreResult<Box<dyn Transport>> {
        let factory = self.factories.get(&kind).ok_or_else(|| {
            CoreError::Unsupported(format!(
                "no transport factory registered for kind {kind:?}"
            ))
        })?;
        factory(json_config)
    }
}

#[cfg(test)]
mod tests {
    use parking_lot::Mutex;

    use super::*;
    use crate::transport::{OutboundReceiver, ShutdownReceiver, TxPriority};

    struct LoopbackMockTransport {
        caps: TransportCaps,
        fail_on_start: bool,
        tx_log: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    impl LoopbackMockTransport {
        fn new(supports_tx: bool, supports_raw_record: bool) -> Self {
            Self {
                caps: TransportCaps {
                    kind: TransportKind::Sim,
                    supports_tx,
                    supports_seek: false,
                    supports_raw_record,
                    supports_auto_reconnect: false,
                },
                fail_on_start: false,
                tx_log: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl Transport for LoopbackMockTransport {
        fn capabilities(&self) -> TransportCaps {
            self.caps
        }

        fn start(
            &mut self,
            rx_sink: PacketSender,
            mut tx_rx: OutboundReceiver,
            mut shutdown_rx: ShutdownReceiver,
        ) -> CoreResult<JoinHandle<CoreResult<()>>> {
            if self.fail_on_start {
                return Err(CoreError::Transport("simulated start failure".into()));
            }
            let tx_log = Arc::clone(&self.tx_log);
            Ok(tokio::spawn(async move {
                let mut emitter = RxPacketEmitter::new(
                    rx_sink,
                    BufferPoolConfig::default(),
                    RxBackpressurePolicy::DropNewest,
                );
                loop {
                    tokio::select! {
                        biased;
                        res = shutdown_rx.changed() => {
                            if res.is_err() || *shutdown_rx.borrow() {
                                break;
                            }
                        }
                        maybe_frame = tx_rx.recv() => {
                            match maybe_frame {
                                Some(frame) => {
                                    tx_log.lock().push(frame.payload.to_vec());
                                    let _ = emitter.emit_slice(&frame.payload, host_monotonic_ns(), None);
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
    fn host_monotonic_clock_is_non_decreasing() {
        let clock = MonotonicClock::new();
        let t0 = host_monotonic_ns();
        let t1 = clock.now_ns();
        let t2 = host_monotonic_ns();
        assert!(t1 >= t0);
        assert!(t2 >= t1);
    }

    #[tokio::test]
    async fn scheduler_start_submit_and_stop_lifecycle() {
        let transport = LoopbackMockTransport::new(true, true);
        let tx_log = Arc::clone(&transport.tx_log);
        let mut scheduler =
            TransportScheduler::new(Box::new(transport), TransportRuntimeConfig::default());

        assert_eq!(scheduler.state(), TransportState::Stopped);
        assert!(!scheduler.is_running());

        let rx = scheduler.start().expect("scheduler should start");
        assert_eq!(scheduler.state(), TransportState::Running);
        assert!(scheduler.is_running());

        scheduler
            .submit_tx(OutboundFrame::fire_and_forget(
                Bytes::from_static(b"PING"),
                TxPriority::High,
            ))
            .expect("TX submit should succeed");

        let pkt = tokio::task::spawn_blocking(move || {
            rx.recv_timeout(std::time::Duration::from_secs(2))
        })
        .await
        .unwrap()
        .expect("loopback packet should arrive");
        assert_eq!(&pkt.bytes[..], b"PING");
        assert_eq!(tx_log.lock().as_slice(), &[b"PING".to_vec()]);

        scheduler.stop().await.expect("clean stop");
        assert_eq!(scheduler.state(), TransportState::Stopped);
        assert!(!scheduler.is_running());
    }

    #[tokio::test]
    async fn scheduler_propagates_start_failure_and_supports_unsupported_tx_guard() {
        let mut fail_transport = LoopbackMockTransport::new(false, false);
        fail_transport.fail_on_start = true;
        let mut fail_scheduler =
            TransportScheduler::new(Box::new(fail_transport), TransportRuntimeConfig::default());
        assert!(fail_scheduler.start().is_err());
        assert_eq!(fail_scheduler.state(), TransportState::Error);

        let rx_only = LoopbackMockTransport::new(false, false);
        let mut rx_only_sched =
            TransportScheduler::new(Box::new(rx_only), TransportRuntimeConfig::default());
        let _rx = rx_only_sched.start().unwrap();
        let tx_err = rx_only_sched
            .submit_tx(OutboundFrame::fire_and_forget(
                Bytes::from_static(b"X"),
                TxPriority::Normal,
            ))
            .unwrap_err();
        assert!(matches!(tx_err, CoreError::Unsupported(_)));
        rx_only_sched.stop().await.unwrap();
    }

    #[test]
    fn transport_registry_registers_and_rejects_unregistered_kinds() {
        fn make_mock(_cfg: &str) -> CoreResult<Box<dyn Transport>> {
            Ok(Box::new(LoopbackMockTransport::new(true, true)))
        }

        let mut registry = TransportRegistry::new();
        registry.register(TransportKind::Sim, make_mock);
        assert!(registry.contains(TransportKind::Sim));
        assert!(!registry.contains(TransportKind::UsbBulk));

        let created = registry.create(TransportKind::Sim, "{}").unwrap();
        assert_eq!(created.capabilities().kind, TransportKind::Sim);

        let missing = registry.create(TransportKind::CanFd, "{}");
        assert!(matches!(missing, Err(CoreError::Unsupported(_))));
    }
}
