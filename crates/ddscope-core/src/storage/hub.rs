//! Lock-free concurrent global channel directory (`ChannelHub`) and per-channel storage entries.
//!
//! Each data source registers its channels under `<source_id>/<local_path>`.
//! Concurrent `SourceSession` workers append directly to their own [`ChannelEntry`] locks
//! without cross-source lock contention, while UI / `ScopeController` readers query
//! channels and latest snapshots concurrently.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use dashmap::DashMap;
use parking_lot::{Mutex, RwLock};

use crate::scope::ddsf::{DdsfSnapshotEntry, DdsfSnapshotKind, DdsfStateRibbon};
use crate::storage::{
    EventSeries, M4QueryResult, MemoryQuotaManager, MemoryQuotaStats, NativeScalarSeries,
    PinnedGuard, ViewportExtract, WatermarkListener,
};
use crate::types::{
    AxisMeta, ChannelDescriptor, ChannelId, ChannelKind, ChannelPath, ChunkStorageRef, CoreError,
    CoreResult, DataChunk, DecodedBatch, DecodedColumnBuf, EventDataChunk, EventSchemaKind,
    LinearScale, NativeScalarColumn, ScalarSlice, ScalarType, SourceId, TimestampNs,
};

/// Multi-modal per-channel time-series storage container (`Scalar`, `Frame`, or `Event`).
///
/// Includes the `Frame` variant as forward-compatibility hook `H3` for Phase 2 FFT/array streams.
pub enum ChannelStore {
    /// Native-width scalar or 1-bit digital boolean time series with M4 pyramid.
    Scalar(Box<RwLock<NativeScalarSeries>>),
    /// Forward-compatibility hook (`H3`) for Phase 2 1D array/spectrum frame series.
    Frame {
        element_type: ScalarType,
        frame_len: usize,
        axis: AxisMeta,
    },
    /// Discrete state machine transitions, text logs, or struct snapshots.
    Event(Box<RwLock<EventSeries>>),
}

impl std::fmt::Debug for ChannelStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Scalar(series) => {
                let guard = series.read();
                f.debug_struct("ChannelStore::Scalar")
                    .field("channel_id", &guard.channel_id())
                    .field("scalar_type", &guard.scalar_type())
                    .field("total_samples", &guard.total_samples())
                    .field("sealed_chunks", &guard.sealed_chunk_count())
                    .finish()
            }
            Self::Frame {
                element_type,
                frame_len,
                axis,
            } => f
                .debug_struct("ChannelStore::Frame")
                .field("element_type", element_type)
                .field("frame_len", frame_len)
                .field("axis", axis)
                .finish(),
            Self::Event(series) => {
                let guard = series.read();
                f.debug_struct("ChannelStore::Event")
                    .field("channel_id", &guard.channel_id())
                    .field("total_samples", &guard.total_samples())
                    .field("sealed_chunks", &guard.sealed_chunk_count())
                    .finish()
            }
        }
    }
}

/// Registered channel entry inside [`ChannelHub`], pairing metadata, lock-isolated series storage,
/// and lock-free atomic latest-value snapshots.
#[derive(Debug)]
pub struct ChannelEntry {
    id: ChannelId,
    descriptor: ChannelDescriptor,
    path: ChannelPath,
    store: ChannelStore,
    latest_f64_bits: AtomicU64,
    latest_timestamp_ns: AtomicU64,
    has_latest_value: AtomicBool,
    last_state_code: Mutex<Option<i32>>,
}

impl ChannelEntry {
    fn new(
        id: ChannelId,
        mut descriptor: ChannelDescriptor,
        quota_mgr: &Arc<MemoryQuotaManager>,
        listeners: &[Arc<dyn WatermarkListener>],
    ) -> Self {
        descriptor.id = Some(id);
        let path = descriptor.full_path();
        let store = match &descriptor.kind {
            ChannelKind::Scalar {
                scalar_type,
                calibration,
            } => {
                let mut series = NativeScalarSeries::new(id, *scalar_type, *calibration);
                series.set_quota_manager(Arc::clone(quota_mgr));
                for listener in listeners {
                    series.register_listener(Arc::clone(listener));
                }
                ChannelStore::Scalar(Box::new(RwLock::new(series)))
            }
            ChannelKind::Frame {
                element_type,
                frame_len,
                axis,
            } => ChannelStore::Frame {
                element_type: *element_type,
                frame_len: *frame_len,
                axis: axis.clone(),
            },
            ChannelKind::Event { .. } => {
                let mut series = EventSeries::new(id);
                for listener in listeners {
                    series.register_listener(Arc::clone(listener));
                }
                ChannelStore::Event(Box::new(RwLock::new(series)))
            }
        };

        Self {
            id,
            descriptor,
            path,
            store,
            latest_f64_bits: AtomicU64::new(0.0_f64.to_bits()),
            latest_timestamp_ns: AtomicU64::new(0),
            has_latest_value: AtomicBool::new(false),
            last_state_code: Mutex::new(None),
        }
    }

    /// Returns the assigned numeric [`ChannelId`].
    #[inline]
    pub fn id(&self) -> ChannelId {
        self.id
    }

    /// Returns the static [`ChannelDescriptor`] metadata.
    #[inline]
    pub fn descriptor(&self) -> &ChannelDescriptor {
        &self.descriptor
    }

    /// Returns the canonical `<source_id>/<local_path>` [`ChannelPath`].
    #[inline]
    pub fn path(&self) -> &ChannelPath {
        &self.path
    }

    /// Returns the owning [`SourceId`].
    #[inline]
    pub fn source_id(&self) -> &SourceId {
        &self.descriptor.source_id
    }

    /// Returns a reference to the underlying [`ChannelStore`].
    #[inline]
    pub fn store(&self) -> &ChannelStore {
        &self.store
    }

    /// Returns the most recently committed `(timestamp_ns, value_f64)` without locking the series.
    #[inline]
    pub fn latest_value(&self) -> Option<(TimestampNs, f64)> {
        if !self.has_latest_value.load(Ordering::Acquire) {
            return None;
        }
        let ts = self.latest_timestamp_ns.load(Ordering::Relaxed);
        let val = f64::from_bits(self.latest_f64_bits.load(Ordering::Relaxed));
        Some((ts, val))
    }

