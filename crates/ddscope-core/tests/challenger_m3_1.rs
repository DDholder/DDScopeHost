//! Adversarial throughput and sustained streaming stress test suite for Milestone 3 (M3).
//!
//! Empirical challenge targets:
//! 1. Sustained continuous streaming ingestion: append 10,000,000 samples (`f32`, `i16`, `u32`)
//!    across 152+ automatic chunk sealing cycles (65,536 samples per chunk).
//! 2. Measure wall-clock throughput via `std::time::Instant` under `cargo test --release -p ddscope-core --test challenger_m3_1`
//!    and verify that sustained throughput strictly exceeds 10,000,000 samples/sec.
//! 3. Validate sample data integrity: verify that global index retrieval (`get_sample_by_index`)
//!    across sealed chunk boundaries and active buffer returns exact original values.
//! 4. Ensure zero clippy warnings and 100% test pass.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use ddscope_core::storage::{NativeScalarSeries, WatermarkEvent, WatermarkListener};
use ddscope_core::types::{
    ChannelId, LinearScale, ScalarSlice, ScalarType, TimestampNs, CHUNK_LEN,
};

/// Total number of samples for the sustained streaming stress test (10 million).
const TOTAL_STREAM_SAMPLES: usize = 10_000_000;

/// Standard frame size matching one full sealed chunk.
const CHUNK_BATCH_SIZE: usize = CHUNK_LEN; // 65,536

/// Expected number of sealed chunks after 10,000,000 samples (10,000,000 / 65,536 = 152).
const EXPECTED_SEALED_CHUNKS: usize = TOTAL_STREAM_SAMPLES / CHUNK_LEN; // 152

/// Expected remaining unsealed samples in the active buffer (10,000,000 % 65,536 = 38,528).
const EXPECTED_ACTIVE_SAMPLES: usize = TOTAL_STREAM_SAMPLES % CHUNK_LEN; // 38,528

/// Acceptance requirement threshold: 10,000,000 samples/sec.
const REQUIRED_THROUGHPUT_SA_PER_SEC: f64 = 10_000_000.0;

/// Sampling interval: 10 ns (nominal 100 MSa/s clock).
const SAMPLING_INTERVAL_NS: u64 = 10;

// =========================================================================
// 1. Sustained Continuous Streaming: f32 (IEEE-754 Single Precision)
// =========================================================================

