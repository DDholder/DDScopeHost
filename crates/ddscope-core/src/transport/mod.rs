//! Multi-source transport layer, slab buffer pool, async scheduler, and coalescing downlink queue.

pub mod buffer_pool;
pub mod recorder;
pub mod replay;
pub mod scheduler;
pub mod tx_queue;

pub use buffer_pool::{
    BufferPoolConfig, BufferPoolStats, PacketBufferPool, DEFAULT_MAX_RETAINED_SLABS,
    DEFAULT_PREALLOC_SLABS, DEFAULT_SLAB_BYTES, OS_PAGE_BYTES,
};
pub use recorder::{
    DdrawHeader, DdrawReader, DdrawRecord, RawByteRecorder, RawRecordFormat, RawRecorderConfig,
    RecorderStats, DDRAW_FILE_HEADER_BYTES, DDRAW_MAGIC, DDRAW_RECORD_HEADER_BYTES,
    DDRAW_VERSION, DEFAULT_RECORDER_CHANNEL_CAPACITY, DEFAULT_RECORDER_WRITER_BUFFER_BYTES,
};
pub use replay::{
    create_replay_transport, RawFileReplayConfig, RawFileReplayTransport, ReadOnlyMmap,
    ReplayControlHandle, DEFAULT_NOMINAL_BYTE_RATE, DEFAULT_REPLAY_CHUNK_BYTES,
    MAX_PLAYBACK_SPEED, MIN_PLAYBACK_SPEED,
};
pub use scheduler::{
    host_monotonic_ns, new_shared_raw_tap, MonotonicClock, RawByteTap, RawTapDirection,
    RxBackpressurePolicy, RxPacketEmitter, SharedRawTap, TransportMetrics,
    TransportMetricsSnapshot, TransportRegistry, TransportRuntimeConfig, TransportScheduler,
    DEFAULT_RX_RING_CAPACITY, DEFAULT_TX_HANDOFF_CAPACITY,
};
pub use tx_queue::{
    SharedTxQueue, TxCoalescingQueue, TxEnqueueReceipt, TxQueueStats, TxStreamConsumer,
    DEFAULT_TX_QUEUE_CAPACITY,
};

use std::sync::Arc;

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::types::{CoreResult, TimestampNs};

/// Zero-copy raw byte packet stamped with monotonic host arrival time.
#[derive(Clone, Debug)]
pub struct RawPacket {
    pub bytes: Bytes,
    pub host_recv_ns: TimestampNs,
    pub source_tag: Option<Arc<str>>,
}

/// Priority lane for outbound frames submitted to a transport's TX queue.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TxPriority {
    High,
    #[default]
    Normal,
}

/// Outbound byte frame queued for transmission through a transport.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboundFrame {
    pub payload: Bytes,
    pub priority: TxPriority,
    /// Optional key used by `TxCoalescingQueue` to overwrite stale unsent parameter updates.
    pub coalesce_key: Option<String>,
    /// Reserved transaction identifier for Phase 2 ACK request-response correlation (Hook `H2`).
    pub transaction_id: Option<u64>,
}

impl OutboundFrame {
    /// Creates a non-coalesced fire-and-forget frame.
    pub fn fire_and_forget(payload: impl Into<Bytes>, priority: TxPriority) -> Self {
        Self {
            payload: payload.into(),
            priority,
            coalesce_key: None,
            transaction_id: None,
        }
    }

    /// Creates a normal-priority frame whose unsent payload can be coalesced by key.
    pub fn coalesced(payload: impl Into<Bytes>, key: impl Into<String>) -> Self {
        Self {
            payload: payload.into(),
            priority: TxPriority::Normal,
            coalesce_key: Some(key.into()),
            transaction_id: None,
        }
    }