    /// Builds a [`DdsfSnapshotEntry`] for the latest value if at least one sample has been committed.
    #[inline]
    pub fn latest_snapshot_entry(&self) -> Option<DdsfSnapshotEntry> {
        let (_ts, val_f64) = self.latest_value()?;
        let kind = match &self.descriptor.kind {
            ChannelKind::Scalar {
                scalar_type: ScalarType::Bool,
                ..
            } => DdsfSnapshotKind::Bool,
            ChannelKind::Scalar { .. } => DdsfSnapshotKind::Scalar,
            ChannelKind::Event { .. } => DdsfSnapshotKind::StateEnum,
            ChannelKind::Frame { .. } => return None,
        };
        Some(DdsfSnapshotEntry {
            channel_num_id: self.id.0,
            kind: kind.as_u8(),
            pad: [0; 3],
            val_f64,
        })
    }

    /// Returns total samples recorded in this channel across sealed chunks and active buffer.
    pub fn total_samples(&self) -> u64 {
        match &self.store {
            ChannelStore::Scalar(lock) => lock.read().total_samples(),
            ChannelStore::Event(lock) => lock.read().total_samples(),
            ChannelStore::Frame { .. } => 0,
        }
    }

    /// Returns the number of sealed chunks currently retained in this channel.
    pub fn sealed_chunk_count(&self) -> usize {
        match &self.store {
            ChannelStore::Scalar(lock) => lock.read().sealed_chunk_count(),
            ChannelStore::Event(lock) => lock.read().sealed_chunk_count(),
            ChannelStore::Frame { .. } => 0,
        }
    }

    /// Returns the overall `(t_start_ns, t_end_ns)` time range spanned by this channel.
    pub fn time_range_ns(&self) -> Option<(TimestampNs, TimestampNs)> {
        match &self.store {
            ChannelStore::Scalar(lock) => lock.read().time_range_ns(),
            ChannelStore::Event(lock) => lock.read().time_range_ns(),
            ChannelStore::Frame { .. } => None,
        }
    }

    /// Executes a read closure against the underlying [`NativeScalarSeries`], if this is a scalar channel.
    pub fn with_scalar_read<R>(&self, f: impl FnOnce(&NativeScalarSeries) -> R) -> Option<R> {
        match &self.store {
            ChannelStore::Scalar(lock) => Some(f(&lock.read())),
            _ => None,
        }
    }

    /// Executes a write closure against the underlying [`NativeScalarSeries`], if this is a scalar channel.
    pub fn with_scalar_write<R>(&self, f: impl FnOnce(&mut NativeScalarSeries) -> R) -> Option<R> {
        match &self.store {
            ChannelStore::Scalar(lock) => Some(f(&mut lock.write())),
            _ => None,
        }
    }

    /// Executes a read closure against the underlying [`EventSeries`], if this is an event channel.
    pub fn with_event_read<R>(&self, f: impl FnOnce(&EventSeries) -> R) -> Option<R> {
        match &self.store {
            ChannelStore::Event(lock) => Some(f(&lock.read())),
            _ => None,
        }
    }

    /// Executes a write closure against the underlying [`EventSeries`], if this is an event channel.
    pub fn with_event_write<R>(&self, f: impl FnOnce(&mut EventSeries) -> R) -> Option<R> {
        match &self.store {
            ChannelStore::Event(lock) => Some(f(&mut lock.write())),
            _ => None,
        }
    }

    /// Registers a watermark listener on this channel entry.
    pub fn register_listener(&self, listener: Arc<dyn WatermarkListener>) {
        match &self.store {
            ChannelStore::Scalar(lock) => lock.write().register_listener(listener),
            ChannelStore::Event(lock) => lock.write().register_listener(listener),
            ChannelStore::Frame { .. } => {}
        }
    }

    #[inline]
    fn update_latest(&self, ts_ns: TimestampNs, val_f64: f64) {
        self.latest_timestamp_ns.store(ts_ns, Ordering::Relaxed);
        self.latest_f64_bits
            .store(val_f64.to_bits(), Ordering::Relaxed);
        self.has_latest_value.store(true, Ordering::Release);
    }

    /// Appends a batch of native scalar samples directly to this channel.
    pub fn append_scalar_batch(
        &self,
        ts_ns: &[TimestampNs],
        values: ScalarSlice<'_>,
    ) -> CoreResult<usize> {
        if ts_ns.is_empty() {
            return Ok(0);
        }
        match &self.store {
            ChannelStore::Scalar(lock) => {
                let mut series = lock.write();
                let count = series.append(ts_ns, values)?;
                if let Some((&last_ts, last_val)) =
                    ts_ns.last().zip(last_scalar_as_f64(values, &series.scale()))
                {
                    self.update_latest(last_ts, last_val);
                }
                Ok(count)
            }
            _ => Err(CoreError::Storage(format!(
                "channel `{}` ({}) is not a Scalar channel",
                self.path, self.id
            ))),
        }
    }

    /// Appends explicit state-machine transition events to this channel.
    pub fn append_state_events(
        &self,
        ts_ns: &[TimestampNs],
        states: &[i32],
    ) -> CoreResult<usize> {
        if ts_ns.is_empty() {
            return Ok(0);
        }
        match &self.store {
            ChannelStore::Event(lock) => {
                let count = lock.write().append(ts_ns, states)?;
                if let (Some(&last_ts), Some(&last_state)) = (ts_ns.last(), states.last()) {
                    *self.last_state_code.lock() = Some(last_state);
                    self.update_latest(last_ts, f64::from(last_state));
                }
                Ok(count)
            }
            _ => Err(CoreError::Storage(format!(
                "channel `{}` ({}) is not an Event channel",
                self.path, self.id
            ))),
        }
    }

