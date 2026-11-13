//! High-throughput columnar scalar time series with active chunk buffer,
//! automatic sealing at 65,536 samples, prefix-sum sample indexing,
//! nearest timestamp lookup, and watermark advancement dispatch.

use std::collections::VecDeque;
use std::sync::Arc;

use crate::scope::ddsf::DdsfAnalogPoint;
use crate::storage::m4::{
    compute_track_stats_from_columns, compute_track_stats_from_raw, feed_bucket_into_slots,
    finish_pixel_slots, reduce_64_buckets, reduce_bucket_slice, reduce_column_slice,
    M4QueryResult, MultiLevelM4Pyramid, PixelSlot, ViewportExtract,
};
use crate::storage::quota::{MemoryQuotaManager, PinnedGuard};
use crate::storage::{WatermarkEvent, WatermarkListener};
use crate::types::chunk::MAX_REL_U32_NS;
use crate::types::{
    AlignedVec, ChannelId, ChunkHandle, ChunkM4Levels, ChunkStorageRef, CoreError, DataChunk,
    LinearScale, M4Bucket, NativeScalarColumn, ScalarDataChunk, ScalarSlice, ScalarType,
    TimestampColumn, TimestampNs, CHUNK_LEN, M4_BRANCH_FACTOR,
};

/// Slices a borrowed [`ScalarSlice`] across a sample index range `start..end` with zero copies.
#[inline]
pub fn subslice_scalar<'a>(slice: ScalarSlice<'a>, start: usize, end: usize) -> ScalarSlice<'a> {
    match slice {
        ScalarSlice::Bool(s) => ScalarSlice::Bool(&s[start..end]),
        ScalarSlice::U8(s) => ScalarSlice::U8(&s[start..end]),
        ScalarSlice::U16(s) => ScalarSlice::U16(&s[start..end]),
        ScalarSlice::U32(s) => ScalarSlice::U32(&s[start..end]),
        ScalarSlice::U64(s) => ScalarSlice::U64(&s[start..end]),
        ScalarSlice::I8(s) => ScalarSlice::I8(&s[start..end]),
        ScalarSlice::I16(s) => ScalarSlice::I16(&s[start..end]),
        ScalarSlice::I32(s) => ScalarSlice::I32(&s[start..end]),
        ScalarSlice::I64(s) => ScalarSlice::I64(&s[start..end]),
        ScalarSlice::F32(s) => ScalarSlice::F32(&s[start..end]),
        ScalarSlice::F64(s) => ScalarSlice::F64(&s[start..end]),
    }
}

/// Searches a [`TimestampColumn`] for the exact or nearest timestamp index in O(log N) time.
/// Supports dual encoding (`RelU32` and `AbsU64`) without heap re-allocation.
pub fn search_timestamp_in_column(col: &TimestampColumn, target: TimestampNs) -> Option<usize> {
    let len = col.len();
    if len == 0 {
        return None;
    }
    if len == 1 {
        return Some(0);
    }

    match col {
        TimestampColumn::AbsU64(vec) => {
            let slice = vec.as_slice();
            let idx = slice.partition_point(|&t| t <= target);
            if idx == 0 {
                Some(0)
            } else if idx >= len {
                Some(len - 1)
            } else {
                let d_left = target.saturating_sub(slice[idx - 1]);
                let d_right = slice[idx].saturating_sub(target);
                if d_left <= d_right {
                    Some(idx - 1)
                } else {
                    Some(idx)
                }
            }
        }
        TimestampColumn::RelU32 { base_ts_ns, rel_ns } => {
            if target <= *base_ts_ns {
                return Some(0);
            }
            let delta = target - *base_ts_ns;
            if delta > u32::MAX as u64 {
                return Some(len - 1);
            }
            let target_rel = delta as u32;
            let slice = rel_ns.as_slice();
            let idx = slice.partition_point(|&d| d <= target_rel);
            if idx == 0 {
                Some(0)
            } else if idx >= len {
                Some(len - 1)
            } else {
                let d_left = target_rel.saturating_sub(slice[idx - 1]);
                let d_right = slice[idx].saturating_sub(target_rel);
                if d_left <= d_right {
                    Some(idx - 1)
                } else {
                    Some(idx)
                }
            }
        }
    }
}

/// Mutable pre-allocated active append buffer for a scalar channel.
/// Guaranteed 64-byte aligned and pre-allocated for exactly `CHUNK_LEN` (65,536) samples.
#[derive(Clone, Debug, PartialEq)]
pub struct ActiveScalarChunk {
    scalar_type: ScalarType,
    timestamps: AlignedVec<TimestampNs>,
    values: NativeScalarColumn,
    m4: ChunkM4Levels,
}

impl ActiveScalarChunk {
    /// Creates a new active chunk with pre-allocated capacity for `CHUNK_LEN` (65,536) samples.
    pub fn new(scalar_type: ScalarType) -> Self {
        Self::with_capacity(scalar_type, CHUNK_LEN)
    }

    /// Creates an active chunk with custom sample capacity (always 64-byte aligned).
    pub fn with_capacity(scalar_type: ScalarType, cap: usize) -> Self {
        Self {
            scalar_type,
            timestamps: AlignedVec::with_capacity(cap),
            values: NativeScalarColumn::with_capacity(scalar_type, cap),
            m4: ChunkM4Levels::empty(),
        }
    }

    /// Returns the number of samples in the active buffer.
    #[inline]
    pub fn len(&self) -> usize {
        self.timestamps.len()
    }

    /// Returns `true` if the active buffer holds zero samples.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.timestamps.is_empty()
    }

    /// Returns `true` if the buffer has reached `CHUNK_LEN`.
    #[inline]
    pub fn is_full(&self) -> bool {
        self.len() >= CHUNK_LEN
    }

    /// Returns the remaining sample capacity before sealing is required.
    #[inline]
    pub fn remaining_capacity(&self) -> usize {
        CHUNK_LEN.saturating_sub(self.len())
    }

    /// Returns the configured [`ScalarType`].
    #[inline]
    pub fn scalar_type(&self) -> ScalarType {
        self.scalar_type
    }

    /// Returns a borrowed slice of timestamps.
    #[inline]
    pub fn timestamps(&self) -> &[TimestampNs] {
        self.timestamps.as_slice()
    }

    /// Returns a reference to the values column.
    #[inline]
    pub fn values(&self) -> &NativeScalarColumn {
        &self.values
    }

    /// Returns the first timestamp in the buffer, or `None` if empty.
    #[inline]
    pub fn first_timestamp(&self) -> Option<TimestampNs> {
        self.timestamps.first().copied()
    }

    /// Returns the last timestamp in the buffer, or `None` if empty.
    #[inline]
    pub fn last_timestamp(&self) -> Option<TimestampNs> {
        self.timestamps.last().copied()
    }

    /// Appends a slice of timestamps and scalar values into the active buffer.
    pub fn append(
        &mut self,
        timestamps: &[TimestampNs],
        values: ScalarSlice<'_>,
    ) -> Result<usize, CoreError> {
        if timestamps.len() != values.len() {
            return Err(CoreError::Storage(format!(
                "ActiveScalarChunk slice length mismatch: timestamps length ({}) != values length ({})",
                timestamps.len(),
                values.len()
            )));
        }
        if values.scalar_type() != self.scalar_type {
            return Err(CoreError::Storage(format!(
                "ActiveScalarChunk scalar type mismatch: buffer is {:?}, slice is {:?}",
                self.scalar_type,
                values.scalar_type()
            )));
        }
        self.timestamps.extend_from_slice(timestamps);
        self.values.append_slice(values)?;
        Ok(timestamps.len())
    }

    /// Reads sample at `idx`, returning `(timestamp_ns, calibrated_f64_value)`.
    pub fn get_sample_as_f64(&self, idx: usize, scale: &LinearScale) -> Option<(TimestampNs, f64)> {
        let ts = self.timestamps.get(idx).copied()?;
        let raw = self.values.get_as_f64(idx)?;
        Some((ts, scale.apply(raw)))
    }

    /// Returns a reference to the intra-chunk M4 levels.
    #[inline]
    pub fn m4(&self) -> &ChunkM4Levels {
        &self.m4
    }

    /// Incrementally updates intra-chunk M4 levels for newly completed 64-sample blocks.
    pub fn update_m4(&mut self, scale: &LinearScale) {
        let total_samples = self.len();
        let mut completed_l1 = self.m4.l1.len();

        while (completed_l1 + 1) * M4_BRANCH_FACTOR <= total_samples {
            let start = completed_l1 * M4_BRANCH_FACTOR;
            let end = start + M4_BRANCH_FACTOR;
            let t_first = self.timestamps[start];
            let t_last = self.timestamps[end - 1];
            let bucket = reduce_column_slice(&self.values, start, end, t_first, t_last, scale);
            self.m4.l1.push(bucket);
            completed_l1 += 1;

            if completed_l1 % M4_BRANCH_FACTOR == 0 {
                let child_array: &[M4Bucket; 64] = self.m4.l1[completed_l1 - 64..completed_l1]
                    .try_into()
                    .expect("exact 64-element slice");
                self.m4.l2.push(reduce_64_buckets(child_array));
            }
        }
    }

    /// Estimates the byte footprint this active chunk would occupy once sealed.
    pub fn estimated_sealed_byte_footprint(&self) -> usize {
        let n = self.len();
        if n == 0 {
            return 0;
        }

        let span = self
            .timestamps
            .last()
            .copied()
            .unwrap_or(0)
            .saturating_sub(self.timestamps.first().copied().unwrap_or(0));
        let ts_bytes = if span <= MAX_REL_U32_NS {
            n * std::mem::size_of::<u32>()
        } else {
            n * std::mem::size_of::<u64>()
        };

        let val_bytes = self.values.byte_footprint();

        let mut l1_count = self.m4.l1.len();
        if n % M4_BRANCH_FACTOR != 0 {
            l1_count += 1;
        }
        let full_l2 = l1_count / M4_BRANCH_FACTOR;
        let mut l2_count = full_l2;
        if l1_count % M4_BRANCH_FACTOR != 0 {
            l2_count += 1;
        }
        let m4_bytes = (l1_count + l2_count) * std::mem::size_of::<M4Bucket>();

        ts_bytes + val_bytes + m4_bytes
    }

    /// Consumes or seals the active chunk, converting its data into an immutable sealed `ScalarDataChunk`.
    /// Zero-copy transfer of `values` column and `m4` envelope levels.
    /// If sealing fails, the active chunk's internal data is preserved for rollback.
    pub fn seal(&mut self, calibration: LinearScale) -> Result<ScalarDataChunk, CoreError> {
        self.update_m4(&calibration);

        // Compute tail L1 bucket if there are unaligned samples
        let total_samples = self.len();
        let tail_start = self.m4.l1.len() * M4_BRANCH_FACTOR;
        if tail_start < total_samples {
            let t_first = self.timestamps[tail_start];
            let t_last = self.timestamps[total_samples - 1];
            let tail_bucket = reduce_column_slice(
                &self.values,
                tail_start,
                total_samples,
                t_first,
                t_last,
                &calibration,
            );
            self.m4.l1.push(tail_bucket);
        }

        // Finalize L2 buckets
        let l1_slice = self.m4.l1.as_slice();
        let full_l2 = l1_slice.len() / M4_BRANCH_FACTOR;
        let current_l2 = self.m4.l2.len();
        for j in current_l2..full_l2 {
            let start = j * M4_BRANCH_FACTOR;
            let end = start + M4_BRANCH_FACTOR;
            let child_array: &[M4Bucket; 64] = l1_slice[start..end]
                .try_into()
                .expect("exact 64-element slice");
            self.m4.l2.push(reduce_64_buckets(child_array));
        }
        let tail_l2_start = full_l2 * M4_BRANCH_FACTOR;
        if tail_l2_start < l1_slice.len() && self.m4.l2.len() == full_l2 {
            if let Some(b) = reduce_bucket_slice(&l1_slice[tail_l2_start..]) {
                self.m4.l2.push(b);
            }
        }

        let ts_col = TimestampColumn::from_slice(self.timestamps.as_slice());
        ts_col.validate_monotonic()?;
        if ts_col.len() != self.values.len() {
            return Err(CoreError::Storage(format!(
                "ScalarDataChunk length mismatch: timestamps length ({}) != values length ({})",
                ts_col.len(),
                self.values.len()
            )));
        }
        if ts_col.len() > CHUNK_LEN {
            return Err(CoreError::Storage(format!(
                "ScalarDataChunk length ({}) exceeds CHUNK_LEN ({})",
                ts_col.len(),
                CHUNK_LEN
            )));
        }

        let values = std::mem::replace(
            &mut self.values,
            NativeScalarColumn::with_capacity(self.scalar_type, 0),
        );
        let m4 = std::mem::take(&mut self.m4);
        ScalarDataChunk::new_with_m4(ts_col, values, calibration, m4)
    }

    /// Clears data while retaining pre-allocated 64-byte aligned memory blocks.
    pub fn clear(&mut self) {
        self.timestamps.clear();
        self.values.clear();
        self.m4.clear();
    }
}

