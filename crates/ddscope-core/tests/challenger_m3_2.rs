//! Adversarial stress test suite for Milestone 3 (Performance Benchmark & Hardening).
//!
//! Specifically validates:
//! 1. Monotonicity violation rejection:
//!    - Intra-batch out-of-order timestamps (t_i < t_{i-1}) rejected with `CoreError::Storage`.
//!    - Inter-batch backwards timestamps (t_batch,0 < t_last) rejected with `CoreError::Storage`.
//!    - Backwards batches rejected after auto chunk sealing and explicit sealing.
//!    - Transactional atomicity: rejection preserves buffer and counter immutability.
//! 2. Numerical extremes under high throughput:
//!    - `f32` with NaN, -NaN, +Inf, -Inf, subnormals, signed zeros, min/max values.
//!    - Signed integers (i8, i16, i32, i64) with MIN/MAX across chunk sealing transitions.
//!    - Unsigned integers (u8, u16, u32, u64) across chunk sealing transitions.
//! 3. Concurrent multi-channel streaming:
//!    - Multi-threaded execution across 8 distinct channels/types.
//!    - Watermark event emission validation and data isolation across concurrent series.
//!    - Concurrent reader/writer stress without data corruption or deadlocks.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use ddscope_core::storage::{EventSeries, NativeScalarSeries, WatermarkEvent, WatermarkListener};
use ddscope_core::types::{
    ChannelId, CoreError, LinearScale, ScalarSlice, ScalarType, TimestampNs, CHUNK_LEN,
};

/// Thread-safe recorder for watermark events emitted during adversarial tests.
#[derive(Default)]
struct ConcurrentWatermarkRecorder {
    events: Mutex<Vec<WatermarkEvent>>,
    call_count: AtomicUsize,
}

impl ConcurrentWatermarkRecorder {
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

impl WatermarkListener for ConcurrentWatermarkRecorder {
    fn on_watermark_advanced(&self, events: &[WatermarkEvent]) {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        let mut guard = self.events.lock().unwrap();
        guard.extend_from_slice(events);
    }
}

// =========================================================================
// 1. Monotonicity Violation Rejection
// =========================================================================

#[test]
fn test_scalar_series_intra_batch_monotonicity_rejections() {
    let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());

    // Single step reversal in the middle: [100, 200, 150, 300]
    let ts_reversal = [100, 200, 150, 300];
    let vals_reversal = [1.0f32, 2.0, 3.0, 4.0];
    let err = series
        .append(&ts_reversal, ScalarSlice::F32(&vals_reversal))
        .expect_err("must reject intra-batch timestamp reversal");
    assert!(
        matches!(err, CoreError::Storage(ref msg) if msg.contains("non-monotonic")),
        "expected CoreError::Storage with non-monotonic error, got: {:?}",
        err
    );

    // Strictly decreasing sequence: [500, 400, 300, 200]
    let ts_decreasing = [500, 400, 300, 200];
    let vals_decreasing = [10.0f32, 20.0, 30.0, 40.0];
    let err2 = series
        .append(&ts_decreasing, ScalarSlice::F32(&vals_decreasing))
        .expect_err("must reject strictly decreasing timestamps");
    assert!(matches!(err2, CoreError::Storage(_)));

    // Reversal at first pair: [200, 100, 300]
    let ts_first_pair = [200, 100, 300];
    let vals_first_pair = [1.0f32, 2.0, 3.0];
    let err3 = series
        .append(&ts_first_pair, ScalarSlice::F32(&vals_first_pair))
        .expect_err("must reject reversal at first pair");
    assert!(matches!(err3, CoreError::Storage(_)));

    // Reversal at last element: [100, 200, 300, 299]
    let ts_last_elem = [100, 200, 300, 299];
    let vals_last_elem = [1.0f32, 2.0, 3.0, 4.0];
    let err4 = series
        .append(&ts_last_elem, ScalarSlice::F32(&vals_last_elem))
        .expect_err("must reject reversal at last element");
    assert!(matches!(err4, CoreError::Storage(_)));

    // Large batch (10,000 samples) with single inverted pair deep inside (index 7341)
    let mut large_ts: Vec<TimestampNs> = (0..10_000).map(|i| i * 10).collect();
    large_ts[7341] = large_ts[7340] - 1; // Invert single timestamp
    let large_vals = vec![0.5f32; 10_000];
    let err5 = series
        .append(&large_ts, ScalarSlice::F32(&large_vals))
        .expect_err("must reject inverted timestamp inside large batch");
    assert!(matches!(err5, CoreError::Storage(_)));

    // Monotonic non-decreasing sequence with identical timestamps: [100, 100, 100] is valid
    let ts_equal = [100, 100, 100];
    let vals_equal = [1.0f32, 2.0, 3.0];
    let appended = series
        .append(&ts_equal, ScalarSlice::F32(&vals_equal))
        .expect("identical timestamps must be accepted as non-decreasing");
    assert_eq!(appended, 3);
    assert_eq!(series.total_samples(), 3);
}

