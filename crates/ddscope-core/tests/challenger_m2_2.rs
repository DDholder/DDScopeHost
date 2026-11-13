//! Adversarial challenge test suite for Milestone 2 (M2) — Storage Series & Watermark Engine.
//!
//! Stress-tests:
//! 1. Memory Safety & 64-byte alignment across all 11 scalar types
//! 2. Multi-batch boundary splitting & rollover math with prime and huge batch sizes
//! 3. Irregular partial flushes (`seal_active()`) interleaved with auto-sealing & prefix sum indexing
//! 4. Dual-encoding timestamp binary search (`RelU32` vs `AbsU64`), boundaries, and nearest-neighbor tie-breaking
//! 5. EventSeries zero-order hold across inter-chunk gaps, simultaneous timestamps, and monotonic rejection
//! 6. Watermark listener fanout stress and zero-sample invariants

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use ddscope_core::storage::scalar_series::search_timestamp_in_column;
use ddscope_core::storage::{ActiveScalarChunk, EventSeries, NativeScalarSeries, WatermarkEvent, WatermarkListener};
use ddscope_core::types::chunk::MAX_REL_U32_NS;
use ddscope_core::types::{
    ChannelId, LinearScale, NativeScalarColumn, ScalarSlice, ScalarType,
    TimestampColumn, TimestampNs, CACHE_LINE_BYTES, CHUNK_LEN,
};

struct RecordingListener {
    events: Mutex<Vec<WatermarkEvent>>,
    call_count: AtomicUsize,
}

impl RecordingListener {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            events: Mutex::new(Vec::new()),
            call_count: AtomicUsize::new(0),
        })
    }

    fn call_count(&self) -> usize {
        self.call_count.load(Ordering::SeqCst)
    }

    fn events(&self) -> Vec<WatermarkEvent> {
        self.events.lock().unwrap().clone()
    }
}

impl WatermarkListener for RecordingListener {
    fn on_watermark_advanced(&self, events: &[WatermarkEvent]) {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        self.events.lock().unwrap().extend_from_slice(events);
    }
}

// =========================================================================
// 1. Memory Safety & 64-Byte Cache Line Alignment Across All Types
// =========================================================================

#[test]
fn test_active_scalar_chunk_cache_line_alignment_all_types() {
    let types = [
        ScalarType::Bool,
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
    ];

    for &st in &types {
        let chunk = ActiveScalarChunk::new(st);
        assert_eq!(chunk.len(), 0);
        assert_eq!(chunk.remaining_capacity(), CHUNK_LEN);
        assert!(chunk.is_empty());
        assert!(!chunk.is_full());
        assert_eq!(chunk.scalar_type(), st);

        // Verify timestamps buffer pointer alignment
        let ts_ptr = chunk.timestamps().as_ptr() as usize;
        assert_eq!(
            ts_ptr % CACHE_LINE_BYTES,
            0,
            "ActiveScalarChunk timestamps for {st:?} not 64-byte aligned: {ts_ptr:#x}"
        );

        // Verify values column buffer pointer alignment
        match chunk.values() {
            NativeScalarColumn::Bool(b) => {
                let p = b.words.as_ptr() as usize;
                assert_eq!(p % CACHE_LINE_BYTES, 0, "Bool words not aligned");
            }
            NativeScalarColumn::U8(v) => assert_eq!(v.as_ptr() as usize % CACHE_LINE_BYTES, 0),
            NativeScalarColumn::U16(v) => assert_eq!(v.as_ptr() as usize % CACHE_LINE_BYTES, 0),
            NativeScalarColumn::U32(v) => assert_eq!(v.as_ptr() as usize % CACHE_LINE_BYTES, 0),
            NativeScalarColumn::U64(v) => assert_eq!(v.as_ptr() as usize % CACHE_LINE_BYTES, 0),
            NativeScalarColumn::I8(v) => assert_eq!(v.as_ptr() as usize % CACHE_LINE_BYTES, 0),
            NativeScalarColumn::I16(v) => assert_eq!(v.as_ptr() as usize % CACHE_LINE_BYTES, 0),
            NativeScalarColumn::I32(v) => assert_eq!(v.as_ptr() as usize % CACHE_LINE_BYTES, 0),
            NativeScalarColumn::I64(v) => assert_eq!(v.as_ptr() as usize % CACHE_LINE_BYTES, 0),
            NativeScalarColumn::F32(v) => assert_eq!(v.as_ptr() as usize % CACHE_LINE_BYTES, 0),
            NativeScalarColumn::F64(v) => assert_eq!(v.as_ptr() as usize % CACHE_LINE_BYTES, 0),
        }
    }
}