/// Columnar storage series managing active buffer and sealed chunk history for a scalar channel.
pub struct NativeScalarSeries {
    channel_id: ChannelId,
    scalar_type: ScalarType,
    scale: LinearScale,
    active: ActiveScalarChunk,
    sealed_chunks: VecDeque<ChunkHandle>,
    chunk_offsets: VecDeque<u64>,
    total_sealed: u64,
    is_uniform_sealed: bool,
    total_samples: u64,
    evicted_samples: u64,
    next_chunk_seq: u64,
    quota_manager: Option<Arc<MemoryQuotaManager>>,
    listeners: Vec<Arc<dyn WatermarkListener>>,
    pyramid: MultiLevelM4Pyramid,
}

impl NativeScalarSeries {
    /// Creates a new series initialized with an empty pre-allocated active buffer.
    pub fn new(channel_id: ChannelId, scalar_type: ScalarType, scale: LinearScale) -> Self {
        Self {
            channel_id,
            scalar_type,
            scale,
            active: ActiveScalarChunk::new(scalar_type),
            sealed_chunks: VecDeque::new(),
            chunk_offsets: VecDeque::new(),
            total_sealed: 0,
            is_uniform_sealed: true,
            total_samples: 0,
            evicted_samples: 0,
            next_chunk_seq: 0,
            quota_manager: None,
            listeners: Vec::new(),
            pyramid: MultiLevelM4Pyramid::new(),
        }
    }

    /// Sets an optional shared MemoryQuotaManager for ring buffer eviction and byte accounting.
    pub fn set_quota_manager(&mut self, quota: Arc<MemoryQuotaManager>) {
        self.quota_manager = Some(quota);
    }

    /// Returns a reference to the configured MemoryQuotaManager, if any.
    #[inline]
    pub fn quota_manager(&self) -> Option<&Arc<MemoryQuotaManager>> {
        self.quota_manager.as_ref()
    }

    /// Returns the cumulative number of samples evicted from memory across rollovers.
    #[inline]
    pub fn evicted_samples(&self) -> u64 {
        self.evicted_samples
    }

    /// Returns the associated channel ID.
    #[inline]
    pub fn channel_id(&self) -> ChannelId {
        self.channel_id
    }

    /// Returns the configured [`ScalarType`].
    #[inline]
    pub fn scalar_type(&self) -> ScalarType {
        self.scalar_type
    }

    /// Returns the linear calibration transform.
    #[inline]
    pub fn scale(&self) -> LinearScale {
        self.scale
    }

    /// Returns the cumulative number of samples recorded across sealed chunks and active buffer.
    #[inline]
    pub fn total_samples(&self) -> u64 {
        self.total_samples
    }

    /// Returns the number of sealed chunks in history.
    #[inline]
    pub fn sealed_chunk_count(&self) -> usize {
        self.sealed_chunks.len()
    }