    /// Creates a frame with an explicit priority whose unsent payload can be coalesced by key.
    pub fn coalesced_with_priority(
        payload: impl Into<Bytes>,
        key: impl Into<String>,
        priority: TxPriority,
    ) -> Self {
        Self {
            payload: payload.into(),
            priority,
            coalesce_key: Some(key.into()),
            transaction_id: None,
        }
    }

    /// Attaches a transaction identifier for Phase 2 ACK request-response correlation.
    pub fn with_transaction_id(mut self, transaction_id: u64) -> Self {
        self.transaction_id = Some(transaction_id);
        self
    }
}

/// Transport implementation family (Phase 1 active variants + future extension hooks).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TransportKind {
    Sim,
    Serial,
    Udp,
    TcpClient,
    TcpServer,
    RawFileReplay,
    UsbBulk,
    DebugProbeRtt,
    CanFd,
}

/// Runtime connection state reported by a source transport.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TransportState {
    #[default]
    Stopped,
    Connecting,
    Running,
    Reconnecting,
    Error,
}

/// Static capabilities advertised by a [`Transport`] implementation.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportCaps {
    pub kind: TransportKind,
    pub supports_tx: bool,
    pub supports_seek: bool,
    pub supports_raw_record: bool,
    pub supports_auto_reconnect: bool,
}

/// Lock-free bounded sender pushing raw RX packets from IO tasks to protocol workers.
pub type PacketSender = crossbeam_channel::Sender<RawPacket>;

/// Lock-free bounded receiver consuming raw RX packets in protocol workers.
pub type PacketReceiver = crossbeam_channel::Receiver<RawPacket>;

/// Async sender feeding outbound downlink frames into a transport's TX task.
pub type OutboundSender = mpsc::Sender<OutboundFrame>;

/// Async receiver drained by a transport's TX task.
pub type OutboundReceiver = mpsc::Receiver<OutboundFrame>;

/// Shutdown signal publisher for cleanly stopping transport IO tasks.
pub type ShutdownSender = watch::Sender<bool>;

/// Shutdown signal subscriber observed by transport IO tasks.
pub type ShutdownReceiver = watch::Receiver<bool>;

/// Unified asynchronous IO interface implemented by all data sources (`Sim`, `Serial`, `UDP`, `TCP`, `Replay`).
pub trait Transport: Send {
    /// Returns static capability flags for this transport instance.
    fn capabilities(&self) -> TransportCaps;

    /// Spawns the transport's async IO loop, forwarding RX packets to `rx_sink`
    /// and draining outbound frames from `tx_rx` until `shutdown_rx` signals `true`.
    fn start(
        &mut self,
        rx_sink: PacketSender,
        tx_rx: OutboundReceiver,
        shutdown_rx: ShutdownReceiver,
    ) -> CoreResult<JoinHandle<CoreResult<()>>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyTransport;

    impl Transport for DummyTransport {
        fn capabilities(&self) -> TransportCaps {
            TransportCaps {
                kind: TransportKind::Sim,
                supports_tx: true,
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
            Err(CoreError::Unsupported("dummy".into()))
        }
    }

    use crate::types::CoreError;

    #[test]
    fn outbound_frame_constructors_and_trait_object_compile() {
        let f1 = OutboundFrame::fire_and_forget(Bytes::from_static(b"PING\n"), TxPriority::High);
        assert_eq!(f1.priority, TxPriority::High);
        assert!(f1.coalesce_key.is_none());
        assert!(f1.transaction_id.is_none());

        let f2 = OutboundFrame::coalesced(Bytes::from_static(b"kp=1.5\n"), "uart1:kp");
        assert_eq!(f2.priority, TxPriority::Normal);
        assert_eq!(f2.coalesce_key.as_deref(), Some("uart1:kp"));

        let boxed: Box<dyn Transport> = Box::new(DummyTransport);
        let caps = boxed.capabilities();
        let json = serde_json::to_string(&caps).expect("TransportCaps should serialize");
        assert!(json.contains("Sim"));
    }
}