#[test]
fn test_sustained_streaming_throughput_and_integrity_f32_10m() {
    println!("\n=== Starting 10,000,000 sample sustained streaming stress test (f32) ===");

    // Pre-generate ground truth data
    let timestamps: Vec<TimestampNs> = (0..TOTAL_STREAM_SAMPLES as u64)
        .map(|i| i * SAMPLING_INTERVAL_NS)
        .collect();
    // Using values that are exact binary fractions (multiples of 0.25)
    let values: Vec<f32> = (0..TOTAL_STREAM_SAMPLES)
        .map(|i| ((i % 10_000) as f32) * 0.25)
        .collect();

    let mut series = NativeScalarSeries::new(
        ChannelId(101),
        ScalarType::F32,
        LinearScale::identity(),
    );

    // Wall-clock timed continuous streaming ingestion
    let start_time = Instant::now();
    let mut offset = 0;
    while offset < TOTAL_STREAM_SAMPLES {
        let batch_len = (TOTAL_STREAM_SAMPLES - offset).min(CHUNK_BATCH_SIZE);
        let ts_slice = &timestamps[offset..offset + batch_len];
        let val_slice = ScalarSlice::F32(&values[offset..offset + batch_len]);

        let appended = series.append(ts_slice, val_slice).expect("f32 append failed");
        assert_eq!(appended, batch_len);
        offset += batch_len;
    }
    let elapsed = start_time.elapsed();

    // Verify stream invariants
    assert_eq!(series.total_samples(), TOTAL_STREAM_SAMPLES as u64);
    assert_eq!(series.sealed_chunk_count(), EXPECTED_SEALED_CHUNKS);
    assert_eq!(series.active_chunk().len(), EXPECTED_ACTIVE_SAMPLES);

    // Calculate throughput
    let elapsed_secs = elapsed.as_secs_f64();
    let throughput = (TOTAL_STREAM_SAMPLES as f64) / elapsed_secs;
    let msa_per_sec = throughput / 1_000_000.0;

    println!(
        "f32 Ingested: {} samples across {} sealed chunks + {} active samples in {:.4}s -> {:.2} MSa/s",
        TOTAL_STREAM_SAMPLES,
        series.sealed_chunk_count(),
        series.active_chunk().len(),
        elapsed_secs,
        msa_per_sec
    );

    // Enforce throughput requirement in release mode
    if !cfg!(debug_assertions) {
        assert!(
            throughput > REQUIRED_THROUGHPUT_SA_PER_SEC,
            "Sustained f32 throughput ({:.2} Sa/s) failed to meet required threshold ({:.2} Sa/s)",
            throughput,
            REQUIRED_THROUGHPUT_SA_PER_SEC
        );
    }

    // Adversarial Data Integrity Checks across Chunk Boundaries and Active Buffer
    let test_indices = [
        0,                                   // Chunk 0 start
        1,
        CHUNK_LEN - 1,                       // Chunk 0 end (65,535)
        CHUNK_LEN,                           // Chunk 1 start (65,536)
        CHUNK_LEN + 1,
        2 * CHUNK_LEN - 1,                   // Chunk 1 end
        2 * CHUNK_LEN,                       // Chunk 2 start
        50 * CHUNK_LEN - 1,                  // Chunk 49 end
        50 * CHUNK_LEN,                      // Chunk 50 start
        100 * CHUNK_LEN - 1,                 // Chunk 99 end
        100 * CHUNK_LEN,                     // Chunk 100 start
        151 * CHUNK_LEN - 1,                 // Chunk 150 end
        151 * CHUNK_LEN,                     // Chunk 151 start
        152 * CHUNK_LEN - 1,                 // Chunk 151 end (last sealed sample: 9,961,471)
        152 * CHUNK_LEN,                     // Active buffer start (sample: 9,961,472)
        152 * CHUNK_LEN + 1,                 // Active buffer second sample
        152 * CHUNK_LEN + 10_000,            // Active buffer mid
        TOTAL_STREAM_SAMPLES - 1,            // Active buffer end (sample: 9,999,999)
    ];

    for &idx in &test_indices {
        let (ts, val) = series
            .get_sample_by_index(idx as u64)
            .unwrap_or_else(|| panic!("Failed to retrieve sample at index {idx}"));
        let exp_ts = timestamps[idx];
        let exp_val = f64::from(values[idx]);

        assert_eq!(
            ts, exp_ts,
            "Timestamp mismatch at index {idx}: got {ts}, expected {exp_ts}"
        );
        assert_eq!(
            val, exp_val,
            "f32 value mismatch at index {idx}: got {val}, expected {exp_val}"
        );
    }

    // Out of bounds check
    assert!(series.get_sample_by_index(TOTAL_STREAM_SAMPLES as u64).is_none());
    assert!(series.get_sample_by_index(TOTAL_STREAM_SAMPLES as u64 + 1).is_none());
    assert!(series.get_sample_by_index(u64::MAX).is_none());
}

// =========================================================================
// 2. Sustained Continuous Streaming: i16 (16-bit Signed ADC Telemetry)
// =========================================================================