// =========================================================================
// 2. Multi-Batch Boundary Splitting & Rollover Math
// =========================================================================

#[test]
fn test_multibatch_prime_size_rollover_continuous() {
    let mut series = NativeScalarSeries::new(
        ChannelId(101),
        ScalarType::F32,
        LinearScale::new(1.0, 0.0),
    );

    // Sequence of prime and non-power-of-two batch sizes:
    let batch_sizes = [13, 97, 1009, 65521, 65537, 131071];
    let total_expected: usize = batch_sizes.iter().sum(); // 263,248
    let mut current_sample = 0usize;

    for &bsize in &batch_sizes {
        let ts: Vec<u64> = (current_sample..current_sample + bsize)
            .map(|i| 1000 + (i as u64) * 10)
            .collect();
        let vals: Vec<f32> = (current_sample..current_sample + bsize)
            .map(|i| i as f32)
            .collect();

        let appended = series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        assert_eq!(appended, bsize);
        current_sample += bsize;
    }

    assert_eq!(series.total_samples(), total_expected as u64);
    let expected_sealed = total_expected / CHUNK_LEN; // 263248 / 65536 = 4
    let expected_active = total_expected % CHUNK_LEN; // 1104
    assert_eq!(series.sealed_chunk_count(), expected_sealed);
    assert_eq!(series.active_chunk().len(), expected_active);

    // Verify sample-accurate retrieval across all chunk boundaries
    // Test sample 0
    let (t0, v0) = series.get_sample_by_index(0).unwrap();
    assert_eq!(t0, 1000);
    assert_eq!(v0, 0.0);

    // Test sample 65535 (last sample of sealed chunk 0)
    let (t_end0, v_end0) = series.get_sample_by_index(65535).unwrap();
    assert_eq!(t_end0, 1000 + 65535 * 10);
    assert_eq!(v_end0, 65535.0);

    // Test sample 65536 (first sample of sealed chunk 1)
    let (t_start1, v_start1) = series.get_sample_by_index(65536).unwrap();
    assert_eq!(t_start1, 1000 + 65536 * 10);
    assert_eq!(v_start1, 65536.0);

    // Test sample 131071 (last of chunk 1) and 131072 (start of chunk 2)
    assert_eq!(series.get_sample_by_index(131071).unwrap().1, 131071.0);
    assert_eq!(series.get_sample_by_index(131072).unwrap().1, 131072.0);

    // Test sample 196607 (last of chunk 2) and 196608 (start of chunk 3)
    assert_eq!(series.get_sample_by_index(196607).unwrap().1, 196607.0);
    assert_eq!(series.get_sample_by_index(196608).unwrap().1, 196608.0);

    // Test sample 262143 (last of chunk 3) and 262144 (first of active buffer)
    assert_eq!(series.get_sample_by_index(262143).unwrap().1, 262143.0);
    assert_eq!(series.get_sample_by_index(262144).unwrap().1, 262144.0);

    // Test last sample in active buffer
    let last_idx = (total_expected - 1) as u64;
    let (t_last, v_last) = series.get_sample_by_index(last_idx).unwrap();
    assert_eq!(t_last, 1000 + last_idx * 10);
    assert_eq!(v_last, last_idx as f64);

    // Test out of bounds index
    assert_eq!(series.get_sample_by_index(last_idx + 1), None);
}

// =========================================================================
// 3. Irregular Partial Sealing Interleaved with Appends
// =========================================================================