    /// Appends a column from a [`DecodedBatch`], automatically deduplicating consecutive
    /// identical state codes for `StateEnum` columns into transition events.
    pub fn append_decoded_column(
        &self,
        ts_ns: &[TimestampNs],
        col: &DecodedColumnBuf,
    ) -> CoreResult<usize> {
        if ts_ns.is_empty() {
            return Ok(0);
        }
        match (&self.store, col) {
            (ChannelStore::Scalar(_), DecodedColumnBuf::Bool(v)) => {
                self.append_scalar_batch(ts_ns, ScalarSlice::Bool(v))
            }
            (ChannelStore::Scalar(_), DecodedColumnBuf::U8(v)) => {
                self.append_scalar_batch(ts_ns, ScalarSlice::U8(v))
            }
            (ChannelStore::Scalar(_), DecodedColumnBuf::U16(v)) => {
                self.append_scalar_batch(ts_ns, ScalarSlice::U16(v))
            }
            (ChannelStore::Scalar(_), DecodedColumnBuf::U32(v)) => {
                self.append_scalar_batch(ts_ns, ScalarSlice::U32(v))
            }
            (ChannelStore::Scalar(_), DecodedColumnBuf::U64(v)) => {
                self.append_scalar_batch(ts_ns, ScalarSlice::U64(v))
            }
            (ChannelStore::Scalar(_), DecodedColumnBuf::I8(v)) => {
                self.append_scalar_batch(ts_ns, ScalarSlice::I8(v))
            }
            (ChannelStore::Scalar(_), DecodedColumnBuf::I16(v)) => {
                self.append_scalar_batch(ts_ns, ScalarSlice::I16(v))
            }
            (ChannelStore::Scalar(_), DecodedColumnBuf::I32(v)) => {
                self.append_scalar_batch(ts_ns, ScalarSlice::I32(v))
            }
            (ChannelStore::Scalar(_), DecodedColumnBuf::I64(v)) => {
                self.append_scalar_batch(ts_ns, ScalarSlice::I64(v))
            }
            (ChannelStore::Scalar(_), DecodedColumnBuf::F32(v)) => {
                self.append_scalar_batch(ts_ns, ScalarSlice::F32(v))
            }
            (ChannelStore::Scalar(_), DecodedColumnBuf::F64(v)) => {
                self.append_scalar_batch(ts_ns, ScalarSlice::F64(v))
            }
            (ChannelStore::Event(lock), DecodedColumnBuf::StateEnum(codes)) => {
                if ts_ns.len() != codes.len() {
                    return Err(CoreError::Storage(format!(
                        "StateEnum column length {} does not match timestamp length {}",
                        codes.len(),
                        ts_ns.len()
                    )));
                }
                let mut ev_ts = Vec::new();
                let mut ev_codes = Vec::new();
                let mut running_state = *self.last_state_code.lock();
                for (&t, &code) in ts_ns.iter().zip(codes.iter()) {
                    if running_state != Some(code) {
                        ev_ts.push(t);
                        ev_codes.push(code);
                        running_state = Some(code);
                    }
                }
                if !ev_ts.is_empty() {
                    lock.write().append(&ev_ts, &ev_codes)?;
                }
                *self.last_state_code.lock() = running_state;
                if let (Some(&last_t), Some(&last_c)) = (ts_ns.last(), codes.last()) {
                    self.update_latest(last_t, f64::from(last_c));
                }
                Ok(codes.len())
            }
            _ => Err(CoreError::Storage(format!(
                "column buffer type mismatch for channel `{}` ({})",
                self.path, self.id
            ))),
        }
    }

    /// Queries pixel-aligned M4 or raw viewport data for a scalar channel.
    pub fn query_m4_viewport(
        &self,
        t_min: TimestampNs,
        t_max: TimestampNs,
        w_px: usize,
    ) -> CoreResult<M4QueryResult> {
        match &self.store {
            ChannelStore::Scalar(lock) => lock.read().query_m4_viewport(t_min, t_max, w_px),
            _ => Err(CoreError::Storage(format!(
                "channel `{}` ({}) does not support M4 viewport queries",
                self.path, self.id
            ))),
        }
    }

    /// Queries pixel-aligned M4 or raw viewport data along with real-time track statistics.
    pub fn query_m4_viewport_with_stats(
        &self,
        t_min: TimestampNs,
        t_max: TimestampNs,
        w_px: usize,
    ) -> CoreResult<ViewportExtract> {
        match &self.store {
            ChannelStore::Scalar(lock) => {
                lock.read().query_m4_viewport_with_stats(t_min, t_max, w_px)
            }
            _ => Err(CoreError::Storage(format!(
                "channel `{}` ({}) does not support M4 viewport queries",
                self.path, self.id
            ))),
        }
    }

    /// Pins sealed chunks overlapping `[t_start_ns, t_end_ns]` for this scalar channel.
    pub fn pin_range(
        &self,
        t_start_ns: TimestampNs,
        t_end_ns: TimestampNs,
    ) -> CoreResult<PinnedGuard> {
        match &self.store {
            ChannelStore::Scalar(lock) => lock.write().pin_range(t_start_ns, t_end_ns),
            _ => Err(CoreError::Storage(format!(
                "channel `{}` ({}) does not support chunk pinning",
                self.path, self.id
            ))),
        }
    }