#[test]
fn test_sustained_streaming_throughput_and_integrity_i16_10m() {
    println!("\n=== Starting 10,000,000 sample sustained streaming stress test (i16) ===");

    let timestamps: Vec<TimestampNs> = (0..TOTAL_STREAM_SAMPLES as u64)
        .map(|i| i * SAMPLING_INTERVAL_NS)
        .collect();
    let values: Vec<i16> = (0..TOTAL_STREAM_SAMPLES)
        .map(|i| ((i % 32_767) as i16) - 16_384)
        .collect();

    let mut series = NativeScalarSeries::new(
        ChannelId(102),
        ScalarType::I16,
        LinearScale::identity(),
    );

    let start_time = Instant::now();
    let mut offset = 0;
    while offset < TOTAL_STREAM_SAMPLES {
        let batch_len = (TOTAL_STREAM_SAMPLES - offset).min(CHUNK_BATCH_SIZE);
        let ts_slice = &timestamps[offset..offset + batch_len];
        let val_slice = ScalarSlice::I16(&values[offset..offset + batch_len]);

        let appended = series.append(ts_slice, val_slice).expect("i16 append failed");
        assert_eq!(appended, batch_len);
        offset += batch_len;
    }
    let elapsed = start_time.elapsed();

    assert_eq!(series.total_samples(), TOTAL_STREAM_SAMPLES as u64);
    assert_eq!(series.sealed_chunk_count(), EXPECTED_SEALED_CHUNKS);
    assert_eq!(series.active_chunk().len(), EXPECTED_ACTIVE_SAMPLES);

    let elapsed_secs = elapsed.as_secs_f64();
    let throughput = (TOTAL_STREAM_SAMPLES as f64) / elapsed_secs;
    let msa_per_sec = throughput / 1_000_000.0;

    println!(
        "i16 Ingested: {} samples across {} sealed chunks + {} active samples in {:.4}s -> {:.2} MSa/s",
        TOTAL_STREAM_SAMPLES,
        series.sealed_chunk_count(),
        series.active_chunk().len(),
        elapsed_secs,
        msa_per_sec
    );

    if !cfg!(debug_assertions) {
        assert!(
            throughput > REQUIRED_THROUGHPUT_SA_PER_SEC,
            "Sustained i16 throughput ({:.2} Sa/s) failed to meet required threshold ({:.2} Sa/s)",
            throughput,
            REQUIRED_THROUGHPUT_SA_PER_SEC
        );
    }

    // Boundary data integrity
    let test_indices = [
        0,
        CHUNK_LEN - 1,
        CHUNK_LEN,
        2 * CHUNK_LEN - 1,
        2 * CHUNK_LEN,
        75 * CHUNK_LEN - 1,
        75 * CHUNK_LEN,
        152 * CHUNK_LEN - 1,
        152 * CHUNK_LEN,
        TOTAL_STREAM_SAMPLES - 1,
    ];

    for &idx in &test_indices {
        let (ts, val) = series
            .get_sample_by_index(idx as u64)
            .unwrap_or_else(|| panic!("Failed to retrieve i16 sample at index {idx}"));
        let exp_ts = timestamps[idx];
        let exp_val = f64::from(values[idx]);

        assert_eq!(ts, exp_ts, "Timestamp mismatch at index {idx}");
        assert_eq!(val, exp_val, "i16 value mismatch at index {idx}");
    }

    assert!(series.get_sample_by_index(TOTAL_STREAM_SAMPLES as u64).is_none());
}

// =========================================================================
// 3. Sustained Continuous Streaming: u32 (32-bit Bus Counter Telemetry)
// =========================================================================

#[test]
fn test_sustained_streaming_throughput_and_integrity_u32_10m() {
    println!("\n=== Starting 10,000,000 sample sustained streaming stress test (u32) ===");

    let timestamps: Vec<TimestampNs> = (0..TOTAL_STREAM_SAMPLES as u64)
        .map(|i| i * SAMPLING_INTERVAL_NS)
        .collect();
    let values: Vec<u32> = (0..TOTAL_STREAM_SAMPLES)
        .map(|i| (i as u32).wrapping_mul(31))
        .collect();

    let mut series = NativeScalarSeries::new(
        ChannelId(103),
        ScalarType::U32,
        LinearScale::identity(),
    );

    let start_time = Instant::now();
    let mut offset = 0;
    while offset < TOTAL_STREAM_SAMPLES {
        let batch_len = (TOTAL_STREAM_SAMPLES - offset).min(CHUNK_BATCH_SIZE);
        let ts_slice = &timestamps[offset..offset + batch_len];
        let val_slice = ScalarSlice::U32(&values[offset..offset + batch_len]);

        let appended = series.append(ts_slice, val_slice).expect("u32 append failed");
        assert_eq!(appended, batch_len);
        offset += batch_len;
    }
    let elapsed = start_time.elapsed();

    assert_eq!(series.total_samples(), TOTAL_STREAM_SAMPLES as u64);
    assert_eq!(series.sealed_chunk_count(), EXPECTED_SEALED_CHUNKS);
    assert_eq!(series.active_chunk().len(), EXPECTED_ACTIVE_SAMPLES);

    let elapsed_secs = elapsed.as_secs_f64();
    let throughput = (TOTAL_STREAM_SAMPLES as f64) / elapsed_secs;
    let msa_per_sec = throughput / 1_000_000.0;

    println!(
        "u32 Ingested: {} samples across {} sealed chunks + {} active samples in {:.4}s -> {:.2} MSa/s",
        TOTAL_STREAM_SAMPLES,
        series.sealed_chunk_count(),
        series.active_chunk().len(),
        elapsed_secs,
        msa_per_sec
    );

    if !cfg!(debug_assertions) {
        assert!(
            throughput > REQUIRED_THROUGHPUT_SA_PER_SEC,
            "Sustained u32 throughput ({:.2} Sa/s) failed to meet required threshold ({:.2} Sa/s)",
            throughput,
            REQUIRED_THROUGHPUT_SA_PER_SEC
        );
    }

    // Boundary data integrity
    let test_indices = [
        0,
        CHUNK_LEN - 1,
        CHUNK_LEN,
        2 * CHUNK_LEN - 1,
        2 * CHUNK_LEN,
        75 * CHUNK_LEN - 1,
        75 * CHUNK_LEN,
        152 * CHUNK_LEN - 1,
        152 * CHUNK_LEN,
        TOTAL_STREAM_SAMPLES - 1,
    ];

    for &idx in &test_indices {
        let (ts, val) = series
            .get_sample_by_index(idx as u64)
            .unwrap_or_else(|| panic!("Failed to retrieve u32 sample at index {idx}"));
        let exp_ts = timestamps[idx];
        let exp_val = f64::from(values[idx]);

        assert_eq!(ts, exp_ts, "Timestamp mismatch at index {idx}");
        assert_eq!(val, exp_val, "u32 value mismatch at index {idx}");
    }

    assert!(series.get_sample_by_index(TOTAL_STREAM_SAMPLES as u64).is_none());
}