#[test]
fn test_irregular_sealing_interleaved_with_appends() {
    let scale = LinearScale::new(2.0, -100.0); // y = 2x - 100
    let mut series = NativeScalarSeries::new(ChannelId(202), ScalarType::I64, scale);

    // 1. Append 10 samples -> seal
    let ts1: Vec<u64> = (0..10).map(|i| 100 + i * 10).collect();
    let vals1: Vec<i64> = (0..10).map(|i| i as i64 * 100).collect();
    series.append(&ts1, ScalarSlice::I64(&vals1)).unwrap();
    let h0 = series.seal_active().unwrap().expect("sealed chunk 0");
    assert_eq!(h0.sample_count, 10);

    // 2. Append 1 sample -> seal
    let ts2 = [200];
    let vals2 = [5000i64];
    series.append(&ts2, ScalarSlice::I64(&vals2)).unwrap();
    let h1 = series.seal_active().unwrap().expect("sealed chunk 1");
    assert_eq!(h1.sample_count, 1);

    // 3. Append 65535 samples -> seal
    let ts3: Vec<u64> = (0..65535).map(|i| 300 + i * 2).collect();
    let vals3: Vec<i64> = (0..65535).map(|i| i as i64).collect();
    series.append(&ts3, ScalarSlice::I64(&vals3)).unwrap();
    let h2 = series.seal_active().unwrap().expect("sealed chunk 2");
    assert_eq!(h2.sample_count, 65535);

    // 4. Append 70000 samples -> auto-seals 65536 into chunk 3, leaves 4464 in active buffer
    let ts4: Vec<u64> = (0..70000).map(|i| 200_000 + i * 2).collect();
    let vals4: Vec<i64> = (0..70000).map(|i| (i + 10) as i64).collect();
    series.append(&ts4, ScalarSlice::I64(&vals4)).unwrap();

    assert_eq!(series.sealed_chunk_count(), 4);
    assert_eq!(series.active_chunk().len(), 4464);
    let total = 10 + 1 + 65535 + 70000;
    assert_eq!(series.total_samples(), total as u64);

    // Verify index lookups on non-uniform chunk boundaries:
    // Chunk 0: index 0..10 (counts 10)
    let (t, v) = series.get_sample_by_index(0).unwrap();
    assert_eq!(t, 100);
    assert_eq!(v, 2.0 * 0.0 - 100.0); // -100.0
    let (t, v) = series.get_sample_by_index(9).unwrap();
    assert_eq!(t, 190);
    assert_eq!(v, 2.0 * 900.0 - 100.0); // 1700.0

    // Chunk 1: index 10 (counts 1)
    let (t, v) = series.get_sample_by_index(10).unwrap();
    assert_eq!(t, 200);
    assert_eq!(v, 2.0 * 5000.0 - 100.0); // 9900.0

    // Chunk 2: index 11..65546 (counts 65535)
    let (t, v) = series.get_sample_by_index(11).unwrap();
    assert_eq!(t, 300);
    assert_eq!(v, 2.0 * 0.0 - 100.0); // -100.0
    let (t, v) = series.get_sample_by_index(65545).unwrap();
    assert_eq!(t, 300 + 65534 * 2);
    assert_eq!(v, 2.0 * 65534.0 - 100.0);

    // Chunk 3: index 65546..131082 (counts 65536)
    let (t, v) = series.get_sample_by_index(65546).unwrap();
    assert_eq!(t, 200_000);
    assert_eq!(v, 2.0 * 10.0 - 100.0); // -80.0
    let (t, v) = series.get_sample_by_index(131081).unwrap();
    assert_eq!(t, 200_000 + 65535 * 2);
    assert_eq!(v, 2.0 * (65535.0 + 10.0) - 100.0);

    // Active buffer: index 131082..135546 (counts 4464)
    let (t, v) = series.get_sample_by_index(131082).unwrap();
    assert_eq!(t, 200_000 + 65536 * 2);
    assert_eq!(v, 2.0 * (65536.0 + 10.0) - 100.0);

    let (t, v) = series.get_sample_by_index(total as u64 - 1).unwrap();
    assert_eq!(t, 200_000 + 69999 * 2);
    assert_eq!(v, 2.0 * (69999.0 + 10.0) - 100.0);

    assert_eq!(series.get_sample_by_index(total as u64), None);
}