    /// Returns `true` if the series contains zero samples.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.total_samples == 0
    }

    /// Returns a reference to the active buffer.
    #[inline]
    pub fn active_chunk(&self) -> &ActiveScalarChunk {
        &self.active
    }

    /// Returns a reference to the sealed chunk handles.
    #[inline]
    pub fn sealed_chunks(&self) -> &VecDeque<ChunkHandle> {
        &self.sealed_chunks
    }

    /// Registers a watermark advance listener.
    pub fn register_listener(&mut self, listener: Arc<dyn WatermarkListener>) {
        self.listeners.push(listener);
    }

    /// Returns the first timestamp recorded in the series, or `None` if empty.
    #[inline]
    pub fn first_timestamp(&self) -> Option<TimestampNs> {
        if let Some(first_sealed) = self.sealed_chunks.front() {
            Some(first_sealed.t_start_ns)
        } else {
            self.active.first_timestamp()
        }
    }

    /// Returns the latest timestamp recorded in the series, or `None` if empty.
    #[inline]
    pub fn latest_timestamp(&self) -> Option<TimestampNs> {
        self.active
            .last_timestamp()
            .or_else(|| self.sealed_chunks.back().map(|c| c.t_end_ns))
    }

    /// Returns the overall time range `(t_start_ns, t_end_ns)` spanned by the series, or `None` if empty.
    #[inline]
    pub fn time_range_ns(&self) -> Option<(TimestampNs, TimestampNs)> {
        Some((self.first_timestamp()?, self.latest_timestamp()?))
    }

    /// Validates batch invariants atomically before modifying any state.
    fn validate_batch(
        &self,
        timestamps: &[TimestampNs],
        values: ScalarSlice<'_>,
    ) -> Result<(), CoreError> {
        let batch_len = timestamps.len();
        if batch_len != values.len() {
            return Err(CoreError::Storage(format!(
                "Batch length mismatch: timestamps length ({batch_len}) != values length ({})",
                values.len()
            )));
        }

        if batch_len == 0 {
            return Ok(());
        }

        if values.scalar_type() != self.scalar_type {
            return Err(CoreError::Storage(format!(
                "Scalar type mismatch: series configured for {:?}, but batch is {:?}",
                self.scalar_type,
                values.scalar_type()
            )));
        }

        // Cross-batch continuity: if the series already contains samples, verify timestamps[0] >= last_ts
        if let Some(last_ts) = self.latest_timestamp() {
            if timestamps[0] < last_ts {
                return Err(CoreError::Storage(format!(
                    "non-monotonic timestamp: {} < {last_ts}",
                    timestamps[0]
                )));
            }
        }

        // Intra-batch monotonicity: verify timestamps[i] >= timestamps[i - 1] for all i >= 1
        for i in 1..timestamps.len() {
            if timestamps[i] < timestamps[i - 1] {
                return Err(CoreError::Storage(format!(
                    "non-monotonic timestamp in batch at index {i}: {} < {}",
                    timestamps[i],
                    timestamps[i - 1]
                )));
            }
        }

        Ok(())
    }

    /// Appends a batch of samples, supporting multi-batch boundary splitting & rollover.
    pub fn append(
        &mut self,
        timestamps: &[TimestampNs],
        values: ScalarSlice<'_>,
    ) -> Result<usize, CoreError> {
        self.validate_batch(timestamps, values)?;

        let batch_len = timestamps.len();
        if batch_len == 0 {
            return Ok(0);
        }

        let mut curr_offset = 0;
        while curr_offset < batch_len {
            let remaining_in_batch = batch_len - curr_offset;
            let room_in_active = self.active.remaining_capacity();
            let take_len = std::cmp::min(remaining_in_batch, room_in_active);

            let ts_slice = &timestamps[curr_offset..curr_offset + take_len];
            let val_slice = subslice_scalar(values, curr_offset, curr_offset + take_len);

            self.active.append(ts_slice, val_slice)?;
            self.active.update_m4(&self.scale);
            curr_offset += take_len;

            if self.active.is_full() {
                self.seal_active_internal()?;
            }
        }

        self.total_samples += batch_len as u64;

        // Watermark notification dispatch upon batch commit
        let latest_ts_ns = *timestamps.last().unwrap();
        let event = WatermarkEvent {
            channel_id: self.channel_id,
            latest_ts_ns,
            total_samples: self.total_samples,
        };
        for listener in &self.listeners {
            listener.on_watermark_advanced(&[event]);
        }

        Ok(batch_len)
    }

    /// Internal helper that seals the current active chunk into immutable history.
    fn seal_active_internal(&mut self) -> Result<Option<ChunkHandle>, CoreError> {
        if self.active.is_empty() {
            return Ok(None);
        }

        let mut old_active = std::mem::replace(
            &mut self.active,
            ActiveScalarChunk::new(self.scalar_type),
        );

        let chunk_seq = self.next_chunk_seq;
        self.next_chunk_seq += 1;
        let t_start_ns = old_active.first_timestamp().unwrap();
        let t_end_ns = old_active.last_timestamp().unwrap();
        let sample_count = old_active.len() as u32;

        let scalar_chunk = match old_active.seal(self.scale) {
            Ok(chunk) => chunk,
            Err(err) => {
                // Transactional rollback: restore active buffer before returning Err
                self.active = old_active;
                self.next_chunk_seq -= 1;
                return Err(err);
            }
        };

        if sample_count as usize != CHUNK_LEN {
            self.is_uniform_sealed = false;
        }

        self.pyramid.append_chunk_l2(scalar_chunk.m4.l2.as_slice());

        let arc_data_chunk = Arc::new(DataChunk::Scalar(scalar_chunk));

        let handle = ChunkHandle {
            chunk_seq,
            t_start_ns,
            t_end_ns,
            sample_count,
            storage: ChunkStorageRef::InMemory(arc_data_chunk),
        };

        self.chunk_offsets.push_back(self.total_sealed);
        self.total_sealed += sample_count as u64;
        self.sealed_chunks.push_back(handle.clone());

        let quota = self.quota_manager.clone();
        if let Some(quota) = quota {
            quota.record_allocation(handle.storage.resident_bytes());
            while quota.is_over_quota() && self.sealed_chunks.len() > 1 {
                if self.evict_oldest_unpinned().is_none() {
                    break;
                }
            }
        }

        Ok(Some(handle))
    }

    /// Explicitly seals any unsealed samples currently in the active buffer into an immutable chunk.
    ///
    /// Useful for stream pause, flush, or shutdown.
    /// Returns `Ok(Some(ChunkHandle))` if samples were sealed, or `Ok(None)` if active was empty.
    pub fn seal_active(&mut self) -> Result<Option<ChunkHandle>, CoreError> {
        self.seal_active_internal()
    }

    /// Evicts the oldest unpinned sealed chunk (`Arc::strong_count == 1`) if any exists.
    /// Returns `Some(handle)` if an unpinned chunk was popped, or `None` if the queue is empty
    /// or the oldest chunk is currently pinned.
    pub fn evict_oldest_unpinned(&mut self) -> Option<ChunkHandle> {
        self.evict_oldest_internal(true)
    }

    /// Unconditionally pops the oldest chunk during continuous buffer rollover.
    /// If unpinned, memory is freed immediately. If pinned, the chunk is removed
    /// from the rolling window but safely retained by its PinnedGuard.
    pub fn evict_oldest(&mut self) -> Option<ChunkHandle> {
        self.evict_oldest_internal(false)
    }

    fn evict_oldest_internal(&mut self, only_if_unpinned: bool) -> Option<ChunkHandle> {
        if self.sealed_chunks.is_empty() {
            return None;
        }

        if only_if_unpinned {
            let oldest = self.sealed_chunks.front()?;
            if let ChunkStorageRef::InMemory(arc) = &oldest.storage {
                if Arc::strong_count(arc) > 1 {
                    return None;
                }
            }
        }

        let handle = self.sealed_chunks.pop_front()?;
        self.evicted_samples += handle.sample_count as u64;

        if !self.chunk_offsets.is_empty() {
            self.chunk_offsets.pop_front();
        }

        if let ChunkStorageRef::InMemory(arc) = &handle.storage {
            if Arc::strong_count(arc) == 1 {
                if let Some(quota) = &self.quota_manager {
                    quota.record_deallocation(handle.storage.resident_bytes());
                }
            }
        }

        self.synchronize_pyramid_on_eviction();

        Some(handle)
    }

    fn synchronize_pyramid_on_eviction(&mut self) {
        if self.sealed_chunks.is_empty() {
            self.pyramid.clear();
            return;
        }

        let oldest_ts = self.sealed_chunks.front().unwrap().t_start_ns;

        // 1. Prune all buckets and accumulator entries fully older than oldest_ts
        self.pyramid.prune_older_than(oldest_ts);

        // 2. If the leading L3 bucket partially covers evicted chunks (b.t_first_ns < oldest_ts),
        // recompute it from the remaining retained chunks that overlap its interval.
        if let Some(first_l3) = self.pyramid.l3.first().copied() {
            if first_l3.t_first_ns < oldest_ts {
                let mut l2_buckets = Vec::new();
                for chunk in &self.sealed_chunks {
                    if chunk.t_start_ns > first_l3.t_last_ns {
                        break;
                    }
                    if let ChunkStorageRef::InMemory(arc) = &chunk.storage {
                        if let DataChunk::Scalar(sc) = arc.as_ref() {
                            for b in sc.m4.l2.as_slice() {
                                if b.t_last_ns >= oldest_ts && b.t_first_ns <= first_l3.t_last_ns {
                                    l2_buckets.push(*b);
                                }
                            }
                        }
                    }
                }
                if let Some(recomputed) = reduce_bucket_slice(&l2_buckets) {
                    self.pyramid.l3[0] = recomputed;
                } else {
                    self.pyramid.l3.drain_prefix(1);
                }
            }
        }

        // 3. If the leading L4 bucket partially covers evicted chunks (b.t_first_ns < oldest_ts),
        // recompute it from the remaining L3 buckets that overlap its interval.
        if let Some(first_l4) = self.pyramid.l4.first().copied() {
            if first_l4.t_first_ns < oldest_ts {
                let mut l3_buckets = Vec::new();
                for b in self.pyramid.l3.as_slice() {
                    if b.t_first_ns > first_l4.t_last_ns {
                        break;
                    }
                    if b.t_last_ns >= oldest_ts && b.t_first_ns <= first_l4.t_last_ns {
                        l3_buckets.push(*b);
                    }
                }
                if let Some(recomputed) = reduce_bucket_slice(&l3_buckets) {
                    self.pyramid.l4[0] = recomputed;
                } else {
                    self.pyramid.l4.drain_prefix(1);
                }
            }
        }
    }

    /// Pins all sealed chunks covering `[t_start_ns, t_end_ns]`.
    ///
    /// If the requested window overlaps the unsealed active buffer, seals the active
    /// buffer first so that all samples in the range are immutable and protected by `PinnedGuard`.
    pub fn pin_range(
        &mut self,
        t_start_ns: TimestampNs,
        t_end_ns: TimestampNs,
    ) -> Result<PinnedGuard, CoreError> {
        if t_start_ns > t_end_ns {
            return Err(CoreError::Storage(format!(
                "invalid pin_range: t_start_ns ({t_start_ns}) > t_end_ns ({t_end_ns})"
            )));
        }

        let quota = match &self.quota_manager {
            Some(q) => Arc::clone(q),
            None => {
                return Err(CoreError::Storage(
                    "cannot pin_range: no MemoryQuotaManager configured for series".into(),
                ));
            }
        };

        // Determine which existing sealed chunks overlap the range
        let start_idx = self.sealed_chunks.partition_point(|c| c.t_end_ns < t_start_ns);
        let end_idx = self.sealed_chunks.partition_point(|c| c.t_start_ns <= t_end_ns);
        let mut estimated_bytes = 0usize;
        for idx in start_idx..end_idx {
            estimated_bytes += self.sealed_chunks[idx].storage.resident_bytes();
        }

        // Determine if active buffer intersects the range
        let mut active_intersects = false;
        if !self.active.is_empty() {
            let act_start = self.active.first_timestamp().unwrap();
            let act_end = self.active.last_timestamp().unwrap();
            if t_end_ns >= act_start && t_start_ns <= act_end {
                active_intersects = true;
                estimated_bytes += self.active.estimated_sealed_byte_footprint();
            }
        }

        if start_idx >= end_idx && !active_intersects {
            return Err(CoreError::Storage(format!(
                "no sealed chunks found overlapping [{t_start_ns}, {t_end_ns}]"
            )));
        }

        // Atomically check and reserve estimated pinning headroom BEFORE mutating any state.
        // If try_pin fails, this exits immediately leaving active buffer untouched.
        quota.try_pin(estimated_bytes).map_err(CoreError::from)?;

        // Now seal active buffer if it intersects
        if active_intersects {
            if let Err(err) = self.seal_active_internal() {
                // Roll back pinned quota reservation if seal failed
                quota.release_pin(estimated_bytes);
                return Err(err);
            }
        }

        // Re-query overlapping sealed chunks (which now includes the newly sealed active chunk if applicable)
        let final_start = self.sealed_chunks.partition_point(|c| c.t_end_ns < t_start_ns);
        let final_end = self.sealed_chunks.partition_point(|c| c.t_start_ns <= t_end_ns);

        if final_start >= final_end {
            quota.release_pin(estimated_bytes);
            return Err(CoreError::Storage(format!(
                "no sealed chunks found overlapping [{t_start_ns}, {t_end_ns}]"
            )));
        }

        let mut chunks = Vec::with_capacity(final_end - final_start);
        let mut actual_bytes = 0usize;

        for idx in final_start..final_end {
            let handle = &self.sealed_chunks[idx];
            match &handle.storage {
                ChunkStorageRef::InMemory(arc_chunk) => {
                    chunks.push(Arc::clone(arc_chunk));
                    actual_bytes += arc_chunk.byte_footprint();
                }
                ChunkStorageRef::MmapSlice(_) => {
                    quota.release_pin(estimated_bytes);
                    return Err(CoreError::Storage(
                        "cannot pin chunk backed by mmap slice".into(),
                    ));
                }
            }
        }

        // Adjust reservation if actual_bytes differs from estimated_bytes
        if actual_bytes > estimated_bytes {
            let diff = actual_bytes - estimated_bytes;
            if let Err(err) = quota.try_pin(diff) {
                quota.release_pin(estimated_bytes);
                return Err(CoreError::from(err));
            }
        } else if actual_bytes < estimated_bytes {
            let diff = estimated_bytes - actual_bytes;
            quota.release_pin(diff);
        }

        Ok(PinnedGuard::new(
            self.channel_id,
            chunks,
            actual_bytes,
            quota,
        ))
    }

    /// Global sample retrieval by index `0..total_samples`.
    /// Returns `Some((timestamp_ns, calibrated_f64))` or `None` if index is out of bounds or evicted.
    pub fn get_sample_by_index(&self, index: u64) -> Option<(TimestampNs, f64)> {
        if index >= self.total_samples {
            return None;
        }

        if index < self.evicted_samples {
            return None;
        }

        if index >= self.total_sealed {
            let intra_idx = (index - self.total_sealed) as usize;
            return self.active.get_sample_as_f64(intra_idx, &self.scale);
        }

        let local_idx = index - self.evicted_samples;
        let (chunk_idx, intra_idx) = if self.is_uniform_sealed {
            ((local_idx >> 16) as usize, (local_idx & 0xFFFF) as usize)
        } else {
            let p = self.chunk_offsets.partition_point(|&off| off <= index);
            let c = if p == 0 { 0 } else { p - 1 };
            let start_idx = self.chunk_offsets.get(c).copied().unwrap_or(0);
            (c, (index - start_idx) as usize)
        };

        let handle = self.sealed_chunks.get(chunk_idx)?;
        match &handle.storage {
            ChunkStorageRef::InMemory(data_chunk) => match data_chunk.as_ref() {
                DataChunk::Scalar(scalar_chunk) => scalar_chunk.get_sample_as_f64(intra_idx),
                _ => None,
            },
            ChunkStorageRef::MmapSlice(_) => None,
        }
    }

    #[inline]
    fn get_sample_by_index_with_idx(&self, index: u64) -> Option<(u64, TimestampNs, f64)> {
        let (ts, val) = self.get_sample_by_index(index)?;
        Some((index, ts, val))
    }

    /// Global sample retrieval by timestamp.
    ///
    /// Locates the exact or nearest sample in O(log N) time across sealed chunks and active buffer.
    /// Returns `Some((global_index, timestamp_ns, calibrated_f64))` or `None` if empty.
    pub fn get_sample_by_timestamp(&self, ts_ns: TimestampNs) -> Option<(u64, TimestampNs, f64)> {
        if self.total_samples == 0 {
            return None;
        }

        let has_sealed = !self.sealed_chunks.is_empty();
        let has_active = !self.active.is_empty();

        let first_ts = self.first_timestamp()?;
        let last_ts = self.latest_timestamp()?;

        if ts_ns < first_ts {
            return None;
        }
        if ts_ns > last_ts {
            return None;
        }
        if ts_ns == first_ts {
            return self.get_sample_by_index_with_idx(self.evicted_samples);
        }
        if ts_ns == last_ts {
            return self.get_sample_by_index_with_idx(self.total_samples - 1);
        }

        // Target lies within (first_ts, last_ts)
        if has_active {
            let active_start_ts = self.active.first_timestamp()?;
            if ts_ns >= active_start_ts {
                let slice = self.active.timestamps.as_slice();
                let idx = slice.partition_point(|&t| t <= ts_ns);
                let intra_idx = if idx == 0 {
                    0
                } else if idx >= slice.len() {
                    slice.len() - 1
                } else {
                    let d_left = ts_ns.saturating_sub(slice[idx - 1]);
                    let d_right = slice[idx].saturating_sub(ts_ns);
                    if d_left <= d_right {
                        idx - 1
                    } else {
                        idx
                    }
                };
                let (ts, val) = self.active.get_sample_as_f64(intra_idx, &self.scale)?;
                return Some((self.total_sealed + intra_idx as u64, ts, val));
            }
        }

        if !has_sealed {
            let slice = self.active.timestamps.as_slice();
            let idx = slice.partition_point(|&t| t <= ts_ns);
            let intra_idx = if idx == 0 {
                0
            } else if idx >= slice.len() {
                slice.len() - 1
            } else {
                let d_left = ts_ns.saturating_sub(slice[idx - 1]);
                let d_right = slice[idx].saturating_sub(ts_ns);
                if d_left <= d_right {
                    idx - 1
                } else {
                    idx
                }
            };
            let (ts, val) = self.active.get_sample_as_f64(intra_idx, &self.scale)?;
            return Some((intra_idx as u64, ts, val));
        }

        // Search sealed chunks
        let p = self.sealed_chunks.partition_point(|h| h.t_start_ns <= ts_ns);
        let c = if p == 0 { 0 } else { p - 1 };
        let handle = &self.sealed_chunks[c];

        if ts_ns <= handle.t_end_ns {
            let scalar_chunk = match &handle.storage {
                ChunkStorageRef::InMemory(data_chunk) => match data_chunk.as_ref() {
                    DataChunk::Scalar(sc) => sc,
                    _ => return None,
                },
                ChunkStorageRef::MmapSlice(_) => return None,
            };
            let intra_idx = search_timestamp_in_column(&scalar_chunk.timestamps, ts_ns)?;
            let (ts, val) = scalar_chunk.get_sample_as_f64(intra_idx)?;
            let start_idx = self.chunk_offsets[c];
            Some((start_idx + intra_idx as u64, ts, val))
        } else {
            // ts_ns falls into gap between chunk c and next chunk / active buffer
            let (next_idx, next_ts) = if c + 1 < self.sealed_chunks.len() {
                (self.chunk_offsets[c + 1], self.sealed_chunks[c + 1].t_start_ns)
            } else {
                (self.total_sealed, self.active.first_timestamp()?)
            };

            let prev_idx = self.chunk_offsets[c] + handle.sample_count as u64 - 1;
            let prev_ts = handle.t_end_ns;

            let d_prev = ts_ns.saturating_sub(prev_ts);
            let d_next = next_ts.saturating_sub(ts_ns);

            let chosen_idx = if d_prev <= d_next { prev_idx } else { next_idx };
            self.get_sample_by_index_with_idx(chosen_idx)
        }
    }

    /// Returns a reference to the series-level multi-level M4 pyramid.
    #[inline]
    pub fn pyramid(&self) -> &MultiLevelM4Pyramid {
        &self.pyramid
    }

    /// Extracts pixel-aligned viewport rendering data across `[t_min, t_max]` downsampled to `w_px`.
    ///
    /// - If sample count $N \le 2 W_{\text{px}}$: returns exact raw points (`AnalogRawXY`).
    /// - If sample count $N > 2 W_{\text{px}}$: returns decimated M4 columns (`AnalogM4Column`),
    ///   guaranteeing 100% single-sample peak and valley retention.
    pub fn query_m4_viewport(
        &self,
        t_min: TimestampNs,
        t_max: TimestampNs,
        w_px: usize,
    ) -> Result<M4QueryResult, CoreError> {
        if w_px == 0 {
            return Err(CoreError::Storage(
                "pixel_width must be greater than 0".to_string(),
            ));
        }
        if t_max <= t_min {
            return Err(CoreError::Storage(
                "t_max must be greater than t_min".to_string(),
            ));
        }
        if self.total_samples == 0 {
            return Ok(M4QueryResult::Raw(Vec::new()));
        }
        let (series_start, series_end) = match self.time_range_ns() {
            Some(range) => range,
            None => return Ok(M4QueryResult::Raw(Vec::new())),
        };
        if t_max < series_start || t_min > series_end {
            return Ok(M4QueryResult::Raw(Vec::new()));
        }

        // 1. Efficient binary search for intersecting sealed chunks
        let start_chunk = self.sealed_chunks.partition_point(|c| c.t_end_ns < t_min);
        let end_chunk = self.sealed_chunks.partition_point(|c| c.t_start_ns <= t_max);

        let mut sample_count_in_range = 0usize;
        if end_chunk > start_chunk {
            if end_chunk == start_chunk + 1 {
                let handle = &self.sealed_chunks[start_chunk];
                if handle.t_start_ns >= t_min && handle.t_end_ns <= t_max {
                    sample_count_in_range += handle.sample_count as usize;
                } else if let ChunkStorageRef::InMemory(data_chunk) = &handle.storage {
                    if let DataChunk::Scalar(sc) = data_chunk.as_ref() {
                        sample_count_in_range += count_samples_in_range(&sc.timestamps, t_min, t_max);
                    }
                }
            } else {
                let first_handle = &self.sealed_chunks[start_chunk];
                if first_handle.t_start_ns >= t_min {
                    sample_count_in_range += first_handle.sample_count as usize;
                } else if let ChunkStorageRef::InMemory(data_chunk) = &first_handle.storage {
                    if let DataChunk::Scalar(sc) = data_chunk.as_ref() {
                        sample_count_in_range += count_samples_in_range(&sc.timestamps, t_min, t_max);
                    }
                }

                for c in (start_chunk + 1)..(end_chunk - 1) {
                    sample_count_in_range += self.sealed_chunks[c].sample_count as usize;
                }

                let last_handle = &self.sealed_chunks[end_chunk - 1];
                if last_handle.t_end_ns <= t_max {
                    sample_count_in_range += last_handle.sample_count as usize;
                } else if let ChunkStorageRef::InMemory(data_chunk) = &last_handle.storage {
                    if let DataChunk::Scalar(sc) = data_chunk.as_ref() {
                        sample_count_in_range += count_samples_in_range(&sc.timestamps, t_min, t_max);
                    }
                }
            }
        }

        let mut active_intersects = false;
        if !self.active.is_empty() {
            let act_start = self.active.first_timestamp().unwrap();
            let act_end = self.active.last_timestamp().unwrap();
            if act_end >= t_min && act_start <= t_max {
                active_intersects = true;
                let cnt = count_active_samples_in_range(self.active.timestamps(), t_min, t_max);
                sample_count_in_range += cnt;
            }
        }

        if sample_count_in_range == 0 {
            return Ok(M4QueryResult::Raw(Vec::new()));
        }

        // 2. Adaptive switching: Raw if N <= 2 * w_px, Columns if N > 2 * w_px
        if sample_count_in_range <= 2 * w_px {
            let mut raw_points = Vec::with_capacity(sample_count_in_range);
            let span = (t_max - t_min) as f32;

            for c in start_chunk..end_chunk {
                let handle = &self.sealed_chunks[c];
                if let ChunkStorageRef::InMemory(data_chunk) = &handle.storage {
                    if let DataChunk::Scalar(sc) = data_chunk.as_ref() {
                        extract_raw_points_from_scalar_chunk(
                            sc,
                            t_min,
                            t_max,
                            span,
                            &mut raw_points,
                        );
                    }
                }
            }

            if active_intersects {
                extract_raw_points_from_active_chunk(
                    &self.active,
                    &self.scale,
                    t_min,
                    t_max,
                    span,
                    &mut raw_points,
                );
            }

            return Ok(M4QueryResult::Raw(raw_points));
        }

        // 3. Columns mode: Select pyramid level L
        // Select coarsest level where bucket duration <= dt_pixel (samples_per_pixel >= span)
        let samples_per_pixel = sample_count_in_range / w_px;
        let inv_dt_pixel = (w_px as f64) / ((t_max - t_min) as f64);
        let mut slots = vec![PixelSlot::EMPTY; w_px];

        if samples_per_pixel >= 16_777_216 && !self.pyramid.l4.is_empty() {
            // Level 4
            for b in self.pyramid.l4.as_slice() {
                feed_bucket_into_slots(&mut slots, b, t_min, t_max, inv_dt_pixel, w_px);
            }
            for b in &self.pyramid.accum_l3 {
                feed_bucket_into_slots(&mut slots, b, t_min, t_max, inv_dt_pixel, w_px);
            }
            for b in &self.pyramid.accum_l2 {
                feed_bucket_into_slots(&mut slots, b, t_min, t_max, inv_dt_pixel, w_px);
            }
            if active_intersects {
                feed_active_chunk_buckets(
                    &self.active,
                    &self.scale,
                    2,
                    (t_min, t_max),
                    inv_dt_pixel,
                    w_px,
                    &mut slots,
                );
            }
        } else if samples_per_pixel >= 262_144 && !self.pyramid.l3.is_empty() {
            // Level 3
            for b in self.pyramid.l3.as_slice() {
                feed_bucket_into_slots(&mut slots, b, t_min, t_max, inv_dt_pixel, w_px);
            }
            for b in &self.pyramid.accum_l2 {
                feed_bucket_into_slots(&mut slots, b, t_min, t_max, inv_dt_pixel, w_px);
            }
            if active_intersects {
                feed_active_chunk_buckets(
                    &self.active,
                    &self.scale,
                    2,
                    (t_min, t_max),
                    inv_dt_pixel,
                    w_px,
                    &mut slots,
                );
            }
        } else if samples_per_pixel >= 4_096 {
            // Level 2
            for c in start_chunk..end_chunk {
                let handle = &self.sealed_chunks[c];
                if let ChunkStorageRef::InMemory(data_chunk) = &handle.storage {
                    if let DataChunk::Scalar(sc) = data_chunk.as_ref() {
                        for b in sc.m4.l2.as_slice() {
                            feed_bucket_into_slots(&mut slots, b, t_min, t_max, inv_dt_pixel, w_px);
                        }
                    }
                }
            }
            if active_intersects {
                feed_active_chunk_buckets(
                    &self.active,
                    &self.scale,
                    2,
                    (t_min, t_max),
                    inv_dt_pixel,
                    w_px,
                    &mut slots,
                );
            }
        } else {
            // Level 1
            for c in start_chunk..end_chunk {
                let handle = &self.sealed_chunks[c];
                if let ChunkStorageRef::InMemory(data_chunk) = &handle.storage {
                    if let DataChunk::Scalar(sc) = data_chunk.as_ref() {
                        for b in sc.m4.l1.as_slice() {
                            feed_bucket_into_slots(&mut slots, b, t_min, t_max, inv_dt_pixel, w_px);
                        }
                    }
                }
            }
            if active_intersects {
                feed_active_chunk_buckets(
                    &self.active,
                    &self.scale,
                    1,
                    (t_min, t_max),
                    inv_dt_pixel,
                    w_px,
                    &mut slots,
                );
            }
        }

        Ok(M4QueryResult::Columns(finish_pixel_slots(&mut slots, w_px)))
    }

    /// Viewport query returning both rendered points/columns and real-time statistics.
    pub fn query_m4_viewport_with_stats(
        &self,
        t_min: TimestampNs,
        t_max: TimestampNs,
        w_px: usize,
    ) -> Result<ViewportExtract, CoreError> {
        let result = self.query_m4_viewport(t_min, t_max, w_px)?;
        let stats = match &result {
            M4QueryResult::Raw(pts) => compute_track_stats_from_raw(pts),
            M4QueryResult::Columns(cols) => compute_track_stats_from_columns(cols),
        };
        Ok(ViewportExtract { result, stats })
    }
}