// =========================================================================
// 4. Multi-Batch Rollover Stress: 1,000 Batches of 10,000 Samples
// =========================================================================

#[test]
fn test_sustained_streaming_unaligned_rollover_batches_10m() {
    println!("\n=== Starting multi-batch boundary rollover stress test (1,000 x 10,000) ===");

    const UNALIGNED_BATCH_SIZE: usize = 10_000;
    assert_eq!(TOTAL_STREAM_SAMPLES % UNALIGNED_BATCH_SIZE, 0);

    let timestamps: Vec<TimestampNs> = (0..TOTAL_STREAM_SAMPLES as u64)
        .map(|i| i * SAMPLING_INTERVAL_NS)
        .collect();
    let values: Vec<f32> = (0..TOTAL_STREAM_SAMPLES)
        .map(|i| (i as f32) * 0.5)
        .collect();

    let mut series = NativeScalarSeries::new(
        ChannelId(104),
        ScalarType::F32,
        LinearScale::identity(),
    );

    let start_time = Instant::now();
    let mut offset = 0;
    while offset < TOTAL_STREAM_SAMPLES {
        let ts_slice = &timestamps[offset..offset + UNALIGNED_BATCH_SIZE];
        let val_slice = ScalarSlice::F32(&values[offset..offset + UNALIGNED_BATCH_SIZE]);

        let appended = series
            .append(ts_slice, val_slice)
            .expect("unaligned batch append failed");
        assert_eq!(appended, UNALIGNED_BATCH_SIZE);
        offset += UNALIGNED_BATCH_SIZE;
    }
    let elapsed = start_time.elapsed();

    // Verification of resulting sealed chunk count and active buffer
    assert_eq!(series.total_samples(), TOTAL_STREAM_SAMPLES as u64);
    assert_eq!(series.sealed_chunk_count(), EXPECTED_SEALED_CHUNKS);
    assert_eq!(series.active_chunk().len(), EXPECTED_ACTIVE_SAMPLES);

    let elapsed_secs = elapsed.as_secs_f64();
    let throughput = (TOTAL_STREAM_SAMPLES as f64) / elapsed_secs;
    let msa_per_sec = throughput / 1_000_000.0;

    println!(
        "Unaligned (10,000): Ingested {} samples across 152 sealed boundaries in {:.4}s -> {:.2} MSa/s",
        TOTAL_STREAM_SAMPLES, elapsed_secs, msa_per_sec
    );

    if !cfg!(debug_assertions) {
        assert!(
            throughput > REQUIRED_THROUGHPUT_SA_PER_SEC,
            "Unaligned rollover throughput ({:.2} Sa/s) failed to meet required threshold ({:.2} Sa/s)",
            throughput,
            REQUIRED_THROUGHPUT_SA_PER_SEC
        );
    }

    // Verify samples across chunks
    for chunk_idx in 0..EXPECTED_SEALED_CHUNKS {
        let first_idx = chunk_idx * CHUNK_LEN;
        let last_idx = first_idx + CHUNK_LEN - 1;

        let (ts_first, val_first) = series.get_sample_by_index(first_idx as u64).unwrap();
        assert_eq!(ts_first, timestamps[first_idx]);
        assert_eq!(val_first, f64::from(values[first_idx]));

        let (ts_last, val_last) = series.get_sample_by_index(last_idx as u64).unwrap();
        assert_eq!(ts_last, timestamps[last_idx]);
        assert_eq!(val_last, f64::from(values[last_idx]));
    }

    // Verify active buffer
    let active_first = EXPECTED_SEALED_CHUNKS * CHUNK_LEN;
    let active_last = TOTAL_STREAM_SAMPLES - 1;
    let (ts_af, val_af) = series.get_sample_by_index(active_first as u64).unwrap();
    assert_eq!(ts_af, timestamps[active_first]);
    assert_eq!(val_af, f64::from(values[active_first]));

    let (ts_al, val_al) = series.get_sample_by_index(active_last as u64).unwrap();
    assert_eq!(ts_al, timestamps[active_last]);
    assert_eq!(val_al, f64::from(values[active_last]));
}