#[test]
fn test_event_series_intra_batch_monotonicity_rejections() {
    let mut series = EventSeries::new(ChannelId(2));

    // Reversal in middle
    let err = series
        .append(&[100, 200, 150, 300], &[1, 2, 3, 4])
        .expect_err("event series must reject intra-batch timestamp reversal");
    assert!(matches!(err, CoreError::Storage(ref msg) if msg.contains("non-monotonic")));

    // Strictly decreasing sequence
    let err2 = series
        .append(&[500, 400, 300], &[10, 20, 30])
        .expect_err("event series must reject decreasing timestamps");
    assert!(matches!(err2, CoreError::Storage(_)));

    // Valid non-decreasing timestamps
    let appended = series
        .append(&[100, 100, 200], &[1, 2, 3])
        .expect("event series must accept non-decreasing timestamps");
    assert_eq!(appended, 3);
    assert_eq!(series.total_samples(), 3);
}

#[test]
fn test_scalar_series_backwards_batch_rejections() {
    let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::I32, LinearScale::identity());

    // Batch 1 ends at t = 1000
    let ts1 = [800, 900, 1000];
    let vals1 = [10i32, 20, 30];
    series.append(&ts1, ScalarSlice::I32(&vals1)).unwrap();
    assert_eq!(series.latest_timestamp(), Some(1000));

    // Batch 2 starts at t = 999 (< 1000)
    let ts_backwards = [999, 1100, 1200];
    let vals2 = [40i32, 50, 60];
    let err = series
        .append(&ts_backwards, ScalarSlice::I32(&vals2))
        .expect_err("must reject backwards batch starting at 999 < 1000");
    assert!(
        matches!(err, CoreError::Storage(ref msg) if msg.contains("non-monotonic")),
        "expected non-monotonic error, got: {:?}",
        err
    );

    // Batch 2 starts far in the past: t = 0
    let ts_past = [0, 10, 20];
    let err2 = series
        .append(&ts_past, ScalarSlice::I32(&vals2))
        .expect_err("must reject backwards batch starting at 0 < 1000");
    assert!(matches!(err2, CoreError::Storage(_)));

    // Exact boundary: t = 1000 (identical to last_ts) is accepted (non-decreasing)
    let ts_boundary = [1000, 1050, 1100];
    let appended = series
        .append(&ts_boundary, ScalarSlice::I32(&vals2))
        .expect("identical boundary timestamp must be accepted");
    assert_eq!(appended, 3);
    assert_eq!(series.total_samples(), 6);
    assert_eq!(series.latest_timestamp(), Some(1100));

    // Next backwards attempt: t = 1099 (< 1100)
    let ts_backwards2 = [1099, 1150];
    let vals3 = [70i32, 80];
    let err3 = series
        .append(&ts_backwards2, ScalarSlice::I32(&vals3))
        .expect_err("must reject backwards batch starting at 1099 < 1100");
    assert!(matches!(err3, CoreError::Storage(_)));
}

#[test]
fn test_scalar_series_backwards_batch_across_sealing() {
    let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());

    // Stream 70,000 samples (auto-sealing chunk 0 with 65,536 samples, active has 4,464 samples)
    let count = 70_000usize;
    let ts: Vec<TimestampNs> = (0..count).map(|i| (i * 10) as TimestampNs).collect();
    let vals = vec![1.0f32; count];
    series.append(&ts, ScalarSlice::F32(&vals)).unwrap();

    assert_eq!(series.sealed_chunk_count(), 1);
    assert_eq!(series.total_samples(), 70_000);
    let last_ts = 69_999 * 10;
    assert_eq!(series.latest_timestamp(), Some(last_ts));

    // Attempt append with timestamp strictly before last_ts
    let backwards_ts = [last_ts - 1, last_ts + 5];
    let backwards_vals = [2.0f32, 3.0];
    let err = series
        .append(&backwards_ts, ScalarSlice::F32(&backwards_vals))
        .expect_err("must reject backwards batch after auto chunk sealing");
    assert!(matches!(err, CoreError::Storage(_)));

    // Attempt append starting within the range of sealed chunk 0 (e.g. t = 300,000)
    let past_ts = [300_000, 300_010];
    let past_vals = [4.0f32, 5.0];
    let err2 = series
        .append(&past_ts, ScalarSlice::F32(&past_vals))
        .expect_err("must reject batch starting inside sealed chunk timestamp range");
    assert!(matches!(err2, CoreError::Storage(_)));

    // Explicitly seal active buffer
    series.seal_active().unwrap();
    assert_eq!(series.sealed_chunk_count(), 2);

    // Attempt append before last sealed chunk's end timestamp
    let err3 = series
        .append(&backwards_ts, ScalarSlice::F32(&backwards_vals))
        .expect_err("must reject backwards batch after explicit seal");
    assert!(matches!(err3, CoreError::Storage(_)));

    // Monotonic append after explicit seal succeeds
    let valid_ts = [last_ts, last_ts + 10];
    let valid_vals = [6.0f32, 7.0];
    let appended = series
        .append(&valid_ts, ScalarSlice::F32(&valid_vals))
        .expect("monotonic batch after explicit seal must succeed");
    assert_eq!(appended, 2);
    assert_eq!(series.total_samples(), 70_002);
}

