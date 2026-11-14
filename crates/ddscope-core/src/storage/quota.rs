//! Standalone 2GB ring buffer memory quota manager and RAII chunk pinning (`PinnedGuard`).
//!
//! Provides thread-safe, lock-free memory tracking, threshold evaluations (85% warning,
//! 40% pinning ceiling), and integration with DDSF v1 frame headers.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::scope::ddsf::{DDSF_FLAG_PINNED_ACTIVE, DDSF_FLAG_QUOTA_WARNING};
use crate::types::{ChannelId, CoreError, DataChunk, TimestampNs};

/// Default global memory quota limit: 2 GiB (2,147,483,648 bytes).
pub const DEFAULT_MEMORY_QUOTA_BYTES: usize = 2 * 1024 * 1024 * 1024;

/// Alias for default quota bytes.
pub const DEFAULT_QUOTA_BYTES: usize = DEFAULT_MEMORY_QUOTA_BYTES;

/// Default warning threshold fraction (85% of total quota).
/// Sets `DDSF_FLAG_QUOTA_WARNING` in DDSF frame header.
pub const WARNING_THRESHOLD_FRACTION: f64 = 0.85;

/// Default warning threshold percentage: 85%.
pub const DEFAULT_WARNING_PERCENT: usize = 85;

/// Default maximum fraction of quota reserved for pinned chunks (40% of total quota).
/// Caps pinned reservation to at most ~800MB under default 2GB quota.
pub const PINNED_QUOTA_MAX_FRACTION: f64 = 0.40;

/// Default pinned reservation ceiling percentage: 40%.
pub const DEFAULT_PINNED_LIMIT_PERCENT: usize = 40;

// Re-export DDSF wire flags for convenience.
pub use crate::scope::ddsf::DDSF_FLAG_PINNED_ACTIVE as DDSF_FLAG_PINNED_ACTIVE_CONST;
pub use crate::scope::ddsf::DDSF_FLAG_QUOTA_WARNING as DDSF_FLAG_QUOTA_WARNING_CONST;

/// Detailed error enumeration for quota and pinning failures.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum QuotaError {
    /// Total memory allocation exceeded the configured quota limit.
    #[error("Memory quota exceeded: requested {requested_bytes} bytes against limit {limit_bytes} bytes")]
    QuotaExceeded {
        requested_bytes: usize,
        limit_bytes: usize,
    },

    /// Pinned memory request exceeded the 40% quota ceiling.
    #[error("Pinning memory ceiling exceeded (40% limit): requested {requested_bytes} bytes, current pinned {current_pinned_bytes} bytes, limit {pinned_limit_bytes} bytes")]
    PinnedLimitExceeded {
        requested_bytes: usize,
        current_pinned_bytes: usize,
        pinned_limit_bytes: usize,
    },

    /// General storage error.
    #[error("Storage error: {0}")]
    Storage(String),
}

impl From<QuotaError> for CoreError {
    fn from(err: QuotaError) -> Self {
        match err {
            QuotaError::QuotaExceeded {
                requested_bytes,
                limit_bytes,
            } => CoreError::QuotaExceeded {
                requested_bytes,
                limit_bytes,
            },
            QuotaError::PinnedLimitExceeded {
                requested_bytes,
                current_pinned_bytes,
                pinned_limit_bytes,
            } => CoreError::Storage(format!(
                "Pinning memory ceiling exceeded (40% limit): requested {requested_bytes} bytes, current pinned {current_pinned_bytes} bytes, limit {pinned_limit_bytes} bytes"
            )),
            QuotaError::Storage(msg) => CoreError::Storage(msg),
        }
    }
}

/// Comprehensive snapshot of memory quota and pinning metrics.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MemoryQuotaStats {
    pub quota_bytes: usize,
    pub allocated_bytes: usize,
    pub pinned_bytes: usize,
    pub warning_threshold_bytes: usize,
    pub pinned_limit_bytes: usize,
    pub is_warning: bool,
    pub is_over_quota: bool,
    pub is_pinned_active: bool,
    pub allocation_ratio: f64,
    pub pinned_ratio: f64,
    pub flags: u16,
}

