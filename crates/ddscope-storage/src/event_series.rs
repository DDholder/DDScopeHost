//! High-performance time-series storage container for discrete state transitions and events
//! with automatic chunk sealing, zero-order hold retrieval, and watermark dispatch.

use std::sync::Arc;

use ddscope_scope::ddsf::DdsfStateRibbon;
use crate::{MemoryQuotaManager, PinnedGuard, WatermarkEvent, WatermarkListener};
use ddscope_types::{
    ChannelId, ChunkHandle, ChunkStorageRef, CoreError, DataChunk, EventDataChunk, TimestampNs, CHUNK_LEN,
};

/// High-performance time-series storage container for discrete state transitions and events.
#[derive(Clone)]
pub struct EventSeries {
    channel_id: ChannelId,
    active_chunk: EventDataChunk,
    sealed_chunks: Vec<ChunkHandle>,
    chunk_offsets: Vec<u64>,
    next_chunk_seq: u64,
    total_sealed: u64,
    evicted_samples: u64,
    total_samples: u64,
    quota_manager: Option<Arc<MemoryQuotaManager>>,
    listeners: Vec<Arc<dyn WatermarkListener>>,
}

impl EventSeries {
    /// Creates an empty event series for `channel_id` with pre-allocated active buffer.
    pub fn new(channel_id: ChannelId) -> Self {
        Self {
            channel_id,
            active_chunk: EventDataChunk::new_state_transitions(CHUNK_LEN),
            sealed_chunks: Vec::new(),
            chunk_offsets: Vec::new(),
            next_chunk_seq: 0,
            total_sealed: 0,
            evicted_samples: 0,
            total_samples: 0,
            quota_manager: None,
            listeners: Vec::new(),
        }
    }

    /// Attaches a shared [`MemoryQuotaManager`] to enforce bounded ring-buffer eviction and pinning.
    pub fn set_quota_manager(&mut self, quota: Arc<MemoryQuotaManager>) {
        self.quota_manager = Some(quota);
    }

    /// Returns the number of oldest samples permanently evicted from sealed history.
    #[inline]
    pub fn evicted_samples(&self) -> u64 {
        self.evicted_samples
    }

    /// Returns the associated channel ID.
    #[inline]
    pub fn channel_id(&self) -> ChannelId {
        self.channel_id
    }

    /// Returns the cumulative number of event samples recorded across sealed history and active buffer.
    #[inline]
    pub fn total_samples(&self) -> u64 {
        self.total_samples
    }

    /// Returns the number of sealed immutable chunks.
    #[inline]
    pub fn sealed_chunk_count(&self) -> usize {
        self.sealed_chunks.len()
    }