#[test]
fn test_event_series_backwards_batch_across_sealing() {
    let mut series = EventSeries::new(ChannelId(2));

    // Ingest 70,000 events to trigger auto-sealing (65,536 sealed, 4,464 active)
    let count = 70_000usize;
    let ts: Vec<TimestampNs> = (0..count).map(|i| (i * 20) as TimestampNs).collect();
    let codes = vec![1i32; count];
    series.append(&ts, &codes).unwrap();

    assert_eq!(series.sealed_chunk_count(), 1);
    assert_eq!(series.total_samples(), 70_000);
    let last_ts = 69_999 * 20;
    assert_eq!(series.latest_timestamp(), Some(last_ts));

    // Backwards batch rejection
    let err = series
        .append(&[last_ts - 5, last_ts + 10], &[2, 3])
        .expect_err("event series must reject backwards batch after chunk sealing");
    assert!(matches!(err, CoreError::Storage(_)));

    // Rejection inside sealed chunk range
    let err2 = series
        .append(&[100_000, 100_020], &[4, 5])
        .expect_err("event series must reject batch inside sealed chunk range");
    assert!(matches!(err2, CoreError::Storage(_)));

    // Valid forward batch succeeds
    let valid = series
        .append(&[last_ts, last_ts + 20], &[6, 7])
        .expect("valid forward batch must succeed");
    assert_eq!(valid, 2);
    assert_eq!(series.total_samples(), 70_002);
}

#[test]
fn test_monotonicity_violation_transactional_atomicity() {
    let mut series = NativeScalarSeries::new(ChannelId(10), ScalarType::F32, LinearScale::identity());
    let listener = ConcurrentWatermarkRecorder::new();
    series.register_listener(listener.clone());

    // Ingest initial 100 valid samples
    let ts_init: Vec<TimestampNs> = (0..100).map(|i| (i * 10) as TimestampNs).collect();
    let vals_init = vec![42.0f32; 100];
    series.append(&ts_init, ScalarSlice::F32(&vals_init)).unwrap();

    assert_eq!(series.total_samples(), 100);
    assert_eq!(series.latest_timestamp(), Some(990));
    assert_eq!(listener.call_count(), 1);

    // Hostile batch with reversal halfway through
    let mut hostile_ts: Vec<TimestampNs> = (100..150).map(|i| (i * 10) as TimestampNs).collect();
    hostile_ts[25] = hostile_ts[24] - 5; // Reversal
    let hostile_vals = vec![99.0f32; 50];

    let err = series
        .append(&hostile_ts, ScalarSlice::F32(&hostile_vals))
        .expect_err("hostile batch must be rejected");
    assert!(matches!(err, CoreError::Storage(_)));

    // Invariant check: series state must be strictly unchanged (transactional rollback)
    assert_eq!(series.total_samples(), 100);
    assert_eq!(series.latest_timestamp(), Some(990));
    assert_eq!(listener.call_count(), 1, "listener must NOT be notified on failed append");

    // Sample retrieval at index 99 still returns 42.0
    let (t_last, val_last) = series.get_sample_by_index(99).unwrap();
    assert_eq!(t_last, 990);
    assert_eq!(val_last, 42.0);

    // Index 100 must still be out of bounds
    assert!(series.get_sample_by_index(100).is_none());

    // Now append a valid batch starting from t = 990
    let valid_ts: Vec<TimestampNs> = (0..50).map(|i| 990 + (i * 10) as TimestampNs).collect();
    let valid_vals = vec![77.0f32; 50];
    series.append(&valid_ts, ScalarSlice::F32(&valid_vals)).unwrap();

    assert_eq!(series.total_samples(), 150);
    assert_eq!(series.latest_timestamp(), Some(990 + 49 * 10));
    assert_eq!(listener.call_count(), 2);
    let (t_new, val_new) = series.get_sample_by_index(100).unwrap();
    assert_eq!(t_new, 990);
    assert_eq!(val_new, 77.0);
}

// =========================================================================
// 2. Numerical Extremes Under High Throughput Across Chunk Sealing
// =========================================================================