impl MemoryQuotaStats {
    /// Alias for `pinned_limit_bytes`.
    #[inline]
    pub fn pinned_ceiling_bytes(&self) -> usize {
        self.pinned_limit_bytes
    }
}

/// Standalone memory quota coordinator managing global memory bounds,
/// threshold warnings, and chunk pinning limits.
#[derive(Debug)]
pub struct MemoryQuotaManager {
    quota_bytes: usize,
    warning_threshold_bytes: usize,
    pinned_limit_bytes: usize,
    allocated_bytes: AtomicUsize,
    pinned_bytes: AtomicUsize,
}

impl Default for MemoryQuotaManager {
    fn default() -> Self {
        Self::with_default_quota()
    }
}

impl MemoryQuotaManager {
    /// Creates a new quota manager with a specified byte capacity.
    /// Thresholds are computed using integer deterministic arithmetic:
    /// 85% for warning, 40% for pinning ceiling.
    pub fn new(quota_bytes: usize) -> Self {
        let warning_threshold_bytes = (quota_bytes as u64 * 85 / 100) as usize;
        let pinned_limit_bytes = (quota_bytes as u64 * 40 / 100) as usize;
        Self {
            quota_bytes,
            warning_threshold_bytes,
            pinned_limit_bytes,
            allocated_bytes: AtomicUsize::new(0),
            pinned_bytes: AtomicUsize::new(0),
        }
    }

    /// Creates a new quota manager with the default 2 GiB limit.
    pub fn with_default_quota() -> Self {
        Self::new(DEFAULT_MEMORY_QUOTA_BYTES)
    }

    /// Alias for test compatibility: creates a 2 GiB quota manager.
    pub fn new_2gb() -> Self {
        Self::with_default_quota()
    }

    /// Records an allocation of `bytes` unconditionally.
    /// Returns `true` if total allocation is now within the quota limit, or `false` if over quota.
    pub fn record_allocation(&self, bytes: usize) -> bool {
        if bytes == 0 {
            return !self.is_over_quota();
        }
        let prev = self.allocated_bytes.fetch_add(bytes, Ordering::AcqRel);
        prev.saturating_add(bytes) <= self.quota_bytes
    }

    /// Records a deallocation of `bytes`, saturating at zero (underflow-safe).
    pub fn record_deallocation(&self, bytes: usize) {
        if bytes == 0 {
            return;
        }
        let mut curr = self.allocated_bytes.load(Ordering::Acquire);
        loop {
            let next = curr.saturating_sub(bytes);
            match self.allocated_bytes.compare_exchange_weak(
                curr,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(actual) => curr = actual,
            }
        }
    }

    /// Compatibility helper: attempts to allocate `bytes`.
    /// Returns `Err(CoreError::Storage)` if exceeding quota limit.
    pub fn allocate(&self, bytes: usize) -> Result<(), CoreError> {
        if bytes == 0 {
            return Ok(());
        }
        let mut curr = self.allocated_bytes.load(Ordering::Acquire);
        loop {
            if curr.saturating_add(bytes) > self.quota_bytes {
                return Err(CoreError::Storage("Memory quota exceeded".into()));
            }
            match self.allocated_bytes.compare_exchange_weak(
                curr,
                curr + bytes,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(()),
                Err(actual) => curr = actual,
            }
        }
    }

    /// Compatibility helper: releases `bytes` from allocated counter.
    #[inline]
    pub fn release(&self, bytes: usize) {
        self.record_deallocation(bytes);
    }