// =========================================================================
// 4. Timestamp Dual-Encoding Boundary and Nearest-Neighbor Resolution
// =========================================================================

#[test]
fn test_timestamp_relu32_vs_absu64_boundary_and_search() {
    let base: TimestampNs = 50_000_000_000; // 50s

    // Case A: exact span = MAX_REL_U32_NS (4.294967295s) -> fits in RelU32
    let ts_rel = [base, base + 1_000, base + MAX_REL_U32_NS];
    let col_rel = TimestampColumn::from_timestamps(base, &ts_rel);
    assert!(matches!(col_rel, TimestampColumn::RelU32 { .. }));
    assert_eq!(search_timestamp_in_column(&col_rel, base), Some(0));
    assert_eq!(search_timestamp_in_column(&col_rel, base + 500), Some(0)); // equidistant -> picks 0
    assert_eq!(search_timestamp_in_column(&col_rel, base + 501), Some(1));
    assert_eq!(
        search_timestamp_in_column(&col_rel, base + MAX_REL_U32_NS),
        Some(2)
    );

    // Case B: span exceeds MAX_REL_U32_NS by 1 ns -> falls back to AbsU64
    let ts_abs = [base, base + 1_000, base + MAX_REL_U32_NS + 1];
    let col_abs = TimestampColumn::from_timestamps(base, &ts_abs);
    assert!(matches!(col_abs, TimestampColumn::AbsU64(_)));
    assert_eq!(search_timestamp_in_column(&col_abs, base), Some(0));
    assert_eq!(search_timestamp_in_column(&col_abs, base + 500), Some(0));
    assert_eq!(search_timestamp_in_column(&col_abs, base + 501), Some(1));
    assert_eq!(
        search_timestamp_in_column(&col_abs, base + MAX_REL_U32_NS + 1),
        Some(2)
    );
}

#[test]
fn test_scalar_series_timestamp_nearest_neighbor_tie_breaking() {
    let mut series = NativeScalarSeries::new(
        ChannelId(1),
        ScalarType::F64,
        LinearScale::identity(),
    );

    let ts = [100, 200, 300];
    let vals = [1.0, 2.0, 3.0];
    series.append(&ts, ScalarSlice::F64(&vals)).unwrap();

    // Query before first -> None
    assert_eq!(series.get_sample_by_timestamp(99), None);

    // Query after last -> None
    assert_eq!(series.get_sample_by_timestamp(301), None);

    // Query exact hits
    assert_eq!(series.get_sample_by_timestamp(100), Some((0, 100, 1.0)));
    assert_eq!(series.get_sample_by_timestamp(200), Some((1, 200, 2.0)));
    assert_eq!(series.get_sample_by_timestamp(300), Some((2, 300, 3.0)));

    // Query midpoint: 150 is exactly halfway between 100 and 200
    // d_left (50) <= d_right (50) -> must break tie toward sample 0
    assert_eq!(series.get_sample_by_timestamp(150), Some((0, 100, 1.0)));

    // Query 151: closer to 200
    assert_eq!(series.get_sample_by_timestamp(151), Some((1, 200, 2.0)));

    // Query 249: closer to 200
    assert_eq!(series.get_sample_by_timestamp(249), Some((1, 200, 2.0)));

    // Query 250: equidistant between 200 and 300 -> breaks tie toward sample 1
    assert_eq!(series.get_sample_by_timestamp(250), Some((1, 200, 2.0)));
}

// =========================================================================
// 5. EventSeries Zero-Order Hold Across Inter-Chunk Gaps & Monotonicity
// =========================================================================