/// Helper counting timestamps within [t_min, t_max] in a TimestampColumn in O(log N).
fn count_samples_in_range(col: &TimestampColumn, t_min: TimestampNs, t_max: TimestampNs) -> usize {
    let len = col.len();
    if len == 0 {
        return 0;
    }
    match col {
        TimestampColumn::RelU32 {
            base_ts_ns,
            rel_ns,
        } => {
            let base = *base_ts_ns;
            let rel_slice = rel_ns.as_slice();
            let start_rel = if t_min <= base {
                0
            } else {
                let diff = t_min - base;
                if diff > u32::MAX as u64 {
                    return 0;
                }
                diff as u32
            };
            let end_rel = if t_max < base {
                return 0;
            } else {
                let diff = t_max - base;
                diff.min(u32::MAX as u64) as u32
            };

            let idx_start = rel_slice.partition_point(|&r| r < start_rel);
            let idx_end = rel_slice.partition_point(|&r| r <= end_rel);
            idx_end.saturating_sub(idx_start)
        }
        TimestampColumn::AbsU64(abs) => {
            let slice = abs.as_slice();
            let idx_start = slice.partition_point(|&a| a < t_min);
            let idx_end = slice.partition_point(|&a| a <= t_max);
            idx_end.saturating_sub(idx_start)
        }
    }
}

