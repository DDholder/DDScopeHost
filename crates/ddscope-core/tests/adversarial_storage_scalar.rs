//! Adversarial Stress and Edge-Case Test Suite for `NativeScalarSeries`.
//!
//! Attacks:
//! 1. Hostile batch sizing and rollover patterns (1-by-1 crossing, 65535+2 exact rollover, 500k massive ingest).
//! 2. Interleaved early flushes with irregular chunk sizes and full index/timestamp coverage.
//! 3. Timestamp query edge cases (chunk boundaries, huge inter-chunk gaps, mixed RelU32/AbsU64).
//! 4. Error injection and state immutability / rollback validation.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use ddscope_core::storage::{NativeScalarSeries, WatermarkEvent, WatermarkListener};
use ddscope_core::types::{
    ChannelId, ChunkStorageRef, CoreError, DataChunk, LinearScale, NativeScalarColumn,
    ScalarDataChunk, ScalarSlice, ScalarType, TimestampColumn, CHUNK_LEN,
};

/// Test listener for watermark verification under adversarial patterns.
struct CountingListener {
    count: AtomicUsize,
    last_samples: std::sync::Mutex<Option<u64>>,
    last_ts: std::sync::Mutex<Option<u64>>,
}

impl CountingListener {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            count: AtomicUsize::new(0),
            last_samples: std::sync::Mutex::new(None),
            last_ts: std::sync::Mutex::new(None),
        })
    }

    fn count(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }

    fn last_samples(&self) -> Option<u64> {
        *self.last_samples.lock().unwrap()
    }

    fn last_ts(&self) -> Option<u64> {
        *self.last_ts.lock().unwrap()
    }
}

impl WatermarkListener for CountingListener {
    fn on_watermark_advanced(&self, events: &[WatermarkEvent]) {
        self.count.fetch_add(events.len(), Ordering::SeqCst);
        if let Some(last) = events.last() {
            *self.last_samples.lock().unwrap() = Some(last.total_samples);
            *self.last_ts.lock().unwrap() = Some(last.latest_ts_ns);
        }
    }
}

// ============================================================================
// Domain 1: Hostile Batch Sizing & Rollover Patterns
// ============================================================================

#[test]
fn test_adversarial_rollover_one_by_one_across_chunk_boundary() {
    let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
    let listener = CountingListener::new();
    series.register_listener(listener.clone());

    // Crossing boundary 65,536 one sample at a time
    let total_samples = 65_536 + 10;
    for i in 0..total_samples {
        let ts = [i as u64 * 100];
        let val = [i as f32];
        let appended = series.append(&ts, ScalarSlice::F32(&val)).expect("append 1 sample");
        assert_eq!(appended, 1);

        if i < 65_535 {
            assert_eq!(series.sealed_chunk_count(), 0);
            assert_eq!(series.active_chunk().len(), i + 1);
        } else if i == 65_535 {
            // At 65,536 samples (index 65,535), chunk 0 must seal immediately
            assert_eq!(series.sealed_chunk_count(), 1);
            assert_eq!(series.active_chunk().len(), 0);
        } else {
            // New active chunk accumulating
            assert_eq!(series.sealed_chunk_count(), 1);
            assert_eq!(series.active_chunk().len(), i - 65_535);
        }
        assert_eq!(series.total_samples(), (i + 1) as u64);
    }

    assert_eq!(series.total_samples(), total_samples as u64);
    assert_eq!(series.sealed_chunk_count(), 1);
    assert_eq!(series.active_chunk().len(), 10);
    assert_eq!(listener.count(), total_samples);
    assert_eq!(listener.last_samples(), Some(total_samples as u64));

    // Verify all samples can be retrieved by index
    for i in 0..total_samples {
        let (ts, val) = series.get_sample_by_index(i as u64).expect("get_sample_by_index");
        assert_eq!(ts, i as u64 * 100);
        assert_eq!(val, i as f32 as f64);
    }
}