    /// Extracts normalized digital logic transition edges `(init_level, edge_x_norm)` over `[t_min, t_max]`.
    pub fn extract_digital_edges(
        &self,
        t_min: TimestampNs,
        t_max: TimestampNs,
    ) -> CoreResult<(u8, Vec<f32>)> {
        if t_max <= t_min {
            return Err(CoreError::Storage(
                "t_max must be greater than t_min".into(),
            ));
        }
        let series = match &self.store {
            ChannelStore::Scalar(lock) => lock.read(),
            _ => {
                return Err(CoreError::Storage(format!(
                    "channel `{}` ({}) is not a Scalar/DigitalBool channel",
                    self.path, self.id
                )))
            }
        };
        if series.scalar_type() != ScalarType::Bool {
            return Err(CoreError::Storage(format!(
                "channel `{}` ({}) scalar_type is {:?}, expected Bool",
                self.path,
                self.id,
                series.scalar_type()
            )));
        }

        let span = (t_max - t_min) as f64;
        let mut edges = Vec::new();
        let mut init_level: Option<u8> = None;
        let mut prev_level: Option<bool> = None;

        // Check sample at or immediately preceding t_min for initial ZOH level
        if let Some((idx, ts, val)) = series.get_sample_by_timestamp(t_min) {
            if ts <= t_min {
                let lvl = val >= 0.5;
                init_level = Some(u8::from(lvl));
                prev_level = Some(lvl);
            } else if idx > series.evicted_samples() {
                if let Some((prev_ts, prev_val)) = series.get_sample_by_index(idx - 1) {
                    if prev_ts <= t_min {
                        let lvl = prev_val >= 0.5;
                        init_level = Some(u8::from(lvl));
                        prev_level = Some(lvl);
                    }
                }
            }
        }

        let sealed = series.sealed_chunks();
        let start_chunk = sealed.partition_point(|c| c.t_end_ns <= t_min);
        let end_chunk = sealed.partition_point(|c| c.t_start_ns <= t_max);

        for handle in sealed
            .iter()
            .skip(start_chunk)
            .take(end_chunk.saturating_sub(start_chunk))
        {
            if let ChunkStorageRef::InMemory(arc_chunk) = &handle.storage {
                if let DataChunk::Scalar(sc) = arc_chunk.as_ref() {
                    if let NativeScalarColumn::Bool(bv) = &sc.values {
                        if bv.is_empty() {
                            continue;
                        }
                        let first_gt = ts_partition_point(&sc.timestamps, bv.len(), |t| t <= t_min);
                        let end_le = ts_partition_point(&sc.timestamps, bv.len(), |t| t <= t_max);

                        if first_gt >= end_le {
                            if end_le > 0 {
                                prev_level = bv.get(end_le - 1);
                                if init_level.is_none() {
                                    init_level = prev_level.map(u8::from);
                                }
                            }
                            continue;
                        }

                        if prev_level.is_none() {
                            let lvl0 = bv.get(first_gt).unwrap_or(false);
                            init_level = Some(u8::from(lvl0));
                            for &edge_idx in &bv.edge_indices {
                                let idx = edge_idx as usize;
                                if idx > first_gt && idx < end_le {
                                    if let Some(ts) = sc.timestamps.get_ns(idx) {
                                        edges.push(((ts - t_min) as f64 / span) as f32);
                                    }
                                }
                            }
                        } else if let Some(prev) = prev_level {
                            if first_gt == 0 && bv.get(0) != Some(prev) {
                                if let Some(t0) = sc.timestamps.get_ns(0) {
                                    edges.push(((t0 - t_min) as f64 / span) as f32);
                                }
                            }
                            let min_edge_idx = first_gt.max(1);
                            for &edge_idx in &bv.edge_indices {
                                let idx = edge_idx as usize;
                                if idx >= min_edge_idx && idx < end_le {
                                    if let Some(ts) = sc.timestamps.get_ns(idx) {
                                        edges.push(((ts - t_min) as f64 / span) as f32);
                                    }
                                }
                            }
                        }

                        prev_level = bv.get(end_le - 1);
                    }
                }
            }
        }

        let active = series.active_chunk();
        if !active.is_empty() {
            let act_ts = active.timestamps();
            let first_gt = act_ts.partition_point(|&t| t <= t_min);
            let end_le = act_ts.partition_point(|&t| t <= t_max);
            for i in first_gt..end_le {
                if let Some((ts, val)) = active.get_sample_as_f64(i, &series.scale()) {
                    let lvl = val >= 0.5;
                    if let Some(prev) = prev_level {
                        if lvl != prev {
                            edges.push(((ts - t_min) as f64 / span) as f32);
                        }
                    } else if init_level.is_none() {
                        init_level = Some(u8::from(lvl));
                    }
                    prev_level = Some(lvl);
                }
            }
        }

        Ok((init_level.unwrap_or(0), edges))
    }

    /// Extracts normalized [`DdsfStateRibbon`] segments across `[t_min, t_max]` for an event channel.
    pub fn extract_state_ribbons(
        &self,
        t_min: TimestampNs,
        t_max: TimestampNs,
    ) -> CoreResult<Vec<DdsfStateRibbon>> {
        if t_max <= t_min {
            return Err(CoreError::Storage(
                "t_max must be greater than t_min".into(),
            ));
        }
        let series = match &self.store {
            ChannelStore::Event(lock) => lock.read(),
            _ => {
                return Err(CoreError::Storage(format!(
                    "channel `{}` ({}) is not an Event channel",
                    self.path, self.id
                )))
            }
        };

        if series.is_empty() {
            return Ok(Vec::new());
        }

        let span = (t_max - t_min) as f64;
        let mut transitions: Vec<(TimestampNs, i32)> = Vec::new();

        for handle in series.sealed_chunks() {
            if handle.t_end_ns < t_min || handle.t_start_ns > t_max {
                continue;
            }
            if let Some(arc_chunk) = handle.storage.as_in_memory() {
                if let DataChunk::Event(EventDataChunk::StateTransitions {
                    timestamps_ns,
                    state_codes,
                }) = arc_chunk.as_ref()
                {
                    let ts_slice = timestamps_ns.as_slice();
                    let code_slice = state_codes.as_slice();
                    let s = ts_slice.partition_point(|&t| t <= t_min);
                    let e = ts_slice.partition_point(|&t| t <= t_max);
                    for i in s..e {
                        transitions.push((ts_slice[i], code_slice[i]));
                    }
                }
            }
        }

        if let EventDataChunk::StateTransitions {
            timestamps_ns,
            state_codes,
        } = series.active_chunk()
        {
            let ts_slice = timestamps_ns.as_slice();
            let code_slice = state_codes.as_slice();
            let s = ts_slice.partition_point(|&t| t <= t_min);
            let e = ts_slice.partition_point(|&t| t <= t_max);
            for i in s..e {
                transitions.push((ts_slice[i], code_slice[i]));
            }
        }

        let mut ribbons = Vec::new();
        let mut current_state = series.get_state_at(t_min);
        let mut seg_start_x = 0.0_f32;

        if current_state.is_none() {
            if let Some(&(first_ts, first_code)) = transitions.first() {
                seg_start_x = (((first_ts - t_min) as f64) / span).clamp(0.0, 1.0) as f32;
                current_state = Some(first_code);
            } else {
                return Ok(ribbons);
            }
        }

        for &(ts, code) in &transitions {
            let x = (((ts - t_min) as f64) / span).clamp(0.0, 1.0) as f32;
            if let Some(active_code) = current_state {
                if code != active_code {
                    if x > seg_start_x {
                        ribbons.push(DdsfStateRibbon {
                            x0_norm: seg_start_x,
                            x1_norm: x,
                            state_id: active_code,
                            color_idx: self.color_index_for_state(active_code),
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
                    color_idx: self.color_index_for_state(active_code),
                });
            }
        }

        Ok(ribbons)
    }

    fn color_index_for_state(&self, state_code: i32) -> u32 {
        if let ChannelKind::Event {
            schema: EventSchemaKind::StateMachine { labels },
        } = &self.descriptor.kind
        {
            if let Some(label) = labels.get(&state_code) {
                if let Some(rgba) = label.color_rgba {
                    return rgba;
                }
            }
        }
        state_code.max(0) as u32
    }

    fn release_unpinned_chunks(&self) {
        if let ChannelStore::Scalar(lock) = &self.store {
            let mut series = lock.write();
            while series.evict_oldest().is_some() {}
        }
    }
}

#[inline]
fn last_scalar_as_f64(slice: ScalarSlice<'_>, scale: &LinearScale) -> Option<f64> {
    match slice {
        ScalarSlice::Bool(v) => v.last().map(|&b| if b { 1.0 } else { 0.0 }),
        ScalarSlice::U8(v) => v.last().map(|&x| scale.apply(f64::from(x))),
        ScalarSlice::U16(v) => v.last().map(|&x| scale.apply(f64::from(x))),
        ScalarSlice::U32(v) => v.last().map(|&x| scale.apply(f64::from(x))),
        ScalarSlice::U64(v) => v.last().map(|&x| scale.apply(x as f64)),
        ScalarSlice::I8(v) => v.last().map(|&x| scale.apply(f64::from(x))),
        ScalarSlice::I16(v) => v.last().map(|&x| scale.apply(f64::from(x))),
        ScalarSlice::I32(v) => v.last().map(|&x| scale.apply(f64::from(x))),
        ScalarSlice::I64(v) => v.last().map(|&x| scale.apply(x as f64)),
        ScalarSlice::F32(v) => v.last().map(|&x| scale.apply(f64::from(x))),
        ScalarSlice::F64(v) => v.last().map(|&x| scale.apply(x)),
    }
}

/// Lock-free concurrent global channel directory (`<source_id>/<local_path>`) and memory quota hub.
pub struct ChannelHub {
    channels: DashMap<ChannelId, Arc<ChannelEntry>>,
    paths: DashMap<ChannelPath, ChannelId>,
    registration_lock: Mutex<()>,
    next_channel_id: AtomicU32,
    quota_mgr: Arc<MemoryQuotaManager>,
    watermark_listeners: RwLock<Vec<Arc<dyn WatermarkListener>>>,
}

impl std::fmt::Debug for ChannelHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelHub")
            .field("channel_count", &self.channels.len())
            .field("quota_stats", &self.quota_mgr.stats())
            .finish()
    }
}