/// Helper counting timestamps within [t_min, t_max] in an active timestamp slice in O(log N).
fn count_active_samples_in_range(
    ts_slice: &[TimestampNs],
    t_min: TimestampNs,
    t_max: TimestampNs,
) -> usize {
    if ts_slice.is_empty() {
        return 0;
    }
    let idx_start = ts_slice.partition_point(|&a| a < t_min);
    let idx_end = ts_slice.partition_point(|&a| a <= t_max);
    idx_end.saturating_sub(idx_start)
}

/// Extracts raw points from a sealed ScalarDataChunk within [t_min, t_max].
fn extract_raw_points_from_scalar_chunk(
    sc: &ScalarDataChunk,
    t_min: TimestampNs,
    t_max: TimestampNs,
    span: f32,
    out: &mut Vec<DdsfAnalogPoint>,
) {
    let len = sc.len();
    if len == 0 {
        return;
    }
    let (idx_start, idx_end) = match &sc.timestamps {
        TimestampColumn::RelU32 {
            base_ts_ns,
            rel_ns,
        } => {
            let base = *base_ts_ns;
            let rel_slice = rel_ns.as_slice();
            let start_rel = if t_min <= base {
                0
            } else {
                let diff = t_min - base;
                if diff > u32::MAX as u64 {
                    return;
                }
                diff as u32
            };
            let end_rel = if t_max < base {
                return;
            } else {
                let diff = t_max - base;
                diff.min(u32::MAX as u64) as u32
            };
            (
                rel_slice.partition_point(|&r| r < start_rel),
                rel_slice.partition_point(|&r| r <= end_rel),
            )
        }
        TimestampColumn::AbsU64(abs) => {
            let slice = abs.as_slice();
            (
                slice.partition_point(|&a| a < t_min),
                slice.partition_point(|&a| a <= t_max),
            )
        }
    };

    for i in idx_start..idx_end {
        if let Some((t, val)) = sc.get_sample_as_f64(i) {
            let x_norm = if span > 0.0 {
                (t.saturating_sub(t_min) as f32) / span
            } else {
                0.0
            };
            out.push(DdsfAnalogPoint {
                x_norm,
                y_val: val as f32,
            });
        }
    }
}