#[test]
fn test_adversarial_exact_boundary_split_65535_then_2() {
    let mut series = NativeScalarSeries::new(ChannelId(2), ScalarType::I32, LinearScale::identity());

    // Batch 1: exactly 65,535 samples
    let ts1: Vec<u64> = (0..65_535).map(|i| i as u64 * 10).collect();
    let val1: Vec<i32> = (0..65_535).collect();
    series.append(&ts1, ScalarSlice::I32(&val1)).expect("batch 1");

    assert_eq!(series.sealed_chunk_count(), 0);
    assert_eq!(series.active_chunk().len(), 65_535);
    assert_eq!(series.active_chunk().remaining_capacity(), 1);

    // Batch 2: exactly 2 samples
    let ts2 = [655_350u64, 655_360u64];
    let val2 = [65535i32, 65536i32];
    series.append(&ts2, ScalarSlice::I32(&val2)).expect("batch 2");

    // Chunk 0 must be sealed with 65,536 samples; active must have 1 sample
    assert_eq!(series.sealed_chunk_count(), 1);
    assert_eq!(series.active_chunk().len(), 1);
    assert_eq!(series.total_samples(), 65_537);

    let sealed_handle = &series.sealed_chunks()[0];
    assert_eq!(sealed_handle.sample_count, CHUNK_LEN as u32);
    assert_eq!(sealed_handle.t_start_ns, 0);
    assert_eq!(sealed_handle.t_end_ns, 655_350);

    // Retrieval verification
    let (t_last_sealed, v_last_sealed) = series.get_sample_by_index(65_535).unwrap();
    assert_eq!(t_last_sealed, 655_350);
    assert_eq!(v_last_sealed, 65535.0);

    let (t_active, v_active) = series.get_sample_by_index(65_536).unwrap();
    assert_eq!(t_active, 655_360);
    assert_eq!(v_active, 65536.0);
}

#[test]
fn test_adversarial_massive_ingest_500k_samples_in_single_append() {
    let mut series = NativeScalarSeries::new(ChannelId(3), ScalarType::F64, LinearScale::new(2.0, 5.0));
    let listener = CountingListener::new();
    series.register_listener(listener.clone());

    let count = 500_000usize;
    let ts: Vec<u64> = (0..count).map(|i| i as u64 * 5).collect();
    let val: Vec<f64> = (0..count).map(|i| i as f64).collect();

    let appended = series.append(&ts, ScalarSlice::F64(&val)).expect("massive append");
    assert_eq!(appended, count);

    // 500,000 / 65,536 = 7 full chunks sealed (458,752 samples), 41,248 in active
    let expected_sealed = 500_000 / CHUNK_LEN;
    let expected_active = 500_000 % CHUNK_LEN;
    assert_eq!(series.sealed_chunk_count(), expected_sealed);
    assert_eq!(series.active_chunk().len(), expected_active);
    assert_eq!(series.total_samples(), 500_000);

    // Listener fired exactly once for this batch
    assert_eq!(listener.count(), 1);
    assert_eq!(listener.last_samples(), Some(500_000));
    assert_eq!(listener.last_ts(), Some((count - 1) as u64 * 5));

    // Verify chunk boundaries across all sealed chunks and active
    for c in 0..expected_sealed {
        let first_idx = (c * CHUNK_LEN) as u64;
        let last_idx = ((c + 1) * CHUNK_LEN - 1) as u64;

        let (t0, v0) = series.get_sample_by_index(first_idx).unwrap();
        assert_eq!(t0, first_idx * 5);
        assert_eq!(v0, (first_idx as f64) * 2.0 + 5.0);

        let (t1, v1) = series.get_sample_by_index(last_idx).unwrap();
        assert_eq!(t1, last_idx * 5);
        assert_eq!(v1, (last_idx as f64) * 2.0 + 5.0);
    }

    // Verify first and last active sample
    let first_active_idx = (expected_sealed * CHUNK_LEN) as u64;
    let (t_act0, v_act0) = series.get_sample_by_index(first_active_idx).unwrap();
    assert_eq!(t_act0, first_active_idx * 5);
    assert_eq!(v_act0, (first_active_idx as f64) * 2.0 + 5.0);

    let last_active_idx = (count - 1) as u64;
    let (t_act1, v_act1) = series.get_sample_by_index(last_active_idx).unwrap();
    assert_eq!(t_act1, last_active_idx * 5);
    assert_eq!(v_act1, (last_active_idx as f64) * 2.0 + 5.0);
}

// ============================================================================
// Domain 2: Interleaved Early Flushes & Irregular Chunk Lookups
// ============================================================================