    /// Attempts to reserve `bytes` against the 40% pinned quota ceiling.
    /// Returns `Ok(())` on success, or `Err(QuotaError::PinnedLimitExceeded)` if ceiling would be exceeded.
    pub fn try_pin(&self, bytes: usize) -> Result<(), QuotaError> {
        if bytes == 0 {
            return Ok(());
        }
        let mut curr = self.pinned_bytes.load(Ordering::Acquire);
        loop {
            if curr.saturating_add(bytes) > self.pinned_limit_bytes {
                return Err(QuotaError::PinnedLimitExceeded {
                    requested_bytes: bytes,
                    current_pinned_bytes: curr,
                    pinned_limit_bytes: self.pinned_limit_bytes,
                });
            }
            match self.pinned_bytes.compare_exchange_weak(
                curr,
                curr + bytes,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(()),
                Err(actual) => curr = actual,
            }
        }
    }

    /// Releases previously pinned bytes, saturating at zero.
    pub fn release_pin(&self, bytes: usize) {
        if bytes == 0 {
            return;
        }
        let mut curr = self.pinned_bytes.load(Ordering::Acquire);
        loop {
            let next = curr.saturating_sub(bytes);
            match self.pinned_bytes.compare_exchange_weak(
                curr,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(actual) => curr = actual,
            }
        }
    }

    /// Compatibility helper: attempts to pin `bytes` of memory and returns a synthetic `PinnedGuard`.
    pub fn pin(self: &Arc<Self>, bytes: usize) -> Result<PinnedGuard, CoreError> {
        self.try_pin(bytes).map_err(CoreError::from)?;
        Ok(PinnedGuard::new_synthetic(Arc::clone(self), bytes))
    }

    /// Returns `true` if current allocation meets or exceeds the 85% warning threshold.
    #[inline]
    pub fn is_warning(&self) -> bool {
        let alloc = self.allocated_bytes.load(Ordering::Acquire);
        if self.quota_bytes == 0 {
            return alloc > 0;
        }
        alloc >= self.warning_threshold_bytes
    }

    /// Returns `true` if total allocated memory strictly exceeds the quota limit.
    #[inline]
    pub fn is_over_quota(&self) -> bool {
        self.allocated_bytes.load(Ordering::Acquire) > self.quota_bytes
    }

    /// Returns `true` if one or more pinned chunks/guards are currently active.
    #[inline]
    pub fn is_pinned_active(&self) -> bool {
        self.pinned_bytes.load(Ordering::Acquire) > 0
    }

    /// Currently allocated bytes.
    #[inline]
    pub fn allocated_bytes(&self) -> usize {
        self.allocated_bytes.load(Ordering::Acquire)
    }

    /// Alias for `allocated_bytes`.
    #[inline]
    pub fn total_allocated(&self) -> usize {
        self.allocated_bytes()
    }

    /// Currently pinned bytes.
    #[inline]
    pub fn pinned_bytes(&self) -> usize {
        self.pinned_bytes.load(Ordering::Acquire)
    }

    /// Configured maximum quota capacity in bytes.
    #[inline]
    pub fn quota_bytes(&self) -> usize {
        self.quota_bytes
    }

    /// Configured warning threshold in bytes (85%).
    #[inline]
    pub fn warning_threshold_bytes(&self) -> usize {
        self.warning_threshold_bytes
    }

    /// Configured pinned ceiling limit in bytes (40%).
    #[inline]
    pub fn pinned_limit_bytes(&self) -> usize {
        self.pinned_limit_bytes
    }

    /// Alias for `pinned_limit_bytes`.
    #[inline]
    pub fn pinned_ceiling_bytes(&self) -> usize {
        self.pinned_limit_bytes
    }

    /// Computes DDSF v1 root frame header flags from current quota status.
    pub fn flags(&self) -> u16 {
        let mut flags = 0u16;
        if self.is_warning() {
            flags |= DDSF_FLAG_QUOTA_WARNING;
        }
        if self.is_pinned_active() {
            flags |= DDSF_FLAG_PINNED_ACTIVE;
        }
        flags
    }

    /// Alias for `flags()`.
    #[inline]
    pub fn ddsf_header_flags(&self) -> u16 {
        self.flags()
    }