// =========================================================================
// 5. Deep Scan across All 152 Sealed Chunk Boundaries & Active Buffer
// =========================================================================

#[test]
fn test_adversarial_boundary_and_timestamp_indexing_152_chunks() {
    const BASE_TS_NS: TimestampNs = 1_000_000_000; // 1 second epoch offset

    let timestamps: Vec<TimestampNs> = (0..TOTAL_STREAM_SAMPLES as u64)
        .map(|i| BASE_TS_NS + i * SAMPLING_INTERVAL_NS)
        .collect();
    let values: Vec<u32> = (0..TOTAL_STREAM_SAMPLES)
        .map(|i| (i as u32) ^ 0x5555_AAAA)
        .collect();

    let mut series = NativeScalarSeries::new(
        ChannelId(105),
        ScalarType::U32,
        LinearScale::identity(),
    );

    let mut offset = 0;
    while offset < TOTAL_STREAM_SAMPLES {
        let batch_len = (TOTAL_STREAM_SAMPLES - offset).min(CHUNK_BATCH_SIZE);
        series
            .append(
                &timestamps[offset..offset + batch_len],
                ScalarSlice::U32(&values[offset..offset + batch_len]),
            )
            .expect("append failed");
        offset += batch_len;
    }

    assert_eq!(series.sealed_chunk_count(), 152);
    assert_eq!(series.active_chunk().len(), 38_528);

    // Deep check every sealed chunk boundary (first, middle, last sample)
    for c in 0..152 {
        let base = c * CHUNK_LEN;
        let mid = base + CHUNK_LEN / 2;
        let last = base + CHUNK_LEN - 1;

        for &idx in &[base, mid, last] {
            let (ts, val) = series
                .get_sample_by_index(idx as u64)
                .unwrap_or_else(|| panic!("Chunk {c} index {idx} failed"));
            assert_eq!(ts, timestamps[idx]);
            assert_eq!(val, f64::from(values[idx]));

            // Timestamp query check
            let (found_idx, found_ts, found_val) = series
                .get_sample_by_timestamp(ts)
                .unwrap_or_else(|| panic!("Timestamp query failed at chunk {c} ts {ts}"));
            assert_eq!(found_idx, idx as u64);
            assert_eq!(found_ts, ts);
            assert_eq!(found_val, val);
        }
    }

    // Check active buffer start, mid, end
    let active_base = 152 * CHUNK_LEN;
    let active_mid = active_base + 38_528 / 2;
    let active_end = TOTAL_STREAM_SAMPLES - 1;

    for &idx in &[active_base, active_mid, active_end] {
        let (ts, val) = series
            .get_sample_by_index(idx as u64)
            .unwrap_or_else(|| panic!("Active buffer index {idx} failed"));
        assert_eq!(ts, timestamps[idx]);
        assert_eq!(val, f64::from(values[idx]));

        let (found_idx, found_ts, found_val) = series
            .get_sample_by_timestamp(ts)
            .unwrap_or_else(|| panic!("Timestamp query failed in active buffer ts {ts}"));
            assert_eq!(found_idx, idx as u64);
            assert_eq!(found_ts, ts);
            assert_eq!(found_val, val);
    }

    // Series time range invariants
    assert_eq!(series.first_timestamp(), Some(timestamps[0]));
    assert_eq!(
        series.latest_timestamp(),
        Some(timestamps[TOTAL_STREAM_SAMPLES - 1])
    );
    assert_eq!(
        series.time_range_ns(),
        Some((timestamps[0], timestamps[TOTAL_STREAM_SAMPLES - 1]))
    );

    // Timestamp out of range queries
    assert!(series.get_sample_by_timestamp(timestamps[0] - 1).is_none());
    assert!(series
        .get_sample_by_timestamp(timestamps[TOTAL_STREAM_SAMPLES - 1] + 1)
        .is_none());
}