#[test]
fn test_adversarial_interleaved_early_flushes_and_dense_lookups() {
    let mut series = NativeScalarSeries::new(ChannelId(10), ScalarType::I16, LinearScale::new(3.0, -10.0));

    // 1. Batch of 100 samples -> seal_active
    let b1_ts: Vec<u64> = (0..100).map(|i| 1_000 + i * 10).collect();
    let b1_val: Vec<i16> = (0..100).map(|i| (i * 2) as i16).collect();
    series.append(&b1_ts, ScalarSlice::I16(&b1_val)).unwrap();
    let h1 = series.seal_active().unwrap().expect("seal chunk 1");
    assert_eq!(h1.sample_count, 100);

    // 2. Batch of 1 sample -> seal_active
    let b2_ts = [2_000u64];
    let b2_val = [999i16];
    series.append(&b2_ts, ScalarSlice::I16(&b2_val)).unwrap();
    let h2 = series.seal_active().unwrap().expect("seal chunk 2");
    assert_eq!(h2.sample_count, 1);

    // 3. Batch of 65,500 samples -> seal_active
    let b3_ts: Vec<u64> = (0..65_500).map(|i| 3_000 + i * 2).collect();
    let b3_val: Vec<i16> = (0..65_500).map(|i| (i % 30_000) as i16).collect();
    series.append(&b3_ts, ScalarSlice::I16(&b3_val)).unwrap();
    let h3 = series.seal_active().unwrap().expect("seal chunk 3");
    assert_eq!(h3.sample_count, 65_500);

    // 4. Batch of 50 samples in active buffer (unsealed)
    let b4_ts: Vec<u64> = (0..50).map(|i| 200_000 + i * 5).collect();
    let b4_val: Vec<i16> = (0..50).map(|i| (i + 10) as i16).collect();
    series.append(&b4_ts, ScalarSlice::I16(&b4_val)).unwrap();

    let total = 100 + 1 + 65_500 + 50;
    assert_eq!(series.total_samples(), total as u64);
    assert_eq!(series.sealed_chunk_count(), 3);
    assert_eq!(series.active_chunk().len(), 50);

    // Master vectors for reference oracle
    let mut oracle_ts = Vec::with_capacity(total);
    let mut oracle_val = Vec::with_capacity(total);
    oracle_ts.extend_from_slice(&b1_ts);
    oracle_val.extend(b1_val.iter().map(|&v| (v as f64) * 3.0 - 10.0));
    oracle_ts.extend_from_slice(&b2_ts);
    oracle_val.extend(b2_val.iter().map(|&v| (v as f64) * 3.0 - 10.0));
    oracle_ts.extend_from_slice(&b3_ts);
    oracle_val.extend(b3_val.iter().map(|&v| (v as f64) * 3.0 - 10.0));
    oracle_ts.extend_from_slice(&b4_ts);
    oracle_val.extend(b4_val.iter().map(|&v| (v as f64) * 3.0 - 10.0));

    // Exhaustive index verification across all 65,651 irregular samples
    for (idx, (&exp_t, &exp_v)) in oracle_ts.iter().zip(oracle_val.iter()).enumerate() {
        let (actual_t, actual_v) = series
            .get_sample_by_index(idx as u64)
            .unwrap_or_else(|| panic!("failed to get sample at index {idx}"));
        assert_eq!(actual_t, exp_t, "ts mismatch at index {idx}");
        assert_eq!(actual_v, exp_v, "val mismatch at index {idx}");
    }

    // Out of bounds check
    assert!(series.get_sample_by_index(total as u64).is_none());
    assert!(series.get_sample_by_index((total + 100) as u64).is_none());

    // Timestamp query verification across all irregular chunks (sampled every 17th sample)
    for (idx, &exp_t) in oracle_ts.iter().enumerate().step_by(17) {
        let (found_idx, found_t, found_v) = series
            .get_sample_by_timestamp(exp_t)
            .unwrap_or_else(|| panic!("failed to get sample at ts {exp_t}"));
        assert_eq!(found_t, exp_t);
        assert_eq!(found_idx, idx as u64);
        assert_eq!(found_v, oracle_val[idx]);
    }
}

// ============================================================================
// Domain 3: Timestamp Query Edge Cases
// ============================================================================