impl Default for ChannelHub {
    fn default() -> Self {
        Self::new()
    }
}

impl ChannelHub {
    /// Creates a new [`ChannelHub`] backed by the default `2 GiB` [`MemoryQuotaManager`].
    pub fn new() -> Self {
        Self::with_quota_manager(Arc::new(MemoryQuotaManager::with_default_quota()))
    }

    /// Creates a new [`ChannelHub`] backed by the provided [`MemoryQuotaManager`].
    pub fn with_quota_manager(quota_mgr: Arc<MemoryQuotaManager>) -> Self {
        Self {
            channels: DashMap::new(),
            paths: DashMap::new(),
            registration_lock: Mutex::new(()),
            next_channel_id: AtomicU32::new(1),
            quota_mgr,
            watermark_listeners: RwLock::new(Vec::new()),
        }
    }

    /// Returns a reference to the shared [`MemoryQuotaManager`].
    #[inline]
    pub fn quota_manager(&self) -> &Arc<MemoryQuotaManager> {
        &self.quota_mgr
    }

    /// Returns a snapshot of global memory quota usage and pinning statistics.
    #[inline]
    pub fn quota_stats(&self) -> MemoryQuotaStats {
        self.quota_mgr.stats()
    }

    /// Returns the total number of registered channels across all sources.
    #[inline]
    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }

    /// Registers a global [`WatermarkListener`] notified when channels advance.
    pub fn register_watermark_listener(&self, listener: Arc<dyn WatermarkListener>) {
        self.watermark_listeners.write().push(Arc::clone(&listener));
        for entry in self.channels.iter() {
            entry.value().register_listener(Arc::clone(&listener));
        }
    }

    /// Registers a channel descriptor or returns the existing [`ChannelEntry`] if `<source_id>/<local_path>`
    /// is already registered with a compatible modality.
    pub fn register_or_get(&self, desc: ChannelDescriptor) -> CoreResult<Arc<ChannelEntry>> {
        if desc.source_id.as_str().trim().is_empty() {
            return Err(CoreError::InvalidConfig(
                "ChannelDescriptor source_id must not be empty".into(),
            ));
        }
        if desc.local_path.trim().is_empty() {
            return Err(CoreError::InvalidConfig(
                "ChannelDescriptor local_path must not be empty".into(),
            ));
        }

        let full_path = desc.full_path();
        if let Some(existing_id) = self.paths.get(&full_path).map(|r| *r.value()) {
            if let Some(entry) = self.channels.get(&existing_id) {
                return validate_existing_entry(entry.value(), &desc);
            }
        }

        let _guard = self.registration_lock.lock();
        if let Some(existing_id) = self.paths.get(&full_path).map(|r| *r.value()) {
            if let Some(entry) = self.channels.get(&existing_id) {
                return validate_existing_entry(entry.value(), &desc);
            }
        }

        let id = match desc.id {
            Some(explicit_id) if !self.channels.contains_key(&explicit_id) => {
                let next = explicit_id.0.saturating_add(1);
                self.next_channel_id.fetch_max(next, Ordering::AcqRel);
                explicit_id
            }
            _ => loop {
                let raw = self.next_channel_id.fetch_add(1, Ordering::AcqRel);
                let candidate = ChannelId(raw);
                if !self.channels.contains_key(&candidate) {
                    break candidate;
                }
            },
        };

        let listeners = self.watermark_listeners.read().clone();
        let entry = Arc::new(ChannelEntry::new(id, desc, &self.quota_mgr, &listeners));
        self.channels.insert(id, Arc::clone(&entry));
        self.paths.insert(full_path, id);
        Ok(entry)
    }

    /// Registers a slice of channel descriptors in order and returns their [`ChannelEntry`] handles.
    pub fn register_channels(
        &self,
        descs: &[ChannelDescriptor],
    ) -> CoreResult<Vec<Arc<ChannelEntry>>> {
        let mut entries = Vec::with_capacity(descs.len());
        for desc in descs {
            entries.push(self.register_or_get(desc.clone())?);
        }
        Ok(entries)
    }

    /// Looks up a channel by its numeric [`ChannelId`].
    #[inline]
    pub fn get(&self, id: ChannelId) -> Option<Arc<ChannelEntry>> {
        self.channels.get(&id).map(|r| Arc::clone(r.value()))
    }

    /// Alias for [`get`](Self::get).
    #[inline]
    pub fn get_by_id(&self, id: ChannelId) -> Option<Arc<ChannelEntry>> {
        self.get(id)
    }

    /// Looks up a channel by its canonical [`ChannelPath`].
    #[inline]
    pub fn get_by_path(&self, path: &ChannelPath) -> Option<Arc<ChannelEntry>> {
        let id = *self.paths.get(path)?.value();
        self.get(id)
    }

    /// Looks up a channel by its `<source_id>/<local_path>` string slice.
    #[inline]
    pub fn get_by_path_str(&self, path: &str) -> Option<Arc<ChannelEntry>> {
        self.get_by_path(&ChannelPath(path.to_owned()))
    }

    /// Returns all channels belonging to `source_id`, sorted by [`ChannelId`].
    pub fn channels_for_source(&self, source_id: &SourceId) -> Vec<Arc<ChannelEntry>> {
        let mut list: Vec<Arc<ChannelEntry>> = self
            .channels
            .iter()
            .filter(|r| r.value().source_id() == source_id)
            .map(|r| Arc::clone(r.value()))
            .collect();
        list.sort_by_key(|e| e.id());
        list
    }

    /// Returns all registered channels across all sources, sorted by [`ChannelId`].
    pub fn all_channels(&self) -> Vec<Arc<ChannelEntry>> {
        let mut list: Vec<Arc<ChannelEntry>> = self
            .channels
            .iter()
            .map(|r| Arc::clone(r.value()))
            .collect();
        list.sort_by_key(|e| e.id());
        list
    }

    /// Removes a single channel from the directory and releases its unpinned quota allocations.
    pub fn remove_channel(&self, id: ChannelId) -> Option<Arc<ChannelEntry>> {
        let (_id, entry) = self.channels.remove(&id)?;
        self.paths.remove(entry.path());
        entry.release_unpinned_chunks();
        Some(entry)
    }

    /// Removes all channels belonging to `source_id` and releases their unpinned quota allocations.
    pub fn remove_source_channels(&self, source_id: &SourceId) -> usize {
        let ids: Vec<ChannelId> = self
            .channels
            .iter()
            .filter(|r| r.value().source_id() == source_id)
            .map(|r| *r.key())
            .collect();
        let mut removed = 0;
        for id in ids {
            if self.remove_channel(id).is_some() {
                removed += 1;
            }
        }
        removed
    }

    /// Appends a batch of scalar values to channel `id` and enforces global memory quota.
    pub fn append_scalar_batch(
        &self,
        id: ChannelId,
        ts_ns: &[TimestampNs],
        values: ScalarSlice<'_>,
    ) -> CoreResult<usize> {
        let entry = self
            .get(id)
            .ok_or_else(|| CoreError::ChannelNotFound(id.to_string()))?;
        let count = entry.append_scalar_batch(ts_ns, values)?;
        if self.quota_mgr.is_over_quota() {
            self.enforce_global_quota();
        }
        Ok(count)
    }

    /// Appends a batch of state transitions to channel `id`.
    pub fn append_state_events(
        &self,
        id: ChannelId,
        ts_ns: &[TimestampNs],
        states: &[i32],
    ) -> CoreResult<usize> {
        let entry = self
            .get(id)
            .ok_or_else(|| CoreError::ChannelNotFound(id.to_string()))?;
        entry.append_state_events(ts_ns, states)
    }

    /// High-throughput zero-lookup batch ingestion used by [`SourceSession`](crate::workspace::SourceSession) workers.
    ///
    /// Appends all columns of `batch` directly into pre-resolved `entries` (1-to-1 with `batch.columns`).
    pub fn ingest_decoded_batch(
        &self,
        entries: &[Arc<ChannelEntry>],
        batch: &DecodedBatch,
    ) -> CoreResult<usize> {
        let rows = batch.sample_rows;
        if rows == 0 {
            return Ok(0);
        }
        if batch.timestamps_ns.len() != rows {
            return Err(CoreError::Storage(format!(
                "DecodedBatch timestamps_ns length {} does not match sample_rows {rows}",
                batch.timestamps_ns.len()
            )));
        }
        if entries.len() != batch.columns.len() {
            return Err(CoreError::Storage(format!(
                "channel entry count {} does not match DecodedBatch column count {}",
                entries.len(),
                batch.columns.len()
            )));
        }

        for (entry, col) in entries.iter().zip(batch.columns.iter()) {
            entry.append_decoded_column(&batch.timestamps_ns, col)?;
        }

        if self.quota_mgr.is_over_quota() {
            self.enforce_global_quota();
        }

        Ok(rows)
    }

    /// Pins sealed chunks covering `[t0_ns, t1_ns]` on channel `id`.
    pub fn pin_channel_range(
        &self,
        id: ChannelId,
        t0_ns: TimestampNs,
        t1_ns: TimestampNs,
    ) -> CoreResult<PinnedGuard> {
        let entry = self
            .get(id)
            .ok_or_else(|| CoreError::ChannelNotFound(id.to_string()))?;
        entry.pin_range(t0_ns, t1_ns)
    }

    /// Collects latest value snapshots across all channels that have received at least one sample.
    pub fn collect_latest_snapshots(&self) -> Vec<DdsfSnapshotEntry> {
        let mut entries: Vec<(ChannelId, DdsfSnapshotEntry)> = self
            .channels
            .iter()
            .filter_map(|r| {
                let entry = r.value();
                entry.latest_snapshot_entry().map(|snap| (entry.id(), snap))
            })
            .collect();
        entries.sort_by_key(|(id, _)| *id);
        entries.into_iter().map(|(_, snap)| snap).collect()
    }

    /// Evicts the globally oldest unpinned sealed chunks across all scalar channels
    /// whenever total allocated bytes exceed the quota ceiling.
    ///
    /// If all channels' oldest front chunks are currently pinned while newer unpinned chunks
    /// exist behind them, rolls off the pinned front chunk from the rolling window (retained safely
    /// in memory by its `PinnedGuard`) so newer unpinned chunks can continue evicting.
    pub fn enforce_global_quota(&self) {
        while self.quota_mgr.is_over_quota() {
            let mut oldest_unpinned: Option<(TimestampNs, Arc<ChannelEntry>)> = None;
            let mut oldest_pinned_rollover: Option<(TimestampNs, Arc<ChannelEntry>)> = None;

            for item in self.channels.iter() {
                let entry = item.value();
                if let ChannelStore::Scalar(lock) = &entry.store {
                    let series = lock.read();
                    let sealed = series.sealed_chunks();
                    if let Some(front) = sealed.front() {
                        if let ChunkStorageRef::InMemory(arc_chunk) = &front.storage {
                            let ts = front.t_start_ns;
                            if Arc::strong_count(arc_chunk) == 1 {
                                let is_older = match &oldest_unpinned {
                                    None => true,
                                    Some((best_ts, _)) => ts < *best_ts,
                                };
                                if is_older {
                                    oldest_unpinned = Some((ts, Arc::clone(entry)));
                                }
                            } else if sealed.len() > 1
                                && sealed.iter().any(|h| {
                                    matches!(
                                        &h.storage,
                                        ChunkStorageRef::InMemory(a) if Arc::strong_count(a) == 1
                                    )
                                })
                            {
                                let is_older = match &oldest_pinned_rollover {
                                    None => true,
                                    Some((best_ts, _)) => ts < *best_ts,
                                };
                                if is_older {
                                    oldest_pinned_rollover = Some((ts, Arc::clone(entry)));
                                }
                            }
                        }
                    }
                }
            }

            if let Some((_, target_entry)) = oldest_unpinned {
                let evicted = target_entry
                    .with_scalar_write(|s| s.evict_oldest_unpinned())
                    .flatten();
                if evicted.is_none() {
                    break;
                }
            } else if let Some((_, rollover_entry)) = oldest_pinned_rollover {
                let popped = rollover_entry
                    .with_scalar_write(|s| s.evict_oldest())
                    .flatten();
                if popped.is_none() {
                    break;
                }
            } else {
                break;
            }
        }
    }
}