#[test]
fn test_eventseries_zoh_inter_chunk_gaps_and_simultaneous() {
    let mut series = EventSeries::new(ChannelId(303));

    // Chunk 0: transitions at 100 -> 1, 200 -> 2
    series.append(&[100, 200], &[1, 2]).unwrap();
    series.seal_active().unwrap();

    // Chunk 1: transitions at 1000 -> 10, 2000 -> 20
    series.append(&[1000, 2000], &[10, 20]).unwrap();
    series.seal_active().unwrap();

    // Active buffer: transitions at 5000 -> 50, 6000 -> 60
    series.append(&[5000, 6000], &[50, 60]).unwrap();

    assert_eq!(series.sealed_chunk_count(), 2);
    assert_eq!(series.total_samples(), 6);

    // Verification of Zero-Order Hold (ZOH) across chunks and gaps:
    assert_eq!(series.get_state_at(50), None); // before first transition
    assert_eq!(series.get_state_at(100), Some(1)); // at first transition
    assert_eq!(series.get_state_at(150), Some(1)); // inside chunk 0
    assert_eq!(series.get_state_at(200), Some(2)); // end of chunk 0
    assert_eq!(series.get_state_at(500), Some(2)); // IN GAP between chunk 0 and 1
    assert_eq!(series.get_state_at(999), Some(2)); // immediately before chunk 1
    assert_eq!(series.get_state_at(1000), Some(10)); // start of chunk 1
    assert_eq!(series.get_state_at(1500), Some(10)); // inside chunk 1
    assert_eq!(series.get_state_at(2000), Some(20)); // end of chunk 1
    assert_eq!(series.get_state_at(3500), Some(20)); // IN GAP between chunk 1 and active buffer
    assert_eq!(series.get_state_at(4999), Some(20)); // immediately before active buffer
    assert_eq!(series.get_state_at(5000), Some(50)); // start of active buffer
    assert_eq!(series.get_state_at(5500), Some(50)); // inside active buffer
    assert_eq!(series.get_state_at(6000), Some(60)); // latest transition
    assert_eq!(series.get_state_at(100_000), Some(60)); // held indefinitely into the future
}

#[test]
fn test_eventseries_simultaneous_transitions_and_atomic_rollback() {
    let mut series = EventSeries::new(ChannelId(304));

    // Simultaneous transitions: multiple states occurring at identical timestamp
    // e.g. state machine stepping through intermediate codes at same clock edge
    let ts = [100, 100, 200, 200, 200];
    let codes = [1, 2, 10, 20, 30];
    assert_eq!(series.append(&ts, &codes).unwrap(), 5);

    // At t=100, final effective state must be 2
    assert_eq!(series.get_state_at(100), Some(2));
    assert_eq!(series.get_state_at(150), Some(2));

    // At t=200, final effective state must be 30
    assert_eq!(series.get_state_at(200), Some(30));
    assert_eq!(series.get_state_at(250), Some(30));

    // Monotonic violation attempt: try to append backward timestamp 199
    let err = series.append(&[199], &[99]).expect_err("must reject retrograde timestamp");
    assert!(err.to_string().contains("non-monotonic"));

    // Verify series state was NOT corrupted by the failed append
    assert_eq!(series.total_samples(), 5);
    assert_eq!(series.latest_timestamp(), Some(200));
}

// =========================================================================
// 6. Watermark Notification Fanout Stress
// =========================================================================

#[test]
fn test_watermark_fanout_stress() {
    let mut series = NativeScalarSeries::new(
        ChannelId(777),
        ScalarType::F32,
        LinearScale::identity(),
    );

    let l1 = RecordingListener::new();
    let l2 = RecordingListener::new();
    let l3 = RecordingListener::new();

    series.register_listener(l1.clone());
    series.register_listener(l2.clone());
    series.register_listener(l3.clone());

    // 10 sequential appends
    for i in 0..10 {
        let ts = [1000 + i * 10, 1000 + i * 10 + 5];
        let vals = [i as f32, (i + 1) as f32];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
    }

    // Every listener must have received all 10 events in exact sequence
    for l in &[&l1, &l2, &l3] {
        assert_eq!(l.call_count(), 10);
        let evs = l.events();
        assert_eq!(evs.len(), 10);
        for (i, ev) in evs.iter().enumerate() {
            assert_eq!(ev.channel_id, ChannelId(777));
            assert_eq!(ev.latest_ts_ns, 1000 + i as u64 * 10 + 5);
            assert_eq!(ev.total_samples, (i + 1) as u64 * 2);
        }
    }
}