#[test]
fn test_adversarial_boundary_timestamps_and_inter_chunk_gaps() {
    let mut series = NativeScalarSeries::new(ChannelId(20), ScalarType::F32, LinearScale::identity());

    // Chunk 0: t in [1000, 2000]
    let c0_ts = [1000u64, 1500, 2000];
    let c0_val = [1.0f32, 1.5, 2.0];
    series.append(&c0_ts, ScalarSlice::F32(&c0_val)).unwrap();
    series.seal_active().unwrap();

    // Chunk 1: Huge gap! t in [100_000_000_000, 100_000_001_000] (100 seconds later)
    let c1_ts = [100_000_000_000u64, 100_000_000_500, 100_000_001_000];
    let c1_val = [10.0f32, 10.5, 11.0];
    series.append(&c1_ts, ScalarSlice::F32(&c1_val)).unwrap();
    series.seal_active().unwrap();

    // 1. Exact boundary hits
    // t_end of chunk 0 (2000)
    let (idx0_end, t0_end, _) = series.get_sample_by_timestamp(2000).unwrap();
    assert_eq!(idx0_end, 2);
    assert_eq!(t0_end, 2000);

    // t_start of chunk 1 (100_000_000_000)
    let (idx1_start, t1_start, _) = series.get_sample_by_timestamp(100_000_000_000).unwrap();
    assert_eq!(idx1_start, 3);
    assert_eq!(t1_start, 100_000_000_000);

    // 2. Querying inside the huge inter-chunk gap (2000 .. 100_000_000_000)
    // Querying very close to chunk 0 end: t = 2001
    let (idx_gap_left, t_gap_left, _) = series.get_sample_by_timestamp(2001).unwrap();
    assert_eq!(idx_gap_left, 2);
    assert_eq!(t_gap_left, 2000);

    // Querying very close to chunk 1 start: t = 99_999_999_999
    let (idx_gap_right, t_gap_right, _) = series.get_sample_by_timestamp(99_999_999_999).unwrap();
    assert_eq!(idx_gap_right, 3);
    assert_eq!(t_gap_right, 100_000_000_000);

    // Exact midpoint: (2000 + 100_000_000_000) / 2 = 50_000_001_000
    // d_left == d_right -> tie break chooses left (prev_idx: 2)
    let midpoint = (2000 + 100_000_000_000) / 2;
    let (idx_mid, t_mid, _) = series.get_sample_by_timestamp(midpoint).unwrap();
    assert_eq!(idx_mid, 2);
    assert_eq!(t_mid, 2000);

    // 1 ns past midpoint -> chooses right (next_idx: 3)
    let (idx_mid_p1, t_mid_p1, _) = series.get_sample_by_timestamp(midpoint + 1).unwrap();
    assert_eq!(idx_mid_p1, 3);
    assert_eq!(t_mid_p1, 100_000_000_000);

    // 3. Out of overall bounds queries
    assert!(series.get_sample_by_timestamp(999).is_none());
    assert!(series.get_sample_by_timestamp(100_000_001_001).is_none());
}