    /// Returns `true` if the series contains zero samples.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.total_samples == 0
    }

    /// Returns a reference to the active buffer chunk.
    #[inline]
    pub fn active_chunk(&self) -> &EventDataChunk {
        &self.active_chunk
    }

    /// Returns a slice of sealed chunk handles.
    #[inline]
    pub fn sealed_chunks(&self) -> &[ChunkHandle] {
        &self.sealed_chunks
    }

    /// Returns the first timestamp recorded in the series, or `None` if empty.
    pub fn first_timestamp(&self) -> Option<TimestampNs> {
        if let Some(first_sealed) = self.sealed_chunks.first() {
            Some(first_sealed.t_start_ns)
        } else {
            match &self.active_chunk {
                EventDataChunk::StateTransitions { timestamps_ns, .. } => {
                    timestamps_ns.first().copied()
                }
                _ => None,
            }
        }
    }

    /// Returns the latest timestamp recorded in the series, or `None` if empty.
    pub fn latest_timestamp(&self) -> Option<TimestampNs> {
        if let EventDataChunk::StateTransitions { timestamps_ns, .. } = &self.active_chunk {
            if let Some(&ts) = timestamps_ns.last() {
                return Some(ts);
            }
        }
        self.sealed_chunks.last().map(|c| c.t_end_ns)
    }

    /// Returns the overall time range `(t_start_ns, t_end_ns)` spanned by the series, or `None` if empty.
    pub fn time_range_ns(&self) -> Option<(TimestampNs, TimestampNs)> {
        Some((self.first_timestamp()?, self.latest_timestamp()?))
    }

    /// Registers a subscriber for watermark advancement notifications.
    pub fn register_listener(&mut self, listener: Arc<dyn WatermarkListener>) {
        self.listeners.push(listener);
    }

    /// Validates batch invariants atomically before modifying any state.
    fn validate_batch(
        &self,
        timestamps: &[TimestampNs],
        state_codes: &[i32],
    ) -> Result<(), CoreError> {
        if timestamps.len() != state_codes.len() {
            return Err(CoreError::Storage(format!(
                "EventSeries append length mismatch: timestamps length ({}) != state_codes length ({})",
                timestamps.len(),
                state_codes.len()
            )));
        }

        if timestamps.is_empty() {
            return Ok(());
        }

        // Check continuity against previous batch commit
        if let Some(last_ts) = self.latest_timestamp() {
            if timestamps[0] < last_ts {
                return Err(CoreError::Storage(format!(
                    "non-monotonic event timestamp: {} < {last_ts}",
                    timestamps[0]
                )));
            }
        }

        // Check intra-batch ordering
        for i in 1..timestamps.len() {
            if timestamps[i] < timestamps[i - 1] {
                return Err(CoreError::Storage(format!(
                    "non-monotonic event timestamp in batch at index {i}: {} < {}",
                    timestamps[i],
                    timestamps[i - 1]
                )));
            }
        }

        Ok(())
    }

    /// Appends a batch of state transitions, enforcing monotonic timestamps and automatic chunk sealing.
    pub fn append(
        &mut self,
        timestamps: &[TimestampNs],
        state_codes: &[i32],
    ) -> Result<usize, CoreError> {
        self.validate_batch(timestamps, state_codes)?;

        let batch_len = timestamps.len();
        if batch_len == 0 {
            return Ok(0);
        }

        let mut consumed = 0;
        while consumed < batch_len {
            let active_len = self.active_chunk.len();
            let room_in_active = CHUNK_LEN.saturating_sub(active_len);
            let take = (batch_len - consumed).min(room_in_active);

            let ts_slice = &timestamps[consumed..consumed + take];
            let code_slice = &state_codes[consumed..consumed + take];

            match &mut self.active_chunk {
                EventDataChunk::StateTransitions {
                    timestamps_ns,
                    state_codes: codes,
                } => {
                    timestamps_ns.extend_from_slice(ts_slice);
                    codes.extend_from_slice(code_slice);
                }
                _ => unreachable!("active_chunk is always StateTransitions"),
            }

            consumed += take;

            if self.active_chunk.len() >= CHUNK_LEN {
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

    /// Internal chunk sealing logic: moves active buffer to immutable sealed history.
    fn seal_active_internal(&mut self) -> Result<Option<ChunkHandle>, CoreError> {
        if self.active_chunk.is_empty() {
            return Ok(None);
        }

        let (t_start_ns, t_end_ns) = match &self.active_chunk {
            EventDataChunk::StateTransitions { timestamps_ns, .. } => {
                (*timestamps_ns.first().unwrap(), *timestamps_ns.last().unwrap())
            }
            _ => return Ok(None),
        };

        let sample_count = self.active_chunk.len() as u32;
        let sealed_payload = if self.active_chunk.len() >= CHUNK_LEN {
            std::mem::replace(
                &mut self.active_chunk,
                EventDataChunk::new_state_transitions(CHUNK_LEN),
            )
        } else {
            let shrunk = self.active_chunk.clone_shrunk();
            self.active_chunk.clear();
            shrunk
        };

        let chunk_seq = self.next_chunk_seq;
        self.next_chunk_seq += 1;

        let handle = ChunkHandle {
            chunk_seq,
            t_start_ns,
            t_end_ns,
            sample_count,
            storage: ChunkStorageRef::InMemory(Arc::new(DataChunk::Event(sealed_payload))),
        };

        self.chunk_offsets.push(self.total_sealed);
        self.total_sealed += sample_count as u64;
        self.sealed_chunks.push(handle.clone());

        if let Some(quota) = self.quota_manager.clone() {
            quota.record_allocation(handle.storage.resident_bytes());
            while quota.is_over_quota() && self.sealed_chunks.len() > 1 {
                if self.evict_oldest_unpinned().is_none() {
                    break;
                }
            }
        }

        Ok(Some(handle))
    }

    /// Manually seals the active buffer on demand (e.g., stream shutdown or flush)
    /// even if sample count is less than `CHUNK_LEN`.
    pub fn seal_active(&mut self) -> Result<Option<ChunkHandle>, CoreError> {
        self.seal_active_internal()
    }

    /// Evicts the oldest unpinned sealed chunk (`Arc::strong_count == 1`) if any exists.
    pub fn evict_oldest_unpinned(&mut self) -> Option<ChunkHandle> {
        self.evict_oldest_internal(true)
    }

    /// Unconditionally pops the oldest sealed chunk during continuous buffer rollover.
    pub fn evict_oldest(&mut self) -> Option<ChunkHandle> {
        self.evict_oldest_internal(false)
    }

    fn evict_oldest_internal(&mut self, only_if_unpinned: bool) -> Option<ChunkHandle> {
        if self.sealed_chunks.is_empty() {
            return None;
        }
        if only_if_unpinned {
            let oldest = self.sealed_chunks.first()?;
            if let ChunkStorageRef::InMemory(arc) = &oldest.storage {
                if Arc::strong_count(arc) > 1 {
                    return None;
                }
            }
        }

        let handle = self.sealed_chunks.remove(0);
        self.evicted_samples += handle.sample_count as u64;
        if !self.chunk_offsets.is_empty() {
            self.chunk_offsets.remove(0);
        }

        if let ChunkStorageRef::InMemory(arc) = &handle.storage {
            if Arc::strong_count(arc) == 1 {
                if let Some(quota) = &self.quota_manager {
                    quota.record_deallocation(handle.storage.resident_bytes());
                }
            }
        }

        Some(handle)
    }

    /// Pins sealed chunks covering `[t_start_ns, t_end_ns]` (plus the preceding ZOH chunk when needed
    /// to resolve the initial state at `t_start_ns`).
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
                    "cannot pin_range: no MemoryQuotaManager configured for event series".into(),
                ));
            }
        };

        let start_idx = self.sealed_chunks.partition_point(|c| c.t_end_ns < t_start_ns);
        let end_idx = self.sealed_chunks.partition_point(|c| c.t_start_ns <= t_end_ns);
        let include_zoh_pre = start_idx > 0
            && (start_idx == end_idx
                || self
                    .sealed_chunks
                    .get(start_idx)
                    .is_some_and(|c| c.t_start_ns > t_start_ns));
        let eff_start = if include_zoh_pre {
            start_idx - 1
        } else {
            start_idx
        };

        let mut estimated_bytes = 0usize;
        for idx in eff_start..end_idx {
            estimated_bytes += self.sealed_chunks[idx].storage.resident_bytes();
        }

        let mut seal_active_for_pin = false;
        if !self.active_chunk.is_empty() {
            let (act_start, act_end) = match &self.active_chunk {
                EventDataChunk::StateTransitions { timestamps_ns, .. } => (
                    *timestamps_ns.first().unwrap(),
                    *timestamps_ns.last().unwrap(),
                ),
                EventDataChunk::TextRecords { timestamps_ns, .. } => (
                    *timestamps_ns.first().unwrap(),
                    *timestamps_ns.last().unwrap(),
                ),
            };
            if t_end_ns >= act_start && (t_start_ns <= act_end || end_idx == self.sealed_chunks.len()) {
                seal_active_for_pin = true;
                estimated_bytes += self.active_chunk.estimated_sealed_byte_footprint();
            }
        }

        if eff_start >= end_idx && !seal_active_for_pin {
            return Err(CoreError::Storage(format!(
                "no sealed chunks found overlapping [{t_start_ns}, {t_end_ns}]"
            )));
        }

        quota.try_pin(estimated_bytes).map_err(CoreError::from)?;

        if seal_active_for_pin {
            if let Err(err) = self.seal_active_internal() {
                quota.release_pin(estimated_bytes);
                return Err(err);
            }
        }

        let raw_final_start = self.sealed_chunks.partition_point(|c| c.t_end_ns < t_start_ns);
        let final_end = self.sealed_chunks.partition_point(|c| c.t_start_ns <= t_end_ns);
        let include_final_zoh = raw_final_start > 0
            && (raw_final_start == final_end
                || self
                    .sealed_chunks
                    .get(raw_final_start)
                    .is_some_and(|c| c.t_start_ns > t_start_ns));
        let final_start = if include_final_zoh {
            raw_final_start - 1
        } else {
            raw_final_start
        };

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
                        "cannot pin event chunk backed by mmap slice".into(),
                    ));
                }
            }
        }

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

    /// Retrieves the active state code at timestamp `ts_ns` under zero-order hold.
    ///
    /// Returns `None` if `ts_ns` is prior to the first transition recorded in the series,
    /// or if the series contains zero samples.
    pub fn get_state_at(&self, ts_ns: TimestampNs) -> Option<i32> {
        // Step 1: Check active buffer
        if let Some(state) = self.active_chunk.get_state_at(ts_ns) {
            return Some(state);
        }

        // Step 2: Search sealed history
        let p = self.sealed_chunks.partition_point(|c| c.t_start_ns <= ts_ns);
        if p == 0 {
            return None;
        }

        let chunk_handle = &self.sealed_chunks[p - 1];
        if let Some(data_chunk) = chunk_handle.storage.as_in_memory() {
            if let DataChunk::Event(event_chunk) = data_chunk.as_ref() {
                return event_chunk.get_state_at(ts_ns);
            }
        }

        None
    }

    /// Retrieves the transition timestamp and state code at global sample `index`.
    pub fn get_transition_by_index(&self, index: u64) -> Option<(TimestampNs, i32)> {
        if index >= self.total_samples || index < self.evicted_samples {
            return None;
        }

        if index >= self.total_sealed {
            let intra_idx = (index - self.total_sealed) as usize;
            return self.active_chunk.get_transition(intra_idx);
        }

        let p = self.chunk_offsets.partition_point(|&off| off <= index);
        let chunk_idx = if p == 0 { 0 } else { p - 1 };
        let start_idx = self.chunk_offsets[chunk_idx];
        let intra_idx = (index - start_idx) as usize;

        let handle = self.sealed_chunks.get(chunk_idx)?;
        if let Some(data_chunk) = handle.storage.as_in_memory() {
            if let DataChunk::Event(event_chunk) = data_chunk.as_ref() {
                return event_chunk.get_transition(intra_idx);
            }
        }

        None
    }
}