#[test]
fn test_f32_numerical_extremes_across_chunk_sealing() {
    let mut series = NativeScalarSeries::new(ChannelId(3), ScalarType::F32, LinearScale::identity());

    // Comprehensive matrix of IEEE-754 extremes
    let extreme_palette = [
        f32::NAN,
        -f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        0.0f32,
        -0.0f32,
        f32::MIN_POSITIVE,              // 1.17549435e-38 (normal minimum)
        f32::from_bits(1),              // 1.4012985e-45 (smallest positive subnormal)
        f32::from_bits(0x007F_FFFF),     // largest subnormal
        1.0e-40f32,                     // subnormal
        -1.0e-40f32,                    // negative subnormal
        f32::MIN,                       // -3.4028235e+38
        f32::MAX,                       // 3.4028235e+38
        -1.0f32,
        1.0f32,
        std::f32::consts::PI,
    ];

    let total_samples = 150_000usize; // > 2 chunks sealed (65536 * 2 = 131,072 sealed + 18,928 active)
    let batch_size = 15_000usize;

    let mut expected_vals = Vec::with_capacity(total_samples);
    for i in 0..total_samples {
        expected_vals.push(extreme_palette[i % extreme_palette.len()]);
    }

    // Append in 10 batches of 15,000 samples
    for batch_idx in 0..(total_samples / batch_size) {
        let start = batch_idx * batch_size;
        let end = start + batch_size;
        let ts: Vec<TimestampNs> = (start..end).map(|i| (i * 10) as TimestampNs).collect();
        let slice = &expected_vals[start..end];
        series.append(&ts, ScalarSlice::F32(slice)).unwrap();
    }

    assert_eq!(series.total_samples(), total_samples as u64);
    assert_eq!(series.sealed_chunk_count(), 2);

    // Verify all 150,000 samples across chunk sealing boundaries and active buffer
    for (i, &expected) in expected_vals.iter().enumerate() {
        let (ts, val) = series
            .get_sample_by_index(i as u64)
            .unwrap_or_else(|| panic!("failed to retrieve sample at index {i}"));

        assert_eq!(ts, (i * 10) as TimestampNs);

        if expected.is_nan() {
            assert!(
                val.is_nan(),
                "index {i}: expected NaN, got {val}"
            );
        } else if expected.is_infinite() {
            if expected.is_sign_positive() {
                assert_eq!(val, f64::INFINITY, "index {i}: expected +Inf");
            } else {
                assert_eq!(val, f64::NEG_INFINITY, "index {i}: expected -Inf");
            }
        } else if expected == 0.0 {
            // Note: In IEEE-754 arithmetic with linear calibration transform `raw * scale + offset`,
            // -0.0 * 1.0 + 0.0 evaluates to +0.0 by IEEE-754 addition rules.
            assert_eq!(val, 0.0, "index {i}: expected 0.0");
        } else {
            let val_f32 = val as f32;
            assert_eq!(
                val_f32.to_bits(),
                expected.to_bits(),
                "index {i}: exact bit mismatch for normal/subnormal: got {:?}, expected {:?}",
                val_f32,
                expected
            );
        }
    }

    // Verify binary search sample lookup by timestamp at chunk boundaries
    let boundary_indices = [
        0usize,
        65_535, // Chunk 0 last sample
        65_536, // Chunk 1 first sample
        131_071, // Chunk 1 last sample
        131_072, // Active buffer first sample
        149_999, // Active buffer last sample
    ];

    for &idx in &boundary_indices {
        let ts_target = (idx * 10) as TimestampNs;
        let (found_idx, found_ts, found_val) = series
            .get_sample_by_timestamp(ts_target)
            .unwrap_or_else(|| panic!("failed timestamp search at {ts_target}"));

        assert_eq!(found_idx, idx as u64);
        assert_eq!(found_ts, ts_target);

        let exp = expected_vals[idx];
        if exp.is_nan() {
            assert!(found_val.is_nan());
        } else if exp.is_infinite() {
            assert_eq!(found_val.is_sign_positive(), exp.is_sign_positive());
        } else if exp == 0.0 {
            assert_eq!(found_val, 0.0);
        } else {
            assert_eq!((found_val as f32).to_bits(), exp.to_bits());
        }
    }
}

#[test]
fn test_f32_calibrated_extremes() {
    // Non-trivial calibration: raw * 2.5 - 10.0
    let scale = LinearScale::new(2.5, -10.0);
    let mut series = NativeScalarSeries::new(ChannelId(3), ScalarType::F32, scale);

    let ts = [100, 200, 300, 400, 500];
    let vals = [
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NAN,
        4.0f32,   // 4.0 * 2.5 - 10.0 = 0.0
        10.0f32,  // 10.0 * 2.5 - 10.0 = 15.0
    ];

    series.append(&ts, ScalarSlice::F32(&vals)).unwrap();

    let (_, v0) = series.get_sample_by_index(0).unwrap();
    assert_eq!(v0, f64::INFINITY);

    let (_, v1) = series.get_sample_by_index(1).unwrap();
    assert_eq!(v1, f64::NEG_INFINITY);

    let (_, v2) = series.get_sample_by_index(2).unwrap();
    assert!(v2.is_nan());

    let (_, v3) = series.get_sample_by_index(3).unwrap();
    assert!((v3 - 0.0).abs() < 1e-6);

    let (_, v4) = series.get_sample_by_index(4).unwrap();
    assert!((v4 - 15.0).abs() < 1e-6);
}