    /// Returns a telemetry snapshot for UI monitoring.
    pub fn stats(&self) -> MemoryQuotaStats {
        let allocated = self.allocated_bytes();
        let pinned = self.pinned_bytes();
        let quota = self.quota_bytes;

        MemoryQuotaStats {
            quota_bytes: quota,
            allocated_bytes: allocated,
            pinned_bytes: pinned,
            warning_threshold_bytes: self.warning_threshold_bytes,
            pinned_limit_bytes: self.pinned_limit_bytes,
            is_warning: self.is_warning(),
            is_over_quota: self.is_over_quota(),
            is_pinned_active: self.is_pinned_active(),
            allocation_ratio: if quota > 0 {
                allocated as f64 / quota as f64
            } else {
                0.0
            },
            pinned_ratio: if quota > 0 {
                pinned as f64 / quota as f64
            } else {
                0.0
            },
            flags: self.flags(),
        }
    }
}

/// Scope lock guard holding strong references to a contiguous range of sealed chunks.
/// Automatically releases its pinned byte reservation upon `Drop` and reconciles
/// memory deallocation if chunks were evicted from the series while pinned.
#[derive(Debug)]
pub struct PinnedGuard {
    channel_id: ChannelId,
    chunks: Vec<Arc<DataChunk>>,
    pinned_bytes: usize,
    quota_manager: Arc<MemoryQuotaManager>,
}

impl PinnedGuard {
    /// Creates a new `PinnedGuard` wrapping concrete chunks and reserved bytes.
    pub fn new(
        channel_id: ChannelId,
        chunks: Vec<Arc<DataChunk>>,
        pinned_bytes: usize,
        quota_manager: Arc<MemoryQuotaManager>,
    ) -> Self {
        Self {
            channel_id,
            chunks,
            pinned_bytes,
            quota_manager,
        }
    }

    /// Creates a synthetic bytes-only guard for testing and quota reservation harnesses.
    pub fn new_synthetic(quota_manager: Arc<MemoryQuotaManager>, pinned_bytes: usize) -> Self {
        Self {
            channel_id: ChannelId(0),
            chunks: Vec::new(),
            pinned_bytes,
            quota_manager,
        }
    }

    /// Returns the target channel identifier.
    #[inline]
    pub fn channel_id(&self) -> ChannelId {
        self.channel_id
    }

    /// Returns a borrowed slice of pinned chunks.
    #[inline]
    pub fn chunks(&self) -> &[Arc<DataChunk>] {
        &self.chunks
    }

    /// Returns the number of bytes accounted against the pinned quota ceiling.
    #[inline]
    pub fn pinned_bytes(&self) -> usize {
        self.pinned_bytes
    }

    /// Alias for `pinned_bytes` for compatibility.
    #[inline]
    pub fn bytes(&self) -> usize {
        self.pinned_bytes
    }

    /// Returns the number of chunks protected by this guard.
    #[inline]
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// Returns `true` if the guard protects zero chunks.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    /// Returns the total sample count across all pinned chunks.
    pub fn total_samples(&self) -> usize {
        self.chunks.iter().map(|c| c.sample_count()).sum()
    }

    /// Returns the inclusive time span `(t_start_ns, t_end_ns)` covered by the pinned chunks.
    pub fn time_range_ns(&self) -> Option<(TimestampNs, TimestampNs)> {
        let first = self.chunks.first()?.time_range_ns()?.0;
        let last = self.chunks.last()?.time_range_ns()?.1;
        Some((first, last))
    }

    /// Queries pixel-aligned M4 or raw viewport data directly from these pinned chunks,
    /// remaining valid even after the underlying series rolls past them.
    pub fn query_m4_viewport(
        &self,
        t_min: TimestampNs,
        t_max: TimestampNs,
        w_px: usize,
    ) -> Result<crate::storage::M4QueryResult, CoreError> {
        crate::storage::query_chunks_m4_viewport(&self.chunks, t_min, t_max, w_px)
    }

    /// Queries viewport data and computed statistics directly from these pinned chunks,
    /// remaining valid even after the underlying series rolls past them.
    pub fn query_m4_viewport_with_stats(
        &self,
        t_min: TimestampNs,
        t_max: TimestampNs,
        w_px: usize,
    ) -> Result<crate::storage::ViewportExtract, CoreError> {
        crate::storage::query_chunks_m4_viewport_with_stats(&self.chunks, t_min, t_max, w_px)
    }