/// Extracts normalized [`DdsfStateRibbon`] segments across `[t_min, t_max]` directly from a
/// chronological slice of sealed event `DataChunk`s (e.g., held by a `PinnedGuard`).
pub fn extract_chunks_state_ribbons<F>(
    chunks: &[Arc<DataChunk>],
    t_min: TimestampNs,
    t_max: TimestampNs,
    color_for_state: F,
) -> Result<Vec<DdsfStateRibbon>, CoreError>
where
    F: FnMut(i32) -> u32,
{
    if t_max <= t_min {
        return Err(CoreError::Storage(
            "t_max must be greater than t_min".into(),
        ));
    }
    if chunks.is_empty() {
        return Ok(Vec::new());
    }

    let mut current_state: Option<i32> = None;
    let mut transitions: Vec<(TimestampNs, i32)> = Vec::new();
    for chunk in chunks {
        let DataChunk::Event(ev) = chunk.as_ref() else {
            return Err(CoreError::Storage(
                "pinned chunk is not an Event chunk".into(),
            ));
        };
        if let Some(st) = ev.get_state_at(t_min) {
            current_state = Some(st);
        }
        collect_event_transitions_in_window(ev, t_min, t_max, &mut transitions);
    }

    Ok(build_state_ribbons_from_transitions(
        current_state,
        &transitions,
        t_min,
        t_max,
        color_for_state,
    ))
}