#[test]
fn test_adversarial_queries_across_mixed_relu32_and_absu64_chunks() {
    let mut series = NativeScalarSeries::new(ChannelId(30), ScalarType::U32, LinearScale::identity());

    // Chunk 0: Short span (100 samples with 1 ms step = 99 ms span < 4.29s).
    // TimestampColumn::from_slice selects RelU32!
    let c0_ts: Vec<u64> = (0..100).map(|i| 1_000_000_000 + i * 1_000_000).collect();
    let c0_val: Vec<u32> = (0..100).map(|i| i as u32).collect();
    series.append(&c0_ts, ScalarSlice::U32(&c0_val)).unwrap();
    series.seal_active().unwrap();

    // Chunk 1: Wide span (> 4.29s, say 100 samples with 100ms step = 9.9s span).
    // TimestampColumn::from_slice selects AbsU64!
    let c1_start = 2_000_000_000u64;
    let c1_ts: Vec<u64> = (0..100).map(|i| c1_start + i * 100_000_000).collect();
    let c1_val: Vec<u32> = (0..100).map(|i| (i + 1000) as u32).collect();
    series.append(&c1_ts, ScalarSlice::U32(&c1_val)).unwrap();
    series.seal_active().unwrap();

    // Chunk 2: Short span again (50 samples with 500 us step = 24.5 ms < 4.29s).
    // TimestampColumn::from_slice selects RelU32!
    let c2_start = 20_000_000_000u64;
    let c2_ts: Vec<u64> = (0..50).map(|i| c2_start + i * 500_000).collect();
    let c2_val: Vec<u32> = (0..50).map(|i| (i + 5000) as u32).collect();
    series.append(&c2_ts, ScalarSlice::U32(&c2_val)).unwrap();
    series.seal_active().unwrap();

    // Verify chunk encoding types under the hood
    let sealed = series.sealed_chunks();
    assert_eq!(sealed.len(), 3);

    let verify_chunk_encoding = |idx: usize, expect_rel: bool| {
        if let ChunkStorageRef::InMemory(dc) = &sealed[idx].storage {
            if let DataChunk::Scalar(sc) = dc.as_ref() {
                match &sc.timestamps {
                    TimestampColumn::RelU32 { .. } => assert!(expect_rel, "Chunk {idx} should be RelU32"),
                    TimestampColumn::AbsU64(_) => assert!(!expect_rel, "Chunk {idx} should be AbsU64"),
                }
            } else {
                panic!("Expected DataChunk::Scalar");
            }
        } else {
            panic!("Expected ChunkStorageRef::InMemory");
        }
    };

    verify_chunk_encoding(0, true);  // Chunk 0 is RelU32
    verify_chunk_encoding(1, false); // Chunk 1 is AbsU64
    verify_chunk_encoding(2, true);  // Chunk 2 is RelU32

    // Query inside Chunk 0 (RelU32)
    let q0_ts = 1_000_000_000 + 42 * 1_000_000;
    let (idx0, t0, v0) = series.get_sample_by_timestamp(q0_ts).unwrap();
    assert_eq!(idx0, 42);
    assert_eq!(t0, q0_ts);
    assert_eq!(v0, 42.0);

    // Query inside Chunk 1 (AbsU64)
    let q1_ts = c1_start + 75 * 100_000_000;
    let (idx1, t1, v1) = series.get_sample_by_timestamp(q1_ts).unwrap();
    assert_eq!(idx1, 100 + 75);
    assert_eq!(t1, q1_ts);
    assert_eq!(v1, 1075.0);

    // Query inside Chunk 2 (RelU32)
    let q2_ts = c2_start + 18 * 500_000;
    let (idx2, t2, v2) = series.get_sample_by_timestamp(q2_ts).unwrap();
    assert_eq!(idx2, 100 + 100 + 18);
    assert_eq!(t2, q2_ts);
    assert_eq!(v2, 5018.0);
}

// ============================================================================
// Domain 4: Error Injection & State Immutability
// ============================================================================

#[test]
fn test_adversarial_error_injection_mismatched_types_leaves_state_unaltered() {
    let mut series = NativeScalarSeries::new(ChannelId(40), ScalarType::F32, LinearScale::identity());

    // 1. Initial valid append: 10 samples
    let valid_ts: Vec<u64> = (0..10).map(|i| i * 100).collect();
    let valid_val: Vec<f32> = (0..10).map(|i| i as f32).collect();
    series.append(&valid_ts, ScalarSlice::F32(&valid_val)).unwrap();
    assert_eq!(series.total_samples(), 10);
    assert_eq!(series.active_chunk().len(), 10);

    // 2. Inject mismatched scalar type: pass I32 slice to F32 series
    let bad_ts = [1000u64, 1100];
    let bad_val = [42i32, 99];
    let err = series.append(&bad_ts, ScalarSlice::I32(&bad_val));
    assert!(err.is_err(), "Must reject mismatched scalar type");
    match err {
        Err(CoreError::Storage(msg)) => assert!(msg.contains("Scalar type mismatch")),
        _ => panic!("Expected Storage error"),
    }

    // 3. Verify state is 100% unaltered
    assert_eq!(series.total_samples(), 10);
    assert_eq!(series.active_chunk().len(), 10);
    assert_eq!(series.sealed_chunk_count(), 0);
    for i in 0..10 {
        let (t, v) = series.get_sample_by_index(i as u64).unwrap();
        assert_eq!(t, i as u64 * 100);
        assert_eq!(v, i as f32 as f64);
    }
    assert!(series.get_sample_by_index(10).is_none());
}