#[inline]
fn ts_partition_point(
    ts_col: &crate::types::TimestampColumn,
    len: usize,
    pred: impl Fn(TimestampNs) -> bool,
) -> usize {
    let mut left = 0usize;
    let mut right = len;
    while left < right {
        let mid = left + (right - left) / 2;
        if let Some(ts) = ts_col.get_ns(mid) {
            if pred(ts) {
                left = mid + 1;
            } else {
                right = mid;
            }
        } else {
            right = mid;
        }
    }
    left
}

fn validate_existing_entry(
    existing: &Arc<ChannelEntry>,
    requested: &ChannelDescriptor,
) -> CoreResult<Arc<ChannelEntry>> {
    let matches_kind = match (&existing.descriptor.kind, &requested.kind) {
        (
            ChannelKind::Scalar {
                scalar_type: a, ..
            },
            ChannelKind::Scalar {
                scalar_type: b, ..
            },
        ) => a == b,
        (
            ChannelKind::Frame {
                element_type: a,
                frame_len: la,
                ..
            },
            ChannelKind::Frame {
                element_type: b,
                frame_len: lb,
                ..
            },
        ) => a == b && la == lb,
        (ChannelKind::Event { .. }, ChannelKind::Event { .. }) => true,
        _ => false,
    };

    if !matches_kind {
        return Err(CoreError::InvalidConfig(format!(
            "channel `{}` already registered with incompatible kind {:?} (requested {:?})",
            existing.path(),
            existing.descriptor.kind,
            requested.kind
        )));
    }

    Ok(Arc::clone(existing))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ScalarValue, StateLabel, CHUNK_LEN};
    use std::collections::BTreeMap;

    #[test]
    fn register_or_get_is_idempotent_and_rejects_type_conflict() {
        let hub = ChannelHub::new();
        let d1 = ChannelDescriptor::new_scalar("src0", "ia", ScalarType::F32);
        let e1 = hub.register_or_get(d1.clone()).unwrap();
        let e2 = hub.register_or_get(d1).unwrap();
        assert_eq!(e1.id(), e2.id());
        assert_eq!(hub.channel_count(), 1);
        assert_eq!(e1.path().as_str(), "src0/ia");

        let incompatible = ChannelDescriptor::new_scalar("src0", "ia", ScalarType::I16);
        let err = hub.register_or_get(incompatible).unwrap_err();
        assert!(matches!(err, CoreError::InvalidConfig(_)));
    }

    #[test]
    fn latest_snapshots_match_scalar_bool_and_state_enum() {
        let hub = ChannelHub::new();
        let e_f32 = hub
            .register_or_get(ChannelDescriptor::new_scalar("s", "v", ScalarType::F32))
            .unwrap();
        let e_bool = hub
            .register_or_get(ChannelDescriptor::new_scalar("s", "dio", ScalarType::Bool))
            .unwrap();
        let mut labels = BTreeMap::new();
        labels.insert(
            2,
            StateLabel {
                name: "RUN".into(),
                color_rgba: Some(0x11223344),
            },
        );
        let e_ev = hub
            .register_or_get(ChannelDescriptor::new_state_machine("s", "fsm", labels))
            .unwrap();

        assert!(e_f32.latest_snapshot_entry().is_none());
        e_f32
            .append_scalar_batch(&[100, 200], ScalarSlice::F32(&[1.5, -3.25]))
            .unwrap();
        e_bool
            .append_scalar_batch(&[100, 200], ScalarSlice::Bool(&[false, true]))
            .unwrap();
        e_ev.append_decoded_column(&[100, 200], &DecodedColumnBuf::StateEnum(vec![2, 2]))
            .unwrap();

        let snaps = hub.collect_latest_snapshots();
        assert_eq!(snaps.len(), 3);
        assert_eq!(snaps[0].kind, DdsfSnapshotKind::Scalar.as_u8());
        assert!((snaps[0].val_f64 - (-3.25)).abs() < 1e-6);
        assert_eq!(snaps[1].kind, DdsfSnapshotKind::Bool.as_u8());
        assert_eq!(snaps[1].val_f64, 1.0);
        assert_eq!(snaps[2].kind, DdsfSnapshotKind::StateEnum.as_u8());
        assert_eq!(snaps[2].val_f64, 2.0);
    }

    #[test]
    fn digital_edges_cover_zoh_floor_exact_boundary_and_sealed_to_active() {
        let hub = ChannelHub::new();
        let entry = hub
            .register_or_get(ChannelDescriptor::new_scalar("d", "pwm", ScalarType::Bool))
            .unwrap();

        let total = CHUNK_LEN + 100;
        let mut ts = Vec::with_capacity(total);
        let mut vals = Vec::with_capacity(total);
        for i in 0..total {
            ts.push(((i + 1) as u64) * 10);
            vals.push((i % 200) >= 100);
        }
        entry.append_scalar_batch(&ts, ScalarSlice::Bool(&vals)).unwrap();
        assert_eq!(entry.sealed_chunk_count(), 1);

        // Case 1: t_min = 1005 (between ts[99]=1000 false and ts[100]=1010 true)
        // Must resolve ZOH floor at idx 99 (false -> 0) and emit rising edge at 1010.
        let t_max = ((total as u64) + 1) * 10;
        let (zoh_init, zoh_edges) = entry.extract_digital_edges(1005, t_max).unwrap();
        assert_eq!(zoh_init, 0);
        assert!(!zoh_edges.is_empty());
        let expected_first = (5.0 / ((t_max - 1005) as f64)) as f32;
        assert!((zoh_edges[0] - expected_first).abs() < 1e-6);

        // Case 2: Edge at exact t_min = 1010 (idx 100: sample at t_min is true -> init_lvl = 1)
        // First emitted edge must be the falling edge at 2010 (no duplicate 0-width edge at x=0.0).
        let (init_lvl, edges) = entry.extract_digital_edges(1010, t_max).unwrap();
        assert_eq!(init_lvl, 1);
        assert!(!edges.is_empty());
        let expected_fall = (1000.0 / ((t_max - 1010) as f64)) as f32;
        assert!((edges[0] - expected_fall).abs() < 1e-6);
        for w in edges.windows(2) {
            assert!(w[1] >= w[0]);
        }
        assert!(edges.iter().all(|&x| (0.0..=1.0).contains(&x)));

        // Case 3: Mid-plateau start at t_min = 1500 (true before 2010 falling edge)
        let (mid_init, mid_edges) = entry.extract_digital_edges(1500, 3500).unwrap();
        assert_eq!(mid_init, 1);
        assert_eq!(mid_edges.len(), 2); // 2010 and 3010
        assert!((mid_edges[0] - 510.0 / 2000.0).abs() < 1e-5);
        assert!((mid_edges[1] - 1510.0 / 2000.0).abs() < 1e-5);
    }

    #[test]
    fn state_ribbons_deduplicate_consecutive_codes() {
        let hub = ChannelHub::new();
        let e_ev = hub
            .register_or_get(ChannelDescriptor::new_state_machine(
                "s",
                "fsm",
                BTreeMap::new(),
            ))
            .unwrap();
        let entry = e_ev;

        entry
            .append_decoded_column(
                &[100, 200, 300, 400, 500],
                &DecodedColumnBuf::StateEnum(vec![0, 0, 1, 1, 2]),
            )
            .unwrap();

        let ribbons = entry.extract_state_ribbons(150, 550).unwrap();
        assert_eq!(ribbons.len(), 3);
        assert_eq!(ribbons[0].state_id, 0);
        assert!((ribbons[0].x0_norm - 0.0).abs() < 1e-6);
        assert!((ribbons[0].x1_norm - 150.0 / 400.0).abs() < 1e-6);
        assert_eq!(ribbons[1].state_id, 1);
        assert_eq!(ribbons[2].state_id, 2);
        assert!((ribbons[2].x1_norm - 1.0).abs() < 1e-6);
    }

    #[test]
    fn global_quota_rolls_past_pinned_front_chunk() {
        // Quota ~ 3 f32 chunks (each CHUNK_LEN f32 chunk ~ 786 KB + M4)
        let quota = Arc::new(MemoryQuotaManager::new(2_600_000));
        let hub = ChannelHub::with_quota_manager(Arc::clone(&quota));
        let e = hub
            .register_or_get(ChannelDescriptor::new_scalar("s", "ch0", ScalarType::F32))
            .unwrap();

        let mut ts = Vec::with_capacity(CHUNK_LEN);
        let mut vals = vec![1.0_f32; CHUNK_LEN];
        vals[123] = 4242.0;
        for i in 0..CHUNK_LEN {
            ts.push((i as u64) * 10);
        }
        hub.append_scalar_batch(e.id(), &ts, ScalarSlice::F32(&vals))
            .unwrap();

        // Pin chunk 0
        let guard = hub
            .pin_channel_range(e.id(), 0, ((CHUNK_LEN - 1) as u64) * 10)
            .unwrap();
        assert_eq!(guard.chunk_count(), 1);

        // Append 5 more full chunks exceeding quota multiple times
        vals.fill(2.0);
        for c in 1..=5u64 {
            for (i, t) in ts.iter_mut().enumerate() {
                *t = c * (CHUNK_LEN as u64) * 10 + (i as u64) * 10;
            }
            hub.append_scalar_batch(e.id(), &ts, ScalarSlice::F32(&vals))
                .unwrap();
        }

        // Unpinned intermediate chunks must have evicted even behind pinned chunk 0
        assert!(
            e.sealed_chunk_count() <= 4,
            "pinned chunk 0 blocked rollover eviction: sealed_chunks={}",
            e.sealed_chunk_count()
        );
        if let DataChunk::Scalar(sc) = guard.chunks()[0].as_ref() {
            assert_eq!(sc.values.get_scalar_value(123), Some(ScalarValue::F32(4242.0)));
        } else {
            panic!("expected scalar chunk");
        }
        drop(guard);
        assert_eq!(quota.pinned_bytes(), 0);
    }
}