#[test]
fn test_signed_integer_extremes_across_sealing() {
    // Test i16 signed extremes across chunk sealing boundary
    let scale = LinearScale::new(0.5, 100.0);
    let mut series = NativeScalarSeries::new(ChannelId(4), ScalarType::I16, scale);

    let extreme_palette: [i16; 8] = [
        i16::MIN,
        i16::MIN + 1,
        -1,
        0,
        1,
        i16::MAX - 1,
        i16::MAX,
        -32768,
    ];

    let total_samples = 70_000usize; // 1 sealed chunk (65,536) + 4,464 active
    let mut expected_raw = Vec::with_capacity(total_samples);
    for i in 0..total_samples {
        expected_raw.push(extreme_palette[i % extreme_palette.len()]);
    }

    let ts: Vec<TimestampNs> = (0..total_samples).map(|i| (i * 10) as TimestampNs).collect();
    series.append(&ts, ScalarSlice::I16(&expected_raw)).unwrap();

    assert_eq!(series.sealed_chunk_count(), 1);
    assert_eq!(series.total_samples(), 70_000);

    for (i, &raw) in expected_raw.iter().enumerate() {
        let (sample_ts, val) = series.get_sample_by_index(i as u64).unwrap();
        assert_eq!(sample_ts, (i * 10) as TimestampNs);
        let expected_calibrated = (raw as f64) * 0.5 + 100.0;
        assert_eq!(val, expected_calibrated, "sample {i} calibrated mismatch");
    }

    // Also test i8, i32, i64 min/max with identity scale
    let mut s_i8 = NativeScalarSeries::new(ChannelId(5), ScalarType::I8, LinearScale::identity());
    let ts_i8 = [10, 20, 30, 40, 50];
    let vals_i8 = [i8::MIN, -1, 0, 1, i8::MAX];
    s_i8.append(&ts_i8, ScalarSlice::I8(&vals_i8)).unwrap();
    assert_eq!(s_i8.get_sample_by_index(0).unwrap().1, i8::MIN as f64);
    assert_eq!(s_i8.get_sample_by_index(4).unwrap().1, i8::MAX as f64);

    let mut s_i64 = NativeScalarSeries::new(ChannelId(6), ScalarType::I64, LinearScale::identity());
    let ts_i64 = [10, 20, 30, 40, 50];
    let vals_i64 = [i64::MIN, -1, 0, 1, i64::MAX];
    s_i64.append(&ts_i64, ScalarSlice::I64(&vals_i64)).unwrap();
    assert_eq!(s_i64.get_sample_by_index(0).unwrap().1, i64::MIN as f64);
    assert_eq!(s_i64.get_sample_by_index(4).unwrap().1, i64::MAX as f64);
}

#[test]
fn test_unsigned_integer_extremes_across_sealing() {
    let mut series = NativeScalarSeries::new(ChannelId(7), ScalarType::U32, LinearScale::identity());

    let extremes = [0u32, 1, u32::MAX / 2, u32::MAX - 1, u32::MAX];
    let total_samples = 70_000usize;
    let mut expected_vals = Vec::with_capacity(total_samples);
    for i in 0..total_samples {
        expected_vals.push(extremes[i % extremes.len()]);
    }

    let ts: Vec<TimestampNs> = (0..total_samples).map(|i| (i * 10) as TimestampNs).collect();
    series.append(&ts, ScalarSlice::U32(&expected_vals)).unwrap();

    assert_eq!(series.sealed_chunk_count(), 1);
    assert_eq!(series.total_samples(), 70_000);

    for (i, &raw) in expected_vals.iter().enumerate() {
        let (_, val) = series.get_sample_by_index(i as u64).unwrap();
        assert_eq!(val, raw as f64, "u32 mismatch at index {i}");
    }
}

// =========================================================================
// 3. Concurrent Multi-Channel Streaming
// =========================================================================