#[test]
fn test_adversarial_error_injection_mismatched_lengths_leaves_state_unaltered() {
    let mut series = NativeScalarSeries::new(ChannelId(41), ScalarType::F32, LinearScale::identity());

    // 1. Initial valid append: 5 samples
    let valid_ts: Vec<u64> = (0..5).map(|i| i * 100).collect();
    let valid_val: Vec<f32> = (0..5).map(|i| i as f32).collect();
    series.append(&valid_ts, ScalarSlice::F32(&valid_val)).unwrap();
    assert_eq!(series.total_samples(), 5);

    // 2. Inject mismatched slice lengths: 4 timestamps, 2 values
    let bad_ts = [500u64, 600, 700, 800];
    let bad_val = [5.0f32, 6.0];
    let err = series.append(&bad_ts, ScalarSlice::F32(&bad_val));
    assert!(err.is_err(), "Must reject mismatched slice lengths");
    match err {
        Err(CoreError::Storage(msg)) => assert!(msg.contains("Batch length mismatch")),
        _ => panic!("Expected Storage error"),
    }

    // 3. Verify state is 100% unaltered
    assert_eq!(series.total_samples(), 5);
    assert_eq!(series.active_chunk().len(), 5);
    for i in 0..5 {
        let (t, v) = series.get_sample_by_index(i as u64).unwrap();
        assert_eq!(t, i as u64 * 100);
        assert_eq!(v, i as f32 as f64);
    }
    assert!(series.get_sample_by_index(5).is_none());
}

#[test]
fn test_adversarial_error_injection_backwards_timestamps_contract_violation() {
    let mut series = NativeScalarSeries::new(ChannelId(50), ScalarType::F32, LinearScale::identity());

    // 1. Initial valid append: 3 samples
    let valid_ts = [100u64, 150, 200];
    let valid_val = [1.0f32, 2.0, 3.0];
    series.append(&valid_ts, ScalarSlice::F32(&valid_val)).unwrap();
    assert_eq!(series.total_samples(), 3);
    assert_eq!(series.active_chunk().len(), 3);

    // 2. Reject cross-batch backwards timestamp (timestamps[0] < last_ts)
    // Last committed ts was 200; new batch attempts to start at 180 < 200
    let backward_cross_ts = [180u64, 250];
    let backward_cross_val = [99.0f32, 99.0];
    let res1 = series.append(&backward_cross_ts, ScalarSlice::F32(&backward_cross_val));
    assert!(res1.is_err(), "Must reject backwards timestamp relative to prior sample");
    match res1 {
        Err(CoreError::Storage(msg)) => {
            assert!(
                msg.to_lowercase().contains("monotonic"),
                "Expected monotonic error message, got: {msg}"
            );
        }
        other => panic!("Expected Storage error, got: {other:?}"),
    }

    // Verify state is 100% unaltered
    assert_eq!(series.total_samples(), 3);
    assert_eq!(series.active_chunk().len(), 3);
    assert_eq!(series.sealed_chunk_count(), 0);
    for i in 0..3 {
        let (t, v) = series.get_sample_by_index(i as u64).unwrap();
        assert_eq!(t, valid_ts[i]);
        assert_eq!(v, valid_val[i] as f64);
    }
    assert!(series.get_sample_by_index(3).is_none());

    // 3. Reject intra-batch backwards timestamp (timestamps[i] < timestamps[i-1])
    let backward_intra_ts = [300u64, 250];
    let backward_intra_val = [99.0f32, 99.0];
    let res2 = series.append(&backward_intra_ts, ScalarSlice::F32(&backward_intra_val));
    assert!(res2.is_err(), "Must reject intra-batch backwards timestamp");
    match res2 {
        Err(CoreError::Storage(msg)) => {
            assert!(
                msg.to_lowercase().contains("monotonic"),
                "Expected monotonic error message, got: {msg}"
            );
        }
        other => panic!("Expected Storage error, got: {other:?}"),
    }

    // Verify state is still 100% unaltered
    assert_eq!(series.total_samples(), 3);
    assert_eq!(series.active_chunk().len(), 3);
    assert_eq!(series.sealed_chunk_count(), 0);
    for i in 0..3 {
        let (t, v) = series.get_sample_by_index(i as u64).unwrap();
        assert_eq!(t, valid_ts[i]);
        assert_eq!(v, valid_val[i] as f64);
    }
    assert!(series.get_sample_by_index(3).is_none());

    // 4. Verify series remains fully operational: append valid next samples
    let next_valid_ts = [250u64, 300];
    let next_valid_val = [4.0f32, 5.0];
    series.append(&next_valid_ts, ScalarSlice::F32(&next_valid_val)).unwrap();
    assert_eq!(series.total_samples(), 5);
    assert_eq!(series.active_chunk().len(), 5);
    for i in 0..5 {
        assert!(series.get_sample_by_index(i as u64).is_some());
    }
}

