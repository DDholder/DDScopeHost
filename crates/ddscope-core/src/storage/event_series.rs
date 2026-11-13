//! High-performance time-series storage container for discrete state transitions and events
//! with automatic chunk sealing, zero-order hold retrieval, and watermark dispatch.

use std::sync::Arc;

use crate::storage::{WatermarkEvent, WatermarkListener};
use crate::types::{
    ChannelId, ChunkHandle, ChunkStorageRef, CoreError, DataChunk, EventDataChunk, TimestampNs, CHUNK_LEN,
};

/// High-performance time-series storage container for discrete state transitions and events.
#[derive(Clone)]
pub struct EventSeries {
    channel_id: ChannelId,
    active_chunk: EventDataChunk,
    sealed_chunks: Vec<ChunkHandle>,
    chunk_offsets: Vec<u64>,
    total_sealed: u64,
    total_samples: u64,
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
            total_sealed: 0,
            total_samples: 0,
            listeners: Vec::new(),
        }
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
        let sealed_payload = std::mem::replace(
            &mut self.active_chunk,
            EventDataChunk::new_state_transitions(CHUNK_LEN),
        );

        let handle = ChunkHandle {
            chunk_seq: self.sealed_chunks.len() as u64,
            t_start_ns,
            t_end_ns,
            sample_count,
            storage: ChunkStorageRef::InMemory(Arc::new(DataChunk::Event(sealed_payload))),
        };

        self.chunk_offsets.push(self.total_sealed);
        self.total_sealed += sample_count as u64;
        self.sealed_chunks.push(handle.clone());
        Ok(Some(handle))
    }

    /// Manually seals the active buffer on demand (e.g., stream shutdown or flush)
    /// even if sample count is less than `CHUNK_LEN`.
    pub fn seal_active(&mut self) -> Result<Option<ChunkHandle>, CoreError> {
        self.seal_active_internal()
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
        if index >= self.total_samples {
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