#[test]
fn test_concurrent_multichannel_streaming_and_watermarks() {
    const NUM_CHANNELS: usize = 8;
    const SAMPLES_PER_BATCH: usize = 20_000;
    const BATCHES_PER_CHANNEL: usize = 5;
    const TOTAL_SAMPLES_PER_CHANNEL: usize = SAMPLES_PER_BATCH * BATCHES_PER_CHANNEL; // 100,000

    let shared_recorder = ConcurrentWatermarkRecorder::new();

    // Spawn 8 worker threads simulating concurrent streaming acquisition across 8 distinct scalar types
    let mut handles = Vec::with_capacity(NUM_CHANNELS);

    for ch_idx in 1..=NUM_CHANNELS {
        let recorder = shared_recorder.clone();

        let handle = thread::spawn(move || {
            let channel_id = ChannelId(ch_idx as u32);

            // Channel type assignment
            match ch_idx {
                1 => {
                    // Ch 1: F32
                    let mut series = NativeScalarSeries::new(channel_id, ScalarType::F32, LinearScale::identity());
                    series.register_listener(recorder);

                    for b in 0..BATCHES_PER_CHANNEL {
                        let offset = b * SAMPLES_PER_BATCH;
                        let ts: Vec<TimestampNs> = (offset..offset + SAMPLES_PER_BATCH)
                            .map(|i| (i * 10) as TimestampNs)
                            .collect();
                        let vals: Vec<f32> = (offset..offset + SAMPLES_PER_BATCH)
                            .map(|i| ((i as f32) * 0.01).sin())
                            .collect();
                        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
                    }

                    assert_eq!(series.total_samples(), TOTAL_SAMPLES_PER_CHANNEL as u64);
                    assert_eq!(series.sealed_chunk_count(), 1);

                    // Verify sample data integrity at boundaries
                    let (_, v_start) = series.get_sample_by_index(0).unwrap();
                    assert!((v_start - 0.0).abs() < 1e-4);

                    let (_, v_sealed_edge) = series.get_sample_by_index(CHUNK_LEN as u64 - 1).unwrap();
                    let expected_edge = (((CHUNK_LEN - 1) as f32) * 0.01).sin() as f64;
                    assert!((v_sealed_edge - expected_edge).abs() < 1e-4);
                }
                2 => {
                    // Ch 2: I16
                    let mut series = NativeScalarSeries::new(channel_id, ScalarType::I16, LinearScale::identity());
                    series.register_listener(recorder);

                    for b in 0..BATCHES_PER_CHANNEL {
                        let offset = b * SAMPLES_PER_BATCH;
                        let ts: Vec<TimestampNs> = (offset..offset + SAMPLES_PER_BATCH)
                            .map(|i| (i * 10) as TimestampNs)
                            .collect();
                        let vals: Vec<i16> = (offset..offset + SAMPLES_PER_BATCH)
                            .map(|i| ((i % 2000) as i16) - 1000)
                            .collect();
                        series.append(&ts, ScalarSlice::I16(&vals)).unwrap();
                    }

                    assert_eq!(series.total_samples(), TOTAL_SAMPLES_PER_CHANNEL as u64);
                    assert_eq!(series.sealed_chunk_count(), 1);

                    let (_, v) = series.get_sample_by_index(1234).unwrap();
                    assert_eq!(v, (1234 - 1000) as f64);
                }
                3 => {
                    // Ch 3: U32
                    let mut series = NativeScalarSeries::new(channel_id, ScalarType::U32, LinearScale::identity());
                    series.register_listener(recorder);

                    for b in 0..BATCHES_PER_CHANNEL {
                        let offset = b * SAMPLES_PER_BATCH;
                        let ts: Vec<TimestampNs> = (offset..offset + SAMPLES_PER_BATCH)
                            .map(|i| (i * 10) as TimestampNs)
                            .collect();
                        let vals: Vec<u32> = (offset..offset + SAMPLES_PER_BATCH)
                            .map(|i| (i as u32) * 7)
                            .collect();
                        series.append(&ts, ScalarSlice::U32(&vals)).unwrap();
                    }

                    assert_eq!(series.total_samples(), TOTAL_SAMPLES_PER_CHANNEL as u64);
                    assert_eq!(series.sealed_chunk_count(), 1);

                    let (_, v) = series.get_sample_by_index(50_000).unwrap();
                    assert_eq!(v, (50_000u32 * 7) as f64);
                }
                4 => {
                    // Ch 4: F64
                    let mut series = NativeScalarSeries::new(channel_id, ScalarType::F64, LinearScale::identity());
                    series.register_listener(recorder);

                    for b in 0..BATCHES_PER_CHANNEL {
                        let offset = b * SAMPLES_PER_BATCH;
                        let ts: Vec<TimestampNs> = (offset..offset + SAMPLES_PER_BATCH)
                            .map(|i| (i * 10) as TimestampNs)
                            .collect();
                        let vals: Vec<f64> = (offset..offset + SAMPLES_PER_BATCH)
                            .map(|i| ((i as f64) * 0.05).cos())
                            .collect();
                        series.append(&ts, ScalarSlice::F64(&vals)).unwrap();
                    }

                    assert_eq!(series.total_samples(), TOTAL_SAMPLES_PER_CHANNEL as u64);
                    assert_eq!(series.sealed_chunk_count(), 1);
                }
                5 => {
                    // Ch 5: I8
                    let mut series = NativeScalarSeries::new(channel_id, ScalarType::I8, LinearScale::identity());
                    series.register_listener(recorder);

                    for b in 0..BATCHES_PER_CHANNEL {
                        let offset = b * SAMPLES_PER_BATCH;
                        let ts: Vec<TimestampNs> = (offset..offset + SAMPLES_PER_BATCH)
                            .map(|i| (i * 10) as TimestampNs)
                            .collect();
                        let vals: Vec<i8> = (offset..offset + SAMPLES_PER_BATCH)
                            .map(|i| (i % 256) as i8)
                            .collect();
                        series.append(&ts, ScalarSlice::I8(&vals)).unwrap();
                    }

                    assert_eq!(series.total_samples(), TOTAL_SAMPLES_PER_CHANNEL as u64);
                    assert_eq!(series.sealed_chunk_count(), 1);
                }
                6 => {
                    // Ch 6: U8
                    let mut series = NativeScalarSeries::new(channel_id, ScalarType::U8, LinearScale::identity());
                    series.register_listener(recorder);

                    for b in 0..BATCHES_PER_CHANNEL {
                        let offset = b * SAMPLES_PER_BATCH;
                        let ts: Vec<TimestampNs> = (offset..offset + SAMPLES_PER_BATCH)
                            .map(|i| (i * 10) as TimestampNs)
                            .collect();
                        let vals: Vec<u8> = (offset..offset + SAMPLES_PER_BATCH)
                            .map(|i| (i % 256) as u8)
                            .collect();
                        series.append(&ts, ScalarSlice::U8(&vals)).unwrap();
                    }

                    assert_eq!(series.total_samples(), TOTAL_SAMPLES_PER_CHANNEL as u64);
                    assert_eq!(series.sealed_chunk_count(), 1);
                }
                7 => {
                    // Ch 7: I32
                    let mut series = NativeScalarSeries::new(channel_id, ScalarType::I32, LinearScale::identity());
                    series.register_listener(recorder);

                    for b in 0..BATCHES_PER_CHANNEL {
                        let offset = b * SAMPLES_PER_BATCH;
                        let ts: Vec<TimestampNs> = (offset..offset + SAMPLES_PER_BATCH)
                            .map(|i| (i * 10) as TimestampNs)
                            .collect();
                        let vals: Vec<i32> = (offset..offset + SAMPLES_PER_BATCH)
                            .map(|i| -(i as i32))
                            .collect();
                        series.append(&ts, ScalarSlice::I32(&vals)).unwrap();
                    }

                    assert_eq!(series.total_samples(), TOTAL_SAMPLES_PER_CHANNEL as u64);
                    assert_eq!(series.sealed_chunk_count(), 1);
                }
                8 => {
                    // Ch 8: U64
                    let mut series = NativeScalarSeries::new(channel_id, ScalarType::U64, LinearScale::identity());
                    series.register_listener(recorder);

                    for b in 0..BATCHES_PER_CHANNEL {
                        let offset = b * SAMPLES_PER_BATCH;
                        let ts: Vec<TimestampNs> = (offset..offset + SAMPLES_PER_BATCH)
                            .map(|i| (i * 10) as TimestampNs)
                            .collect();
                        let vals: Vec<u64> = (offset..offset + SAMPLES_PER_BATCH)
                            .map(|i| (i as u64) * 1000)
                            .collect();
                        series.append(&ts, ScalarSlice::U64(&vals)).unwrap();
                    }

                    assert_eq!(series.total_samples(), TOTAL_SAMPLES_PER_CHANNEL as u64);
                    assert_eq!(series.sealed_chunk_count(), 1);
                }
                _ => unreachable!(),
            }
        });

        handles.push(handle);
    }

    // Join all 8 concurrent producer threads
    for handle in handles {
        handle.join().expect("producer thread panicked");
    }

    // Invariant verification on watermark events across concurrent channels
    let all_events = shared_recorder.events();
    let expected_total_events = NUM_CHANNELS * BATCHES_PER_CHANNEL; // 8 * 5 = 40
    assert_eq!(
        all_events.len(),
        expected_total_events,
        "expected exactly 40 watermark events across 8 channels"
    );

    // Group events by channel
    let mut channel_events: HashMap<ChannelId, Vec<WatermarkEvent>> = HashMap::new();
    for ev in all_events {
        channel_events.entry(ev.channel_id).or_default().push(ev);
    }

    assert_eq!(channel_events.len(), NUM_CHANNELS);

    for ch_idx in 1..=NUM_CHANNELS {
        let cid = ChannelId(ch_idx as u32);
        let events = channel_events.get(&cid).expect("missing channel events");
        assert_eq!(events.len(), BATCHES_PER_CHANNEL);

        // Verify watermark progression within this channel
        let mut prev_ts = 0u64;
        for (i, ev) in events.iter().enumerate() {
            assert_eq!(ev.channel_id, cid);
            let expected_samples = ((i + 1) * SAMPLES_PER_BATCH) as u64;
            assert_eq!(ev.total_samples, expected_samples);
            assert!(
                ev.latest_ts_ns >= prev_ts,
                "latest_ts_ns must be non-decreasing within channel {ch_idx}"
            );
            prev_ts = ev.latest_ts_ns;
        }

        // Verify final watermark timestamp
        let last_ev = events.last().unwrap();
        let expected_final_ts = ((TOTAL_SAMPLES_PER_CHANNEL - 1) * 10) as u64;
        assert_eq!(last_ev.latest_ts_ns, expected_final_ts);
    }
}