// =========================================================================
// 6. Sustained Streaming with Active Watermark Listener Fanout
// =========================================================================

struct FastWatermarkTracker {
    call_count: AtomicUsize,
    last_samples: AtomicU64,
    last_ts_ns: AtomicU64,
}

impl FastWatermarkTracker {
    fn new() -> Self {
        Self {
            call_count: AtomicUsize::new(0),
            last_samples: AtomicU64::new(0),
            last_ts_ns: AtomicU64::new(0),
        }
    }
}

impl WatermarkListener for FastWatermarkTracker {
    fn on_watermark_advanced(&self, events: &[WatermarkEvent]) {
        self.call_count.fetch_add(1, Ordering::Relaxed);
        if let Some(last) = events.last() {
            self.last_samples
                .store(last.total_samples, Ordering::Relaxed);
            self.last_ts_ns
                .store(last.latest_ts_ns, Ordering::Relaxed);
        }
    }
}

#[test]
fn test_sustained_streaming_with_active_watermark_listener_10m() {
    println!("\n=== Starting 10M streaming test with active watermark listener ===");

    let timestamps: Vec<TimestampNs> = (0..TOTAL_STREAM_SAMPLES as u64)
        .map(|i| i * SAMPLING_INTERVAL_NS)
        .collect();
    let values: Vec<f32> = (0..TOTAL_STREAM_SAMPLES)
        .map(|i| ((i % 5_000) as f32) * 0.125)
        .collect();

    let mut series = NativeScalarSeries::new(
        ChannelId(106),
        ScalarType::F32,
        LinearScale::identity(),
    );

    let tracker = Arc::new(FastWatermarkTracker::new());
    series.register_listener(tracker.clone());

    let start_time = Instant::now();
    let mut offset = 0;
    let mut batch_count = 0;
    while offset < TOTAL_STREAM_SAMPLES {
        let batch_len = (TOTAL_STREAM_SAMPLES - offset).min(CHUNK_BATCH_SIZE);
        series
            .append(
                &timestamps[offset..offset + batch_len],
                ScalarSlice::F32(&values[offset..offset + batch_len]),
            )
            .expect("append failed");
        offset += batch_len;
        batch_count += 1;
    }
    let elapsed = start_time.elapsed();

    // Verify watermark invocations
    assert_eq!(tracker.call_count.load(Ordering::SeqCst), batch_count);
    assert_eq!(
        tracker.last_samples.load(Ordering::SeqCst),
        TOTAL_STREAM_SAMPLES as u64
    );
    assert_eq!(
        tracker.last_ts_ns.load(Ordering::SeqCst),
        timestamps[TOTAL_STREAM_SAMPLES - 1]
    );

    let elapsed_secs = elapsed.as_secs_f64();
    let throughput = (TOTAL_STREAM_SAMPLES as f64) / elapsed_secs;
    let msa_per_sec = throughput / 1_000_000.0;

    println!(
        "Watermark Ingested: {} samples with {} callbacks in {:.4}s -> {:.2} MSa/s",
        TOTAL_STREAM_SAMPLES, batch_count, elapsed_secs, msa_per_sec
    );

    if !cfg!(debug_assertions) {
        assert!(
            throughput > REQUIRED_THROUGHPUT_SA_PER_SEC,
            "Throughput with watermark ({:.2} Sa/s) failed to meet required threshold ({:.2} Sa/s)",
            throughput,
            REQUIRED_THROUGHPUT_SA_PER_SEC
        );
    }
}