    /// Extracts normalized digital transition edges directly from these pinned boolean chunks,
    /// remaining valid even after the underlying series rolls past them.
    pub fn extract_digital_edges(
        &self,
        t_min: TimestampNs,
        t_max: TimestampNs,
    ) -> Result<(u8, Vec<f32>), CoreError> {
        crate::storage::extract_chunks_digital_edges(&self.chunks, t_min, t_max)
    }

    /// Extracts viewport-decimated digital transition edges across `w_px` columns directly
    /// from these pinned boolean chunks, preserving parity and single-sample glitches.
    pub fn extract_digital_edges_viewport(
        &self,
        t_min: TimestampNs,
        t_max: TimestampNs,
        w_px: usize,
    ) -> Result<(u8, Vec<f32>), CoreError> {
        crate::storage::extract_chunks_digital_edges_viewport(&self.chunks, t_min, t_max, w_px)
    }

    /// Extracts normalized state-machine ribbons directly from these pinned event chunks.
    pub fn extract_state_ribbons<F>(
        &self,
        t_min: TimestampNs,
        t_max: TimestampNs,
        color_for_state: F,
    ) -> Result<Vec<crate::scope::ddsf::DdsfStateRibbon>, CoreError>
    where
        F: FnMut(i32) -> u32,
    {
        crate::storage::extract_chunks_state_ribbons(&self.chunks, t_min, t_max, color_for_state)
    }
}