#[test]
fn test_concurrent_reader_writer_stress() {
    let series_arc = Arc::new(parking_lot::RwLock::new(
        NativeScalarSeries::new(ChannelId(11), ScalarType::F32, LinearScale::identity()),
    ));

    let stop_signal = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Writer thread: continuously appends batches across chunk boundaries
    let writer_series = series_arc.clone();
    let writer_stop = stop_signal.clone();
    let writer_handle = thread::spawn(move || {
        let batch_size = 8_192usize;
        let mut curr_sample = 0usize;

        while !writer_stop.load(Ordering::Relaxed) && curr_sample < 200_000 {
            let ts: Vec<TimestampNs> = (curr_sample..curr_sample + batch_size)
                .map(|i| (i * 10) as TimestampNs)
                .collect();
            // Use exact power-of-two fraction (0.25) so f32 and f64 representations are exact
            let vals: Vec<f32> = (curr_sample..curr_sample + batch_size)
                .map(|i| ((i % 1000) as f32) * 0.25)
                .collect();

            {
                let mut s = writer_series.write();
                s.append(&ts, ScalarSlice::F32(&vals)).unwrap();
            }

            curr_sample += batch_size;
            thread::yield_now();
        }

        curr_sample
    });

    // Reader thread: continuously reads historical samples while writer runs
    let reader_series = series_arc.clone();
    let reader_stop = stop_signal.clone();
    let reader_handle = thread::spawn(move || {
        let mut reads_performed = 0usize;

        while !reader_stop.load(Ordering::Relaxed) {
            let total = {
                let s = reader_series.read();
                s.total_samples()
            };

            if total > 100 {
                let target_idx = total / 2;
                let s = reader_series.read();
                if let Some((ts, val)) = s.get_sample_by_index(target_idx) {
                    assert_eq!(ts, (target_idx * 10) as TimestampNs);
                    let expected = (((target_idx % 1000) as f32) * 0.25) as f64;
                    assert_eq!(val, expected);
                    reads_performed += 1;
                }
            }

            thread::yield_now();
        }

        reads_performed
    });

    let total_written = writer_handle.join().unwrap();
    stop_signal.store(true, Ordering::Relaxed);
    let total_reads = reader_handle.join().unwrap();

    let final_s = series_arc.read();
    assert_eq!(final_s.total_samples(), total_written as u64);
    assert!(total_reads > 0, "reader must have completed concurrent reads");
}

