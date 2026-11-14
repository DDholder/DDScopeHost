//! Native-width columnar channel storage, M4 pyramid, chunk pinning, and watermark hooks.

pub mod event_series;
pub mod hub;
pub mod m4;
pub mod quota;
pub mod scalar_series;

pub use event_series::EventSeries;
pub use hub::{ChannelEntry, ChannelHub, ChannelStore};
pub use m4::{
    aggregate_buckets_to_columns, compute_track_stats_from_columns, compute_track_stats_from_raw,
    feed_bucket_into_slots, finish_pixel_slots, ChunkM4Levels, M4QueryResult, MultiLevelM4Pyramid,
    PixelSlot, ViewportExtract, ViewportRenderData,
};
pub use quota::{
    DEFAULT_MEMORY_QUOTA_BYTES, DEFAULT_PINNED_LIMIT_PERCENT, DEFAULT_QUOTA_BYTES,
    DEFAULT_WARNING_PERCENT, MemoryQuotaManager, MemoryQuotaStats, PinnedGuard, QuotaError,
    PINNED_QUOTA_MAX_FRACTION, WARNING_THRESHOLD_FRACTION,
};
pub use scalar_series::{
    extract_chunks_digital_edges, query_chunks_m4_viewport, query_chunks_m4_viewport_with_stats,
    ActiveScalarChunk, NativeScalarSeries,
};

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