/// Extracts raw points from ActiveScalarChunk within [t_min, t_max].
fn extract_raw_points_from_active_chunk(
    active: &ActiveScalarChunk,
    scale: &LinearScale,
    t_min: TimestampNs,
    t_max: TimestampNs,
    span: f32,
    out: &mut Vec<DdsfAnalogPoint>,
) {
    let slice = active.timestamps();
    if slice.is_empty() {
        return;
    }
    let idx_start = slice.partition_point(|&a| a < t_min);
    let idx_end = slice.partition_point(|&a| a <= t_max);

    for i in idx_start..idx_end {
        if let Some((t, val)) = active.get_sample_as_f64(i, scale) {
            let x_norm = if span > 0.0 {
                (t.saturating_sub(t_min) as f32) / span
            } else {
                0.0
            };
            out.push(DdsfAnalogPoint {
                x_norm,
                y_val: val as f32,
            });
        }
    }
}

/// Feeds M4 buckets covering [t_min, t_max] from ActiveScalarChunk directly into pixel slots.
fn feed_active_chunk_buckets(
    active: &ActiveScalarChunk,
    scale: &LinearScale,
    target_level: usize,
    time_range: (TimestampNs, TimestampNs),
    inv_dt_pixel: f64,
    w_px: usize,
    slots: &mut [PixelSlot],
) {
    let (t_min, t_max) = time_range;
    if active.is_empty() {
        return;
    }

    if target_level >= 2 {
        for b in active.m4.l2.as_slice() {
            feed_bucket_into_slots(slots, b, t_min, t_max, inv_dt_pixel, w_px);
        }
        let l1_slice = active.m4.l1.as_slice();
        let l2_covered_l1 = active.m4.l2.len() * M4_BRANCH_FACTOR;
        if l2_covered_l1 < l1_slice.len() {
            for b in &l1_slice[l2_covered_l1..] {
                feed_bucket_into_slots(slots, b, t_min, t_max, inv_dt_pixel, w_px);
            }
        }
        let tail_start = l1_slice.len() * M4_BRANCH_FACTOR;
        if tail_start < active.len() {
            let t_first = active.timestamps[tail_start];
            let t_last = active.timestamps[active.len() - 1];
            if t_last >= t_min && t_first <= t_max {
                let tail_bucket = reduce_column_slice(
                    &active.values,
                    tail_start,
                    active.len(),
                    t_first,
                    t_last,
                    scale,
                );
                feed_bucket_into_slots(slots, &tail_bucket, t_min, t_max, inv_dt_pixel, w_px);
            }
        }
    } else {
        for b in active.m4.l1.as_slice() {
            feed_bucket_into_slots(slots, b, t_min, t_max, inv_dt_pixel, w_px);
        }
        let tail_start = active.m4.l1.len() * M4_BRANCH_FACTOR;
        if tail_start < active.len() {
            let t_first = active.timestamps[tail_start];
            let t_last = active.timestamps[active.len() - 1];
            if t_last >= t_min && t_first <= t_max {
                let tail_bucket = reduce_column_slice(
                    &active.values,
                    tail_start,
                    active.len(),
                    t_first,
                    t_last,
                    scale,
                );
                feed_bucket_into_slots(slots, &tail_bucket, t_min, t_max, inv_dt_pixel, w_px);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::CACHE_LINE_BYTES;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct TestListener {
        events: std::sync::Mutex<Vec<WatermarkEvent>>,
        count: AtomicUsize,
    }

    impl TestListener {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                events: std::sync::Mutex::new(Vec::new()),
                count: AtomicUsize::new(0),
            })
        }

        fn count(&self) -> usize {
            self.count.load(Ordering::SeqCst)
        }

        fn last_event(&self) -> Option<WatermarkEvent> {
            self.events.lock().unwrap().last().copied()
        }
    }

    impl WatermarkListener for TestListener {
        fn on_watermark_advanced(&self, events: &[WatermarkEvent]) {
            self.count.fetch_add(events.len(), Ordering::SeqCst);
            self.events.lock().unwrap().extend_from_slice(events);
        }
    }

    #[test]
    fn test_active_scalar_chunk_alignment_and_capacity() {
        let chunk = ActiveScalarChunk::new(ScalarType::F32);
        assert_eq!(chunk.len(), 0);
        assert_eq!(chunk.remaining_capacity(), CHUNK_LEN);
        assert!(!chunk.is_full());
        assert!(chunk.is_empty());
        assert_eq!((chunk.timestamps.as_ptr() as usize) % CACHE_LINE_BYTES, 0);
    }

    #[test]
    fn test_active_scalar_chunk_append_and_clear() {
        let mut chunk = ActiveScalarChunk::new(ScalarType::I16);
        let ts = [1000, 2000, 3000];
        let vals = [10i16, 20, 30];
        let appended = chunk.append(&ts, ScalarSlice::I16(&vals)).unwrap();
        assert_eq!(appended, 3);
        assert_eq!(chunk.len(), 3);
        assert_eq!(chunk.first_timestamp(), Some(1000));
        assert_eq!(chunk.last_timestamp(), Some(3000));

        let scale = LinearScale::new(2.0, 1.0);
        let (t, v) = chunk.get_sample_as_f64(1, &scale).unwrap();
        assert_eq!(t, 2000);
        assert_eq!(v, 41.0); // 20 * 2 + 1 = 41

        chunk.clear();
        assert_eq!(chunk.len(), 0);
        assert!(chunk.is_empty());
    }

    #[test]
    fn test_active_scalar_chunk_type_and_length_mismatch() {
        let mut chunk = ActiveScalarChunk::new(ScalarType::F32);
        assert!(chunk.append(&[100, 200], ScalarSlice::F32(&[1.0])).is_err());
        assert!(chunk.append(&[100], ScalarSlice::I32(&[42])).is_err());
    }

    #[test]
    fn test_native_scalar_series_all_scalar_types() {
        let types = [
            ScalarType::U8,
            ScalarType::U16,
            ScalarType::U32,
            ScalarType::U64,
            ScalarType::I8,
            ScalarType::I16,
            ScalarType::I32,
            ScalarType::I64,
            ScalarType::F32,
            ScalarType::F64,
            ScalarType::Bool,
        ];

        for (ch, &st) in types.iter().enumerate() {
            let mut series = NativeScalarSeries::new(ChannelId(ch as u32), st, LinearScale::identity());
            let ts = [100, 200];
            match st {
                ScalarType::U8 => series.append(&ts, ScalarSlice::U8(&[1, 2])).unwrap(),
                ScalarType::U16 => series.append(&ts, ScalarSlice::U16(&[1, 2])).unwrap(),
                ScalarType::U32 => series.append(&ts, ScalarSlice::U32(&[1, 2])).unwrap(),
                ScalarType::U64 => series.append(&ts, ScalarSlice::U64(&[1, 2])).unwrap(),
                ScalarType::I8 => series.append(&ts, ScalarSlice::I8(&[1, 2])).unwrap(),
                ScalarType::I16 => series.append(&ts, ScalarSlice::I16(&[1, 2])).unwrap(),
                ScalarType::I32 => series.append(&ts, ScalarSlice::I32(&[1, 2])).unwrap(),
                ScalarType::I64 => series.append(&ts, ScalarSlice::I64(&[1, 2])).unwrap(),
                ScalarType::F32 => series.append(&ts, ScalarSlice::F32(&[1.0, 2.0])).unwrap(),
                ScalarType::F64 => series.append(&ts, ScalarSlice::F64(&[1.0, 2.0])).unwrap(),
                ScalarType::Bool => series.append(&ts, ScalarSlice::Bool(&[true, false])).unwrap(),
            };
            assert_eq!(series.total_samples(), 2);
            assert_eq!(series.get_sample_by_index(0).unwrap().0, 100);
            assert_eq!(series.get_sample_by_index(1).unwrap().0, 200);
        }
    }

    #[test]
    fn test_series_boundary_rollover_and_sealing() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let total = 70_000;
        let ts: Vec<u64> = (0..total).map(|i| i as u64 * 10).collect();
        let vals: Vec<f32> = (0..total).map(|i| i as f32).collect();

        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        assert_eq!(series.sealed_chunk_count(), 1);
        assert_eq!(series.total_samples(), 70_000);

        let (t0, v0) = series.get_sample_by_index(0).unwrap();
        assert_eq!(t0, 0);
        assert_eq!(v0, 0.0);

        let (t_boundary, v_boundary) = series.get_sample_by_index(65_535).unwrap();
        assert_eq!(t_boundary, 65_535 * 10);
        assert_eq!(v_boundary, 65_535.0);

        let (t_active, v_active) = series.get_sample_by_index(65_536).unwrap();
        assert_eq!(t_active, 65_536 * 10);
        assert_eq!(v_active, 65_536.0);
    }

    #[test]
    fn test_series_explicit_seal_active() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::I32, LinearScale::identity());
        assert!(series.seal_active().unwrap().is_none());

        let ts = [100, 200, 300];
        let vals = [10i32, 20, 30];
        series.append(&ts, ScalarSlice::I32(&vals)).unwrap();
        assert_eq!(series.sealed_chunk_count(), 0);

        let handle = series.seal_active().unwrap().expect("should seal active buffer");
        assert_eq!(handle.sample_count, 3);
        assert_eq!(handle.t_start_ns, 100);
        assert_eq!(handle.t_end_ns, 300);
        assert_eq!(series.sealed_chunk_count(), 1);
        assert_eq!(series.total_samples(), 3);

        // Verification by index and timestamp after explicit seal
        let (t, v) = series.get_sample_by_index(1).unwrap();
        assert_eq!(t, 200);
        assert_eq!(v, 20.0);

        let (idx, t, v) = series.get_sample_by_timestamp(200).unwrap();
        assert_eq!(idx, 1);
        assert_eq!(t, 200);
        assert_eq!(v, 20.0);
    }

    #[test]
    fn test_series_watermark_dispatch() {
        let mut series = NativeScalarSeries::new(ChannelId(42), ScalarType::F32, LinearScale::identity());
        let listener = TestListener::new();
        series.register_listener(listener.clone());

        // Zero-length append emits no event
        series.append(&[], ScalarSlice::F32(&[])).unwrap();
        assert_eq!(listener.count(), 0);

        // Normal append emits 1 event
        series.append(&[1000, 2000], ScalarSlice::F32(&[1.0, 2.0])).unwrap();
        assert_eq!(listener.count(), 1);
        let ev = listener.last_event().unwrap();
        assert_eq!(ev.channel_id, ChannelId(42));
        assert_eq!(ev.latest_ts_ns, 2000);
        assert_eq!(ev.total_samples, 2);
    }

    #[test]
    fn test_query_m4_viewport_parameter_validations() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());

        // Empty series returns empty Raw
        let res = series.query_m4_viewport(100, 200, 10).unwrap();
        assert_eq!(res, M4QueryResult::Raw(Vec::new()));

        // w_px == 0 returns error
        assert!(series.query_m4_viewport(100, 200, 0).is_err());

        // t_max <= t_min returns error
        assert!(series.query_m4_viewport(200, 100, 10).is_err());
        assert!(series.query_m4_viewport(200, 200, 10).is_err());

        // Append some data: [100, 200, 300]
        series
            .append(&[100, 200, 300], ScalarSlice::F32(&[1.0, 2.0, 3.0]))
            .unwrap();

        // Query out of range before series
        let res_before = series.query_m4_viewport(10, 50, 10).unwrap();
        assert_eq!(res_before, M4QueryResult::Raw(Vec::new()));

        // Query out of range after series
        let res_after = series.query_m4_viewport(500, 600, 10).unwrap();
        assert_eq!(res_after, M4QueryResult::Raw(Vec::new()));
    }

    #[test]
    fn test_query_m4_viewport_raw_mode_when_n_le_2_w_px() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::new(2.0, 10.0));
        let ts: Vec<u64> = (0..100).map(|i| 1000 + i * 10).collect();
        let vals: Vec<f32> = (0..100).map(|i| i as f32).collect();
        series
            .append(&ts, ScalarSlice::F32(&vals))
            .expect("append ok");

        // 100 points, w_px = 200 => N = 100 <= 2 * 200 = 400 => Raw mode!
        let res = series.query_m4_viewport(1000, 1990, 200).unwrap();
        match res {
            M4QueryResult::Raw(points) => {
                assert_eq!(points.len(), 100);
                assert_eq!(points[0].x_norm, 0.0);
                assert!((points[99].x_norm - 1.0).abs() < 1e-4);
                // First point calibrated: 0 * 2 + 10 = 10.0
                assert_eq!(points[0].y_val, 10.0);
                // Last point calibrated: 99 * 2 + 10 = 208.0
                assert_eq!(points[99].y_val, 208.0);
            }
            M4QueryResult::Columns(_) => panic!("expected Raw points mode"),
        }
    }

    #[test]
    fn test_query_m4_viewport_columns_mode_when_n_gt_2_w_px() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let total_n = 5_000;
        let ts: Vec<u64> = (0..total_n).map(|i| 1000 + (i as u64) * 10).collect();
        let vals: Vec<f32> = (0..total_n).map(|i| (i as f32) * 0.1).collect();
        series
            .append(&ts, ScalarSlice::F32(&vals))
            .expect("append ok");

        // N = 5000, w_px = 2000 => 2 * w_px = 4000 < 5000 => Columns mode!
        let t_end = 1000 + ((total_n - 1) as u64) * 10;
        let res = series.query_m4_viewport(1000, t_end, 2000).unwrap();
        match res {
            M4QueryResult::Columns(cols) => {
                assert_eq!(cols.len(), 2000);
                assert_eq!(cols[0].x_norm, 0.5 / 2000.0);
                assert_eq!(cols[1999].x_norm, 1999.5 / 2000.0);
                assert!(cols[0].v_min <= cols[0].v_max);
                assert!(cols[1999].v_min <= cols[1999].v_max);
            }
            M4QueryResult::Raw(_) => panic!("expected Columns mode"),
        }
    }

    #[test]
    fn test_query_m4_viewport_100_percent_single_sample_peak_and_valley_retention() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());

        // Ingest 2 full chunks (131,072 samples)
        let chunk_size = CHUNK_LEN;
        let mut ts_buf = Vec::with_capacity(chunk_size);
        let mut val_buf = Vec::with_capacity(chunk_size);

        for chunk_idx in 0..2 {
            ts_buf.clear();
            val_buf.clear();
            let base_t = (chunk_idx * chunk_size) as u64 * 10;
            for i in 0..chunk_size {
                let global_idx = chunk_idx * chunk_size + i;
                let t = base_t + (i as u64) * 10;
                let mut val = 0.0_f32;

                // Insert isolated single-point peak at index 50,000
                if global_idx == 50_000 {
                    val = 88_888.0;
                }
                // Insert isolated single-point dip at index 100,000
                if global_idx == 100_000 {
                    val = -77_777.0;
                }

                ts_buf.push(t);
                val_buf.push(val);
            }
            series
                .append(&ts_buf, ScalarSlice::F32(&val_buf))
                .expect("append ok");
        }

        assert_eq!(series.sealed_chunk_count(), 2);
        assert_eq!(series.total_samples(), 131_072);

        let (t_start, t_end) = series.time_range_ns().unwrap();
        // Query downsampled to 2,000 pixels
        let res = series.query_m4_viewport(t_start, t_end, 2000).unwrap();
        match res {
            M4QueryResult::Columns(cols) => {
                assert_eq!(cols.len(), 2000);
                let mut found_peak = false;
                let mut found_valley = false;

                for col in &cols {
                    if (col.v_max - 88_888.0).abs() < 1e-3 {
                        found_peak = true;
                    }
                    if (col.v_min - (-77_777.0)).abs() < 1e-3 {
                        found_valley = true;
                    }
                }

                assert!(
                    found_peak,
                    "Single-sample peak (88,888.0) must be retained with 100% fidelity!"
                );
                assert!(
                    found_valley,
                    "Single-sample valley (-77,777.0) must be retained with 100% fidelity!"
                );
            }
            M4QueryResult::Raw(_) => panic!("expected Columns mode"),
        }
    }

    #[test]
    fn test_query_m4_viewport_active_chunk_tail_retention() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());

        // Ingest 1 sealed chunk (65,536 samples)
        let mut ts_buf = Vec::with_capacity(CHUNK_LEN);
        let mut val_buf = Vec::with_capacity(CHUNK_LEN);
        for i in 0..CHUNK_LEN {
            ts_buf.push((i as u64) * 10);
            val_buf.push(1.0);
        }
        series
            .append(&ts_buf, ScalarSlice::F32(&val_buf))
            .expect("append chunk");
        assert_eq!(series.sealed_chunk_count(), 1);

        // Ingest 100 samples into active unsealed buffer, with a spike at sample 75 (tail!)
        let mut act_ts = Vec::new();
        let mut act_val = Vec::new();
        let base_act_t = (CHUNK_LEN as u64) * 10;
        for i in 0..100 {
            act_ts.push(base_act_t + (i as u64) * 10);
            let v = if i == 75 { 55_555.0 } else { 1.0 };
            act_val.push(v);
        }
        series
            .append(&act_ts, ScalarSlice::F32(&act_val))
            .expect("append active tail");
        assert_eq!(series.sealed_chunk_count(), 1);
        assert_eq!(series.active_chunk().len(), 100);

        // Query downsampled across full range including active tail
        let (t_start, t_end) = series.time_range_ns().unwrap();
        let extract = series
            .query_m4_viewport_with_stats(t_start, t_end, 2000)
            .unwrap();

        match &extract.result {
            M4QueryResult::Columns(cols) => {
                let mut found_tail_peak = false;
                for col in cols {
                    if (col.v_max - 55_555.0).abs() < 1e-3 {
                        found_tail_peak = true;
                    }
                }
                assert!(
                    found_tail_peak,
                    "Active chunk unaligned tail peak must be retained 100%!"
                );
            }
            M4QueryResult::Raw(_) => panic!("expected Columns mode"),
        }

        assert_eq!(extract.stats.max, 55_555.0);
        assert!(extract.stats.vpp >= 55_554.0);
    }

    #[test]
    fn test_multi_level_pyramid_l3_and_l4_accumulation() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());

        // Initially pyramid is empty
        assert_eq!(series.pyramid().l3.len(), 0);
        assert_eq!(series.pyramid().l4.len(), 0);

        // Ingest 4 sealed chunks (each 65,536 samples -> 16 L2 buckets each)
        // 4 * 16 = 64 L2 buckets -> should form exactly 1 L3 bucket!
        let mut ts_buf = Vec::with_capacity(CHUNK_LEN);
        let mut val_buf = Vec::with_capacity(CHUNK_LEN);
        for chunk_idx in 0..4 {
            ts_buf.clear();
            val_buf.clear();
            let base_t = (chunk_idx * CHUNK_LEN) as u64 * 10;
            for i in 0..CHUNK_LEN {
                ts_buf.push(base_t + (i as u64) * 10);
                val_buf.push((i % 100) as f32);
            }
            series
                .append(&ts_buf, ScalarSlice::F32(&val_buf))
                .expect("append ok");
        }

        assert_eq!(series.sealed_chunk_count(), 4);
        assert_eq!(series.pyramid().l3.len(), 1, "4 chunks must form 1 L3 bucket");
        assert_eq!(series.pyramid().l4.len(), 0);
    }

    #[test]
    fn test_series_quota_manager_attachment() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let quota = Arc::new(MemoryQuotaManager::with_default_quota());
        series.set_quota_manager(Arc::clone(&quota));
        assert!(series.quota_manager().is_some());

        // Ingest 1 full chunk
        let ts: Vec<u64> = (0..CHUNK_LEN).map(|i| (i as u64) * 10).collect();
        let val: Vec<f32> = (0..CHUNK_LEN).map(|i| i as f32).collect();
        series.append(&ts, ScalarSlice::F32(&val)).unwrap();

        assert_eq!(series.sealed_chunk_count(), 1);
        assert!(quota.allocated_bytes() > 0);
        assert_eq!(quota.pinned_bytes(), 0);
    }

    #[test]
    fn test_series_fifo_unpinned_eviction_on_quota_overflow() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        // Capacity for roughly 1 chunk (~545KB) + 50KB margin = 600KB
        let quota = Arc::new(MemoryQuotaManager::new(600 * 1024));
        series.set_quota_manager(Arc::clone(&quota));

        // Ingest chunk 0 (65,536 samples)
        let ts0: Vec<u64> = (0..CHUNK_LEN).map(|i| (i as u64) * 10).collect();
        let val0: Vec<f32> = (0..CHUNK_LEN).map(|i| i as f32).collect();
        series.append(&ts0, ScalarSlice::F32(&val0)).unwrap();
        assert_eq!(series.sealed_chunk_count(), 1);
        assert_eq!(series.evicted_samples(), 0);

        // Ingest chunk 1 (65,536 samples) -> causes quota overflow and FIFO eviction of chunk 0
        let base_t = (CHUNK_LEN as u64) * 10;
        let ts1: Vec<u64> = (0..CHUNK_LEN).map(|i| base_t + (i as u64) * 10).collect();
        let val1: Vec<f32> = (0..CHUNK_LEN).map(|i| (i + 1000) as f32).collect();
        series.append(&ts1, ScalarSlice::F32(&val1)).unwrap();

        // Chunk 0 was evicted, chunk 1 remains
        assert_eq!(series.sealed_chunk_count(), 1);
        assert_eq!(series.evicted_samples(), CHUNK_LEN as u64);

        // Global index 0 is evicted
        assert!(series.get_sample_by_index(0).is_none());
        assert!(series.get_sample_by_index((CHUNK_LEN - 1) as u64).is_none());

        // Global index CHUNK_LEN (sample 0 of chunk 1) is present
        let (ts, val) = series.get_sample_by_index(CHUNK_LEN as u64).unwrap();
        assert_eq!(ts, base_t);
        assert_eq!(val, 1000.0);
    }

    #[test]
    fn test_series_eviction_blocks_when_head_is_pinned() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let quota = Arc::new(MemoryQuotaManager::new(10 * 1024 * 1024));
        series.set_quota_manager(Arc::clone(&quota));

        // Ingest chunk 0
        let ts0: Vec<u64> = (0..CHUNK_LEN).map(|i| (i as u64) * 10).collect();
        let val0: Vec<f32> = (0..CHUNK_LEN).map(|i| i as f32).collect();
        series.append(&ts0, ScalarSlice::F32(&val0)).unwrap();

        let t_start = ts0[0];
        let t_end = ts0[CHUNK_LEN - 1];

        // Pin chunk 0
        let guard = series.pin_range(t_start, t_end).unwrap();
        assert_eq!(guard.chunk_count(), 1);
        assert!(quota.pinned_bytes() > 0);

        // Attempting to evict unpinned chunk should return None because head chunk is pinned!
        assert!(series.evict_oldest_unpinned().is_none());
        assert_eq!(series.sealed_chunk_count(), 1);
        assert_eq!(series.evicted_samples(), 0);

        drop(guard);
        assert_eq!(quota.pinned_bytes(), 0);

        // Now that guard is dropped, chunk 0 is unpinned and can be evicted
        assert!(series.evict_oldest_unpinned().is_some());
        assert_eq!(series.sealed_chunk_count(), 0);
        assert_eq!(series.evicted_samples(), CHUNK_LEN as u64);
    }

    #[test]
    fn test_series_pin_range_active_chunk_overlap() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let quota = Arc::new(MemoryQuotaManager::with_default_quota());
        series.set_quota_manager(Arc::clone(&quota));

        // Append 100 samples into active buffer
        let ts: Vec<u64> = (0..100).map(|i| 1_000 + (i as u64) * 10).collect();
        let val: Vec<f32> = (0..100).map(|i| i as f32).collect();
        series.append(&ts, ScalarSlice::F32(&val)).unwrap();
        assert_eq!(series.sealed_chunk_count(), 0);
        assert_eq!(series.active_chunk().len(), 100);

        // Pin window overlapping active buffer
        let guard = series.pin_range(1_000, 1_990).unwrap();
        // Active buffer was sealed into sealed_chunks!
        assert_eq!(series.sealed_chunk_count(), 1);
        assert_eq!(series.active_chunk().len(), 0);
        assert_eq!(guard.chunk_count(), 1);
        assert_eq!(guard.total_samples(), 100);
        assert_eq!(guard.time_range_ns(), Some((1_000, 1_990)));
    }

    #[test]
    fn test_series_pin_range_validation_errors() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());

        // Without quota manager
        assert!(series.pin_range(100, 200).is_err());

        // Attach quota manager
        let quota = Arc::new(MemoryQuotaManager::with_default_quota());
        series.set_quota_manager(quota);

        // Inverted range
        assert!(series.pin_range(200, 100).is_err());

        // Empty series (no chunks)
        assert!(series.pin_range(100, 200).is_err());
    }

    #[test]
    fn test_series_continuous_rollover_retains_pinned_chunks() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let quota = Arc::new(MemoryQuotaManager::new(2 * 1024 * 1024));
        series.set_quota_manager(Arc::clone(&quota));

        // Ingest chunk 0
        let ts0: Vec<u64> = (0..CHUNK_LEN).map(|i| (i as u64) * 10).collect();
        let val0: Vec<f32> = (0..CHUNK_LEN).map(|i| 42.0 + i as f32).collect();
        series.append(&ts0, ScalarSlice::F32(&val0)).unwrap();

        let initial_alloc = quota.allocated_bytes();

        // Pin chunk 0
        let guard = series.pin_range(ts0[0], ts0[CHUNK_LEN - 1]).unwrap();
        assert_eq!(guard.chunk_count(), 1);

        // Unconditionally evict chunk 0 from series (simulating continuous ring rollover)
        let evicted = series.evict_oldest().unwrap();
        assert_eq!(evicted.chunk_seq, 0);
        assert_eq!(series.sealed_chunk_count(), 0);
        assert_eq!(series.evicted_samples(), CHUNK_LEN as u64);

        // Memory in quota was NOT deallocated yet because guard still holds chunk 0!
        assert_eq!(quota.allocated_bytes(), initial_alloc);

        // Pinned chunk data is 100% accessible and valid in guard
        let chunk_ref = &guard.chunks()[0];
        match chunk_ref.as_ref() {
            DataChunk::Scalar(sc) => {
                let (_, v0) = sc.get_sample_as_f64(0).unwrap();
                let (_, v_last) = sc.get_sample_as_f64(CHUNK_LEN - 1).unwrap();
                assert_eq!(v0, 42.0);
                assert_eq!(v_last, 42.0 + (CHUNK_LEN - 1) as f64);
            }
            _ => panic!("Expected ScalarDataChunk"),
        }

        // Drop evicted handle so guard is the sole remaining owner of the chunk
        drop(evicted);

        // Dropping guard frees memory and reconciles deallocation!
        drop(guard);
        assert_eq!(quota.pinned_bytes(), 0);
        assert_eq!(quota.allocated_bytes(), 0);
    }

    #[test]
    fn test_o1_global_sample_indexing_across_evictions() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());

        // Ingest 3 chunks
        for c in 0..3 {
            let base_t = (c * CHUNK_LEN) as u64 * 10;
            let ts: Vec<u64> = (0..CHUNK_LEN).map(|i| base_t + (i as u64) * 10).collect();
            let val: Vec<f32> = (0..CHUNK_LEN).map(|i| (c * 1000 + i) as f32).collect();
            series.append(&ts, ScalarSlice::F32(&val)).unwrap();
        }

        assert_eq!(series.sealed_chunk_count(), 3);
        assert_eq!(series.total_samples(), (3 * CHUNK_LEN) as u64);

        // Evict chunk 0
        series.evict_oldest();
        assert_eq!(series.evicted_samples(), CHUNK_LEN as u64);

        // Index in chunk 0 returns None
        assert!(series.get_sample_by_index(0).is_none());
        assert!(series.get_sample_by_index((CHUNK_LEN - 1) as u64).is_none());

        // Index in chunk 1 resolves in O(1)
        let (ts1, v1) = series.get_sample_by_index(CHUNK_LEN as u64).unwrap();
        assert_eq!(ts1, (CHUNK_LEN as u64) * 10);
        assert_eq!(v1, 1000.0);

        // Index in chunk 2 resolves in O(1)
        let (ts2, v2) = series.get_sample_by_index((2 * CHUNK_LEN) as u64).unwrap();
        assert_eq!(ts2, (2 * CHUNK_LEN as u64) * 10);
        assert_eq!(v2, 2000.0);
    }

    #[test]
    fn test_get_sample_by_timestamp_boundary_post_eviction() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());

        // Ingest 2 chunks
        for c in 0..2 {
            let base_t = 10_000 + (c * CHUNK_LEN) as u64 * 10;
            let ts: Vec<u64> = (0..CHUNK_LEN).map(|i| base_t + (i as u64) * 10).collect();
            let val: Vec<f32> = (0..CHUNK_LEN).map(|i| (c * 1000 + i) as f32).collect();
            series.append(&ts, ScalarSlice::F32(&val)).unwrap();
        }

        // Evict chunk 0
        series.evict_oldest();

        // First timestamp is now chunk 1's start timestamp
        let first_ts = series.first_timestamp().unwrap();
        assert_eq!(first_ts, 10_000 + (CHUNK_LEN as u64) * 10);

        // Timestamp lookup at first_ts must return global index == CHUNK_LEN
        let (idx, ts, val) = series.get_sample_by_timestamp(first_ts).unwrap();
        assert_eq!(idx, CHUNK_LEN as u64);
        assert_eq!(ts, first_ts);
        assert_eq!(val, 1000.0);

        // Looking up earlier timestamp returns None
        assert!(series.get_sample_by_timestamp(10_000).is_none());
    }
}