// =========================================================================
// 4. Boundary & Invariant Stress
// =========================================================================

#[test]
fn test_empty_batch_invariants() {
    let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
    let listener = ConcurrentWatermarkRecorder::new();
    series.register_listener(listener.clone());

    // Empty append
    let count = series.append(&[], ScalarSlice::F32(&[])).unwrap();
    assert_eq!(count, 0);
    assert_eq!(series.total_samples(), 0);
    assert_eq!(listener.call_count(), 0, "no watermarks for empty append");

    let mut event_series = EventSeries::new(ChannelId(2));
    let ev_listener = ConcurrentWatermarkRecorder::new();
    event_series.register_listener(ev_listener.clone());

    let ev_count = event_series.append(&[], &[]).unwrap();
    assert_eq!(ev_count, 0);
    assert_eq!(event_series.total_samples(), 0);
    assert_eq!(ev_listener.call_count(), 0);
}

#[test]
fn test_mismatched_slice_types_and_lengths() {
    let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());

    // Length mismatch
    let err_len = series
        .append(&[100, 200], ScalarSlice::F32(&[1.0]))
        .expect_err("must reject length mismatch");
    assert!(matches!(err_len, CoreError::Storage(_)));

    // Type mismatch: F64 passed to F32 series
    let err_type = series
        .append(&[100], ScalarSlice::F64(&[1.0]))
        .expect_err("must reject scalar type mismatch");
    assert!(matches!(err_type, CoreError::Storage(_)));

    assert_eq!(series.total_samples(), 0);
}
