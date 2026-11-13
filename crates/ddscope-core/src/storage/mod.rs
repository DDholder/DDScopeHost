//! Native-width columnar channel storage, M4 pyramid, chunk pinning, and watermark hooks.

pub use crate::types::{ChunkHandle, ChunkStorageRef, DataChunk, M4Bucket, MmapChunkLocator};

use crate::types::{ChannelId, TimestampNs};

/// Watermark advancement notification emitted when new samples are appended to storage (Hook `H3`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct WatermarkEvent {
    pub channel_id: ChannelId,
    pub latest_ts_ns: TimestampNs,
    pub total_samples: u64,
}

/// Subscriber hook notified as source channels advance (Phase 2 virtual DAG scheduling hook `H3`).
pub trait WatermarkListener: Send + Sync {
    /// Called after a batch append advances one or more channel watermarks.
    fn on_watermark_advanced(&self, events: &[WatermarkEvent]);
}