impl Drop for PinnedGuard {
    fn drop(&mut self) {
        // 1. Release the pinned ceiling counter reservation
        self.quota_manager.release_pin(self.pinned_bytes);

        // 2. Reconcile memory for chunks that were evicted from the series while pinned.
        // If strong_count is 1, this guard holds the last reference.
        // When self.chunks is dropped, OS deallocation occurs, so record deallocation.
        for chunk in &self.chunks {
            if Arc::strong_count(chunk) == 1 {
                self.quota_manager.record_deallocation(chunk.byte_footprint());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quota_manager_defaults() {
        let q = MemoryQuotaManager::with_default_quota();
        assert_eq!(q.quota_bytes(), DEFAULT_MEMORY_QUOTA_BYTES);
        assert_eq!(q.allocated_bytes(), 0);
        assert_eq!(q.pinned_bytes(), 0);
        assert_eq!(q.warning_threshold_bytes(), 1_825_361_100);
        assert_eq!(q.pinned_limit_bytes(), 858_993_459);
        assert!(!q.is_warning());
        assert!(!q.is_over_quota());
        assert!(!q.is_pinned_active());
        assert_eq!(q.flags(), 0);
    }

    #[test]
    fn test_allocation_and_deallocation() {
        let q = MemoryQuotaManager::new(1000);
        assert!(q.record_allocation(500));
        assert_eq!(q.allocated_bytes(), 500);
        assert!(!q.is_over_quota());

        assert!(q.record_allocation(500));
        assert_eq!(q.allocated_bytes(), 1000);
        assert!(!q.is_over_quota());

        // Over quota allocation
        assert!(!q.record_allocation(1));
        assert_eq!(q.allocated_bytes(), 1001);
        assert!(q.is_over_quota());

        // Deallocation
        q.record_deallocation(500);
        assert_eq!(q.allocated_bytes(), 501);
        assert!(!q.is_over_quota());

        // Underflow protection
        q.record_deallocation(1000);
        assert_eq!(q.allocated_bytes(), 0);
    }

    #[test]
    fn test_warning_threshold_and_flags() {
        let q = MemoryQuotaManager::new(10_000);
        assert_eq!(q.warning_threshold_bytes(), 8_500);
        assert_eq!(q.pinned_limit_bytes(), 4_000);

        q.record_allocation(8_499);
        assert!(!q.is_warning());
        assert_eq!(q.flags(), 0);

        q.record_allocation(1);
        assert!(q.is_warning());
        assert_eq!(q.flags(), DDSF_FLAG_QUOTA_WARNING);

        // Try pinning
        assert!(q.try_pin(1_000).is_ok());
        assert!(q.is_pinned_active());
        assert_eq!(q.flags(), DDSF_FLAG_QUOTA_WARNING | DDSF_FLAG_PINNED_ACTIVE);

        q.release_pin(1_000);
        assert!(!q.is_pinned_active());
        assert_eq!(q.flags(), DDSF_FLAG_QUOTA_WARNING);
    }

    #[test]
    fn test_pinned_ceiling_enforcement() {
        let q = MemoryQuotaManager::new(10_000);
        assert!(q.try_pin(4_000).is_ok());
        assert_eq!(q.pinned_bytes(), 4_000);

        // Exceed ceiling
        let err = q.try_pin(1).unwrap_err();
        match err {
            QuotaError::PinnedLimitExceeded {
                requested_bytes,
                current_pinned_bytes,
                pinned_limit_bytes,
            } => {
                assert_eq!(requested_bytes, 1);
                assert_eq!(current_pinned_bytes, 4_000);
                assert_eq!(pinned_limit_bytes, 4_000);
            }
            _ => panic!("Expected PinnedLimitExceeded"),
        }
        assert_eq!(q.pinned_bytes(), 4_000);

        q.release_pin(1_000);
        assert_eq!(q.pinned_bytes(), 3_000);
        assert!(q.try_pin(1_000).is_ok());
        assert_eq!(q.pinned_bytes(), 4_000);
    }

    #[test]
    fn test_concurrent_try_pin() {
        let q = Arc::new(MemoryQuotaManager::new(10_000)); // Pinned cap = 4_000
        let mut handles = Vec::new();

        for _ in 0..20 {
            let q_clone = Arc::clone(&q);
            handles.push(std::thread::spawn(move || {
                q_clone.try_pin(300)
            }));
        }

        let mut success = 0;
        let mut failed = 0;
        for h in handles {
            match h.join().unwrap() {
                Ok(_) => success += 1,
                Err(_) => failed += 1,
            }
        }

        assert_eq!(success, 13); // 13 * 300 = 3,900 <= 4,000
        assert_eq!(failed, 7);
        assert_eq!(q.pinned_bytes(), 3_900);
    }

    #[test]
    fn test_pinned_guard_raii() {
        let q = Arc::new(MemoryQuotaManager::new(10_000));
        {
            let guard = q.pin(2_000).unwrap();
            assert_eq!(guard.bytes(), 2_000);
            assert_eq!(guard.pinned_bytes(), 2_000);
            assert_eq!(q.pinned_bytes(), 2_000);
            assert!(guard.is_empty());
            assert_eq!(guard.chunk_count(), 0);
            assert_eq!(guard.total_samples(), 0);
        }
        assert_eq!(q.pinned_bytes(), 0);
    }

    #[test]
    fn test_stats() {
        let q = MemoryQuotaManager::new(10_000);
        q.record_allocation(8_500);
        q.try_pin(2_000).unwrap();

        let s = q.stats();
        assert_eq!(s.quota_bytes, 10_000);
        assert_eq!(s.allocated_bytes, 8_500);
        assert_eq!(s.pinned_bytes, 2_000);
        assert_eq!(s.warning_threshold_bytes, 8_500);
        assert_eq!(s.pinned_limit_bytes, 4_000);
        assert_eq!(s.pinned_ceiling_bytes(), 4_000);
        assert!(s.is_warning);
        assert!(!s.is_over_quota);
        assert!(s.is_pinned_active);
        assert!((s.allocation_ratio - 0.85).abs() < 1e-6);
        assert!((s.pinned_ratio - 0.20).abs() < 1e-6);
        assert_eq!(s.flags, DDSF_FLAG_QUOTA_WARNING | DDSF_FLAG_PINNED_ACTIVE);
    }
}