/// Appends state transitions strictly inside `(t_min, t_max]` from one event chunk.
pub(crate) fn collect_event_transitions_in_window(
    ev: &EventDataChunk,
    t_min: TimestampNs,
    t_max: TimestampNs,
    out: &mut Vec<(TimestampNs, i32)>,
) {
    if let EventDataChunk::StateTransitions {
        timestamps_ns,
        state_codes,
    } = ev
    {
        let ts_slice = timestamps_ns.as_slice();
        let code_slice = state_codes.as_slice();
        let s = ts_slice.partition_point(|&t| t <= t_min);
        let e = ts_slice.partition_point(|&t| t <= t_max);
        for i in s..e {
            out.push((ts_slice[i], code_slice[i]));
        }
    }
}

/// Shared state-ribbon builder converting `(initial_state, in_window_transitions)` into normalized ribbons.
pub(crate) fn build_state_ribbons_from_transitions<F>(
    initial_state: Option<i32>,
    transitions: &[(TimestampNs, i32)],
    t_min: TimestampNs,
    t_max: TimestampNs,
    mut color_for_state: F,
) -> Vec<DdsfStateRibbon>
where
    F: FnMut(i32) -> u32,
{
    let span = (t_max - t_min) as f64;
    let mut ribbons = Vec::new();
    let mut current_state = initial_state;
    let mut seg_start_x = 0.0_f32;

    if current_state.is_none() {
        if let Some(&(first_ts, first_code)) = transitions.first() {
            seg_start_x = (((first_ts - t_min) as f64) / span).clamp(0.0, 1.0) as f32;
            current_state = Some(first_code);
        } else {
            return ribbons;
        }
    }

    for &(ts, code) in transitions {
        let x = (((ts - t_min) as f64) / span).clamp(0.0, 1.0) as f32;
        if let Some(active_code) = current_state {
            if code != active_code {
                if x > seg_start_x {
                    ribbons.push(DdsfStateRibbon {
                        x0_norm: seg_start_x,
                        x1_norm: x,
                        state_id: active_code,
                        color_idx: color_for_state(active_code),
                    });
                }
                seg_start_x = x;
                current_state = Some(code);
            }
        } else {
            seg_start_x = x;
            current_state = Some(code);
        }
    }

    if let Some(active_code) = current_state {
        if 1.0 > seg_start_x || ribbons.is_empty() {
            ribbons.push(DdsfStateRibbon {
                x0_norm: seg_start_x,
                x1_norm: 1.0,
                state_id: active_code,
                color_idx: color_for_state(active_code),
            });
        }
    }

    ribbons
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn test_event_series_basic_append_and_zoh() {
        let mut series = EventSeries::new(ChannelId(10));
        assert!(series.is_empty());
        assert_eq!(series.total_samples(), 0);
        assert_eq!(series.get_state_at(100), None);

        let ts = [1000, 2000, 3000];
        let codes = [1, 2, 3];
        assert_eq!(series.append(&ts, &codes).unwrap(), 3);
        assert_eq!(series.total_samples(), 3);
        assert_eq!(series.first_timestamp(), Some(1000));
        assert_eq!(series.latest_timestamp(), Some(3000));
        assert_eq!(series.time_range_ns(), Some((1000, 3000)));

        assert_eq!(series.get_state_at(500), None);
        assert_eq!(series.get_state_at(1000), Some(1));
        assert_eq!(series.get_state_at(1500), Some(1));
        assert_eq!(series.get_state_at(2000), Some(2));
        assert_eq!(series.get_state_at(2999), Some(2));
        assert_eq!(series.get_state_at(3000), Some(3));
        assert_eq!(series.get_state_at(5000), Some(3));
    }

    #[test]
    fn test_event_series_monotonic_rejection() {
        let mut series = EventSeries::new(ChannelId(1));
        series.append(&[100, 200], &[10, 20]).unwrap();

        // Retrograde in batch
        assert!(series.append(&[300, 250], &[1, 2]).is_err());
        assert_eq!(series.total_samples(), 2); // Unmodified

        // Retrograde across batches
        assert!(series.append(&[150], &[30]).is_err());
        assert_eq!(series.total_samples(), 2); // Unmodified

        // Simultaneous transitions are allowed
        assert!(series.append(&[200, 300], &[25, 30]).is_ok());
        assert_eq!(series.total_samples(), 4);
    }

    #[test]
    fn test_event_series_length_mismatch_and_empty() {
        let mut series = EventSeries::new(ChannelId(1));
        assert!(series.append(&[100, 200], &[1]).is_err());
        assert_eq!(series.append(&[], &[]).unwrap(), 0);
        assert_eq!(series.total_samples(), 0);
    }

    #[test]
    fn test_event_series_explicit_seal_active() {
        let mut series = EventSeries::new(ChannelId(1));
        assert!(series.seal_active().unwrap().is_none());

        series.append(&[100, 200], &[1, 2]).unwrap();
        assert_eq!(series.sealed_chunk_count(), 0);

        let handle = series.seal_active().unwrap().expect("should seal active buffer");
        assert_eq!(handle.sample_count, 2);
        assert_eq!(handle.t_start_ns, 100);
        assert_eq!(handle.t_end_ns, 200);
        assert_eq!(series.sealed_chunk_count(), 1);
        assert_eq!(series.total_samples(), 2);

        // State retrieval works after seal
        assert_eq!(series.get_state_at(150), Some(1));
        assert_eq!(series.get_state_at(250), Some(2));
    }

    #[test]
    fn test_event_series_watermark_integration() {
        let mut series = EventSeries::new(ChannelId(9));
        let listener = TestListener::new();
        series.register_listener(listener.clone());

        series.append(&[], &[]).unwrap();
        assert_eq!(listener.count(), 0);

        series.append(&[100, 200], &[10, 20]).unwrap();
        assert_eq!(listener.count(), 1);
        let ev = listener.last_event().unwrap();
        assert_eq!(ev.channel_id, ChannelId(9));
        assert_eq!(ev.latest_ts_ns, 200);
        assert_eq!(ev.total_samples, 2);
    }

    #[test]
    fn test_event_series_index_lookup() {
        let mut series = EventSeries::new(ChannelId(1));
        series.append(&[10, 20, 30], &[1, 2, 3]).unwrap();
        assert_eq!(series.get_transition_by_index(0), Some((10, 1)));
        assert_eq!(series.get_transition_by_index(1), Some((20, 2)));
        assert_eq!(series.get_transition_by_index(2), Some((30, 3)));
        assert_eq!(series.get_transition_by_index(3), None);

        series.seal_active().unwrap();
        assert_eq!(series.get_transition_by_index(0), Some((10, 1)));
        assert_eq!(series.get_transition_by_index(1), Some((20, 2)));
        assert_eq!(series.get_transition_by_index(2), Some((30, 3)));
        assert_eq!(series.get_transition_by_index(3), None);
    }
}