#[test]
fn test_adversarial_error_injection_intra_batch_inversion_corrupts_timestamp_search() {
    let mut series = NativeScalarSeries::new(ChannelId(52), ScalarType::F32, LinearScale::identity());

    // Inversion where first <= last: [100, 200, 50, 150]
    // Here first is 100, last is 150 (100 <= 150), but index 2 (50) is < index 1 (200).
    let inverted_ts = [100u64, 200, 50, 150];
    let vals = [1.0f32, 2.0, 3.0, 4.0];

    let append_res = series.append(&inverted_ts, ScalarSlice::F32(&vals));
    assert!(
        append_res.is_err(),
        "Must reject intra-batch timestamp inversion even when first <= last"
    );
    match append_res {
        Err(CoreError::Storage(msg)) => {
            assert!(
                msg.to_lowercase().contains("monotonic"),
                "Expected monotonic error message, got: {msg}"
            );
        }
        other => panic!("Expected Storage error, got: {other:?}"),
    }

    // Verify state is 100% unaltered
    assert_eq!(series.total_samples(), 0);
    assert_eq!(series.active_chunk().len(), 0);
    assert_eq!(series.sealed_chunk_count(), 0);
    assert!(series.get_sample_by_index(0).is_none());

    // Also verify direct ScalarDataChunk construction rejects intra-chunk inversions
    let bad_ts_col = TimestampColumn::from_slice(&inverted_ts);
    let mut bad_vals = NativeScalarColumn::with_capacity(ScalarType::F32, 4);
    bad_vals.append_slice(ScalarSlice::F32(&vals)).unwrap();
    let chunk_res = ScalarDataChunk::new(
        bad_ts_col,
        bad_vals,
        LinearScale::identity(),
    );
    assert!(
        chunk_res.is_err(),
        "ScalarDataChunk::new must reject intra-chunk inversions even when first <= last"
    );
    match chunk_res {
        Err(CoreError::Storage(msg)) => {
            assert!(
                msg.to_lowercase().contains("monotonic"),
                "Expected monotonic error message in chunk creation, got: {msg}"
            );
        }
        other => panic!("Expected Storage error, got: {other:?}"),
    }
}

#[test]
fn test_adversarial_error_injection_boundary_fill_seal_failure_wipes_active_buffer() {
    let mut series = NativeScalarSeries::new(ChannelId(51), ScalarType::F32, LinearScale::identity());

    // Step 1: Pre-populate active buffer with 5 valid samples
    let valid_ts: Vec<u64> = (0..5).map(|i| 100 + i * 10).collect();
    let valid_val: Vec<f32> = (0..5).map(|i| i as f32).collect();
    series.append(&valid_ts, ScalarSlice::F32(&valid_val)).unwrap();
    assert_eq!(series.total_samples(), 5);
    assert_eq!(series.active_chunk().len(), 5);

    // Step 2: Attempt to append 65,531 samples containing an inverted tail timestamp
    let fill_count = CHUNK_LEN - 5;
    let mut bad_ts: Vec<u64> = (0..fill_count).map(|i| 200 + i as u64).collect();
    *bad_ts.last_mut().unwrap() = 50; // Inverted tail timestamp
    let bad_val: Vec<f32> = vec![1.0; fill_count];

    let append_res = series.append(&bad_ts, ScalarSlice::F32(&bad_val));
    assert!(append_res.is_err(), "Append must return Err for non-monotonic batch");

    // Invariant check: Original 5 valid samples must be 100% preserved
    assert_eq!(series.sealed_chunk_count(), 0);
    assert_eq!(
        series.active_chunk().len(),
        5,
        "Active buffer must retain original 5 samples!"
    );
    assert_eq!(series.total_samples(), 5, "total_samples must remain 5!");

    for i in 0..5 {
        let (t, v) = series
            .get_sample_by_index(i as u64)
            .expect("original samples must survive");
        assert_eq!(t, 100 + i as u64 * 10);
        assert_eq!(v, i as f32 as f64);
    }
    assert!(series.get_sample_by_index(5).is_none());

    // Step 3: Verify series remains operational by appending valid samples
    let recovery_ts: Vec<u64> = (0..10).map(|i| 200 + i * 10).collect();
    let recovery_val: Vec<f32> = vec![2.0; 10];
    series.append(&recovery_ts, ScalarSlice::F32(&recovery_val)).unwrap();
    assert_eq!(series.total_samples(), 15);
    assert_eq!(series.active_chunk().len(), 15);
    for i in 0..15 {
        assert!(series.get_sample_by_index(i as u64).is_some());
    }
}


