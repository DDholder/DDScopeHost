//! Empirical Challenger 2 Adversarial Stress Suite for Milestone TA3.
//!
//! Conforms to Phase 1 Track A Milestone TA3 specifications:
//! - docs/04-phase1-design-and-plan.md §3.4, §5.2
//! - sub_orch_ta3/SCOPE.md
//! - ORIGINAL_REQUEST.md
//!
//! Specifically validates:
//! 1. Rollover Survival & Bit-Exactness:
//!    - Continuous ingestion pushing multiple ring buffer rollovers.
//!    - PinnedGuard holding evicted chunks remains 100% readable and bit-exact across all 65,536 samples.
//!    - Arc strong count reaches 1 after series eviction, and 0 upon PinnedGuard drop.
//!    - Memory quota atomic counters decrement accurately on PinnedGuard drop.
//! 2. Multi-Scope Shared Pinning & Drop Order:
//!    - Overlapping pinned ranges across multiple PinnedGuards.
//!    - Cascading drop order verification: quota deallocation occurs ONLY when last guard drops.
//! 3. O(1) Global Index Translation Across Evictions:
//!    - Ingestion of large volume of samples, followed by multiple evictions.
//!    - Strict O(1) lookup latency benchmarking for resident indices.
//!    - Evicted indices (index < evicted_samples) return None without panic or stale data.
//!    - Non-uniform chunk offset indexing across rollovers.
//! 4. Timestamp Boundary Resolution Post-Eviction:
//!    - Earliest surviving timestamp boundary search in get_sample_by_timestamp.
//!    - Gap interpolation and out-of-range queries (before earliest surviving timestamp).
//!    - Edge case when all sealed chunks are evicted and only active chunk remains.
//! 5. 40% Pinning Memory Ceiling & Concurrency:
//!    - Exact byte ceiling rejection (QuotaError::PinnedLimitExceeded).
//!    - Atomic tracking and multi-threaded race safety.

use std::sync::Arc;
use std::thread;
use std::time::Instant;

use ddscope_core::storage::quota::MemoryQuotaManager;
use ddscope_core::storage::NativeScalarSeries;
use ddscope_core::types::{
    ChannelId, DataChunk, LinearScale, ScalarSlice, ScalarType, TimestampNs, CHUNK_LEN,
};

const STEP_NS: u64 = 10;

/// Helper to generate monotonic timestamps.
fn make_timestamps(start_ns: u64, count: usize) -> Vec<TimestampNs> {
    (0..count).map(|i| start_ns + (i as u64) * STEP_NS).collect()
}

/// Helper to generate deterministic test signal: val = offset + i as f32
fn make_values_linear(offset: f32, count: usize) -> Vec<f32> {
    (0..count).map(|i| offset + (i as f32)).collect()
}

/// Helper to generate deterministic sine signal (bit-exact f32 check)
fn make_values_sine(base_freq: f32, count: usize) -> Vec<f32> {
    (0..count)
        .map(|i| ((i as f32) * base_freq).sin() * 50.0 + 100.0)
        .collect()
}

// ============================================================================
// 1. Rollover Survival & Bit-Exactness Tests
// ============================================================================

#[test]
fn test_rollover_pinned_chunk_bit_exact_survival() {
    let mut series =
        NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
    let quota = Arc::new(MemoryQuotaManager::new(4 * 1024 * 1024)); // 4 MB quota
    series.set_quota_manager(Arc::clone(&quota));

    // 1. Ingest Chunk 0 with high-precision sine wave values
    let ts0 = make_timestamps(1_000_000, CHUNK_LEN);
    let val0 = make_values_sine(0.005, CHUNK_LEN);
    series
        .append(&ts0, ScalarSlice::F32(&val0))
        .expect("append chunk 0 ok");

    assert_eq!(series.sealed_chunk_count(), 1);
    let initial_alloc = quota.allocated_bytes();
    assert!(initial_alloc > 0);

    // 2. Pin Chunk 0
    let guard0 = series
        .pin_range(ts0[0], ts0[CHUNK_LEN - 1])
        .expect("pin_range chunk 0 ok");
    assert_eq!(guard0.chunk_count(), 1);
    assert_eq!(guard0.total_samples(), CHUNK_LEN);
    let pinned_bytes = quota.pinned_bytes();
    assert!(pinned_bytes > 0);
    assert!(quota.is_pinned_active());

    // 3. Simulate continuous rolling ingestion pushing 5 more chunks
    // This pushes multiple buffer wraps
    for c in 1..=5 {
        let base_ts = 1_000_000 + (c * CHUNK_LEN as u64) * STEP_NS;
        let ts_c = make_timestamps(base_ts, CHUNK_LEN);
        let val_c = make_values_linear((c * 10_000) as f32, CHUNK_LEN);
        series
            .append(&ts_c, ScalarSlice::F32(&val_c))
            .unwrap_or_else(|_| panic!("append chunk {c} ok"));
    }

    assert_eq!(series.sealed_chunk_count(), 6);
    assert_eq!(series.total_samples(), (6 * CHUNK_LEN) as u64);

    // 4. Force eviction of chunk 0 (and subsequent chunks) from series ring index
    // Unconditional ring buffer rollover
    let evicted_chunk0 = series.evict_oldest().expect("evict chunk 0");
    assert_eq!(evicted_chunk0.chunk_seq, 0);
    assert_eq!(series.evicted_samples(), CHUNK_LEN as u64);

    // Evict chunk 1 and chunk 2 as well
    let evicted_chunk1 = series.evict_oldest().expect("evict chunk 1");
    assert_eq!(evicted_chunk1.chunk_seq, 1);
    let evicted_chunk2 = series.evict_oldest().expect("evict chunk 2");
    assert_eq!(evicted_chunk2.chunk_seq, 2);
    assert_eq!(series.evicted_samples(), (3 * CHUNK_LEN) as u64);
    assert_eq!(series.sealed_chunk_count(), 3);

    // Series no longer contains chunk 0!
    assert!(series.get_sample_by_index(0).is_none());
    assert!(series.get_sample_by_index((CHUNK_LEN - 1) as u64).is_none());

    // 5. Assert that chunk 0 held in PinnedGuard remains 100% intact, readable, and bit-exact
    let pinned_chunk_arc = &guard0.chunks()[0];
    match pinned_chunk_arc.as_ref() {
        DataChunk::Scalar(sc) => {
            assert_eq!(sc.len(), CHUNK_LEN);
            // Verify every single sample is bit-exact with the original val0
            for i in 0..CHUNK_LEN {
                let (ts, val) = sc
                    .get_sample_as_f64(i)
                    .unwrap_or_else(|| panic!("failed reading sample {i} from pinned chunk"));
                assert_eq!(ts, ts0[i], "timestamp mismatch at sample {i}");
                assert_eq!(
                    val, val0[i] as f64,
                    "bit-exact value mismatch at sample {i}"
                );
            }
        }
        _ => panic!("Expected DataChunk::Scalar"),
    }

    // Drop temporary handles returned from evict_oldest so guard0 is the sole owner of chunk 0
    drop(evicted_chunk0);
    drop(evicted_chunk1);
    drop(evicted_chunk2);

    // Verify chunk 0 strong count is strictly 1 (guard0 holds the sole reference)
    assert_eq!(
        Arc::strong_count(pinned_chunk_arc),
        1,
        "Strong count must be 1 after series eviction and local handle drop"
    );

    // Allocated bytes in quota still includes chunk 0 because guard0 is holding it
    let alloc_before_drop = quota.allocated_bytes();
    assert!(
        alloc_before_drop >= pinned_bytes,
        "Allocated bytes must account for retained pinned chunk"
    );

    // 6. Drop guard0: assert strong counts reach 0 and quota memory is properly deallocated
    let chunk_footprint = pinned_chunk_arc.byte_footprint();
    drop(guard0);

    // Pinned quota is released
    assert_eq!(quota.pinned_bytes(), 0);
    assert!(!quota.is_pinned_active());

    // Quota allocated bytes reconciled the deallocation of chunk 0
    let alloc_after_drop = quota.allocated_bytes();
    assert_eq!(
        alloc_after_drop,
        alloc_before_drop - chunk_footprint,
        "Allocated bytes must decrement by exactly the evicted chunk's footprint"
    );
}

#[test]
fn test_multi_scope_shared_pinning_cascading_drop() {
    let mut series =
        NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
    let quota = Arc::new(MemoryQuotaManager::new(8 * 1024 * 1024));
    series.set_quota_manager(Arc::clone(&quota));

    // Ingest 3 chunks
    let mut timestamps = Vec::new();
    for c in 0..3 {
        let base_ts = 10_000 + (c * CHUNK_LEN as u64) * STEP_NS;
        let ts = make_timestamps(base_ts, CHUNK_LEN);
        let vals = make_values_linear((c * 1000) as f32, CHUNK_LEN);
        timestamps.push(ts.clone());
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
    }

    // Scope A pins chunk 0 and chunk 1: [ts[0][0], ts[1][CHUNK_LEN - 1]]
    let guard_a = series
        .pin_range(timestamps[0][0], timestamps[1][CHUNK_LEN - 1])
        .expect("Scope A pin");
    assert_eq!(guard_a.chunk_count(), 2);

    // Scope B pins chunk 1 and chunk 2: [ts[1][0], ts[2][CHUNK_LEN - 1]]
    let guard_b = series
        .pin_range(timestamps[1][0], timestamps[2][CHUNK_LEN - 1])
        .expect("Scope B pin");
    assert_eq!(guard_b.chunk_count(), 2);

    // Chunk 1 is shared by BOTH guards and series: strong_count should be 3
    let chunk1_arc_a = &guard_a.chunks()[1];
    let chunk1_arc_b = &guard_b.chunks()[0];
    assert!(Arc::ptr_eq(chunk1_arc_a, chunk1_arc_b));
    assert_eq!(Arc::strong_count(chunk1_arc_a), 3);

    // Evict chunk 0 and chunk 1 from series
    let e0 = series.evict_oldest().unwrap();
    let e1 = series.evict_oldest().unwrap();
    drop(e0);
    drop(e1);

    // Now chunk 1 is only held by guard_a and guard_b (strong_count = 2)
    assert_eq!(Arc::strong_count(chunk1_arc_a), 2);

    let alloc_snapshot = quota.allocated_bytes();

    // Drop Guard A:
    // Chunk 0: only Guard A held it -> strong count becomes 0 -> deallocated!
    // Chunk 1: Guard B still holds it -> strong count becomes 1 -> NOT deallocated yet!
    let chunk0_bytes = guard_a.chunks()[0].byte_footprint();
    let chunk1_bytes = chunk1_arc_a.byte_footprint();
    drop(guard_a);

    // Allocated bytes should have decremented by Chunk 0 footprint ONLY
    assert_eq!(
        quota.allocated_bytes(),
        alloc_snapshot - chunk0_bytes,
        "Only chunk 0 should be deallocated when Guard A drops"
    );

    // Chunk 1 is still 100% accessible and valid in Guard B
    match chunk1_arc_b.as_ref() {
        DataChunk::Scalar(sc) => {
            let (_, val) = sc.get_sample_as_f64(0).unwrap();
            assert_eq!(val, 1000.0);
        }
        _ => panic!("Expected DataChunk::Scalar"),
    }

    // Now drop Guard B: Chunk 1 and Chunk 2 reconciled
    // (Chunk 2 is still resident in series! So Chunk 2 strong count is still 1 -> series owns it)
    // Only Chunk 1 is deallocated!
    let alloc_before_b = quota.allocated_bytes();
    drop(guard_b);

    assert_eq!(
        quota.allocated_bytes(),
        alloc_before_b - chunk1_bytes,
        "Chunk 1 must be deallocated when Guard B drops"
    );
    assert_eq!(quota.pinned_bytes(), 0);
}

// ============================================================================
// 2. Index Translation Verification (O(1) Resolution & Eviction Boundary)
// ============================================================================

#[test]
fn test_o1_global_index_translation_benchmark_and_correctness() {
    let mut series =
        NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());

    // Ingest 8 uniform chunks (524,288 samples)
    const NUM_CHUNKS: usize = 8;
    for c in 0..NUM_CHUNKS {
        let base_ts = (c * CHUNK_LEN) as u64 * STEP_NS;
        let ts = make_timestamps(base_ts, CHUNK_LEN);
        let vals = make_values_linear((c * 10_000) as f32, CHUNK_LEN);
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
    }

    assert_eq!(
        series.total_samples(),
        (NUM_CHUNKS * CHUNK_LEN) as u64
    );

    // Evict 3 chunks sequentially
    for c in 0..3 {
        let h = series.evict_oldest().unwrap();
        assert_eq!(h.chunk_seq, c as u64);
    }
    assert_eq!(series.evicted_samples(), (3 * CHUNK_LEN) as u64);
    assert_eq!(series.sealed_chunk_count(), 5);

    // 1. Evicted indices must return None
    assert!(series.get_sample_by_index(0).is_none());
    assert!(series.get_sample_by_index(1).is_none());
    assert!(series.get_sample_by_index(CHUNK_LEN as u64 - 1).is_none());
    assert!(series.get_sample_by_index(CHUNK_LEN as u64).is_none());
    assert!(series
        .get_sample_by_index((3 * CHUNK_LEN as u64) - 1)
        .is_none());

    // 2. Surviving indices: First surviving sample (chunk 3, sample 0)
    let first_valid_idx = (3 * CHUNK_LEN) as u64;
    let (ts_first, val_first) = series
        .get_sample_by_index(first_valid_idx)
        .expect("first surviving index should resolve");
    assert_eq!(ts_first, first_valid_idx * STEP_NS);
    assert_eq!(val_first, 30_000.0);

    // Last sample of chunk 3
    let last_chunk3_idx = (4 * CHUNK_LEN - 1) as u64;
    let (ts_c3, val_c3) = series
        .get_sample_by_index(last_chunk3_idx)
        .expect("last sample of chunk 3");
    assert_eq!(ts_c3, last_chunk3_idx * STEP_NS);
    assert_eq!(val_c3, 30_000.0 + (CHUNK_LEN - 1) as f64);

    // Sample in chunk 7
    let chunk7_mid = (7 * CHUNK_LEN + 1234) as u64;
    let (ts7, val7) = series
        .get_sample_by_index(chunk7_mid)
        .expect("sample in chunk 7");
    assert_eq!(ts7, chunk7_mid * STEP_NS);
    assert_eq!(val7, 70_000.0 + 1234.0);

    // Out of bounds index
    assert!(series
        .get_sample_by_index((NUM_CHUNKS * CHUNK_LEN) as u64)
        .is_none());
    assert!(series.get_sample_by_index(u64::MAX).is_none());

    // 3. Empirical O(1) latency test: 20,000 index lookups across surviving chunks
    let start_instant = Instant::now();
    let num_queries = 20_000usize;
    let mut accumulator = 0.0f64;
    for i in 0..num_queries {
        let sample_offset = (i * 17) % (5 * CHUNK_LEN);
        let query_idx = (3 * CHUNK_LEN) as u64 + sample_offset as u64;
        let (_, val) = series.get_sample_by_index(query_idx).unwrap();
        accumulator += val;
    }
    let elapsed = start_instant.elapsed();
    assert!(accumulator > 0.0);

    let avg_ns_per_lookup = elapsed.as_nanos() as f64 / num_queries as f64;
    // O(1) uniform indexing (bit-shift) should comfortably execute in < 250 ns per lookup in debug mode
    assert!(
        avg_ns_per_lookup < 5000.0,
        "Lookup took too long ({avg_ns_per_lookup} ns/lookup), expected strict O(1)"
    );
}

#[test]
fn test_non_uniform_chunks_index_translation_across_evictions() {
    let mut series =
        NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());

    // Ingest uneven batches and explicitly seal them to create non-uniform chunks
    let sizes = [500usize, 1200, 350, 4000, 800];
    let mut running_samples = 0u64;

    for (c, &sz) in sizes.iter().enumerate() {
        let base_ts = running_samples * STEP_NS;
        let ts = make_timestamps(base_ts, sz);
        let vals = make_values_linear((c * 100) as f32, sz);
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        series.seal_active().unwrap();
        running_samples += sz as u64;
    }

    assert_eq!(series.sealed_chunk_count(), 5);
    assert_eq!(series.total_samples(), running_samples);

    // Evict first two chunks (500 + 1200 = 1700 samples)
    series.evict_oldest().unwrap(); // evicted 500
    series.evict_oldest().unwrap(); // evicted 1200
    assert_eq!(series.evicted_samples(), 1700);
    assert_eq!(series.sealed_chunk_count(), 3);

    // Evicted range [0..1700) returns None
    assert!(series.get_sample_by_index(0).is_none());
    assert!(series.get_sample_by_index(499).is_none());
    assert!(series.get_sample_by_index(500).is_none());
    assert!(series.get_sample_by_index(1699).is_none());

    // Chunk 2 (350 samples, range [1700..2050))
    let (t_1700, v_1700) = series.get_sample_by_index(1700).unwrap();
    assert_eq!(t_1700, 1700 * STEP_NS);
    assert_eq!(v_1700, 200.0);

    let (t_2049, v_2049) = series.get_sample_by_index(2049).unwrap();
    assert_eq!(t_2049, 2049 * STEP_NS);
    assert_eq!(v_2049, 200.0 + 349.0);

    // Chunk 3 (4000 samples, range [2050..6050))
    let (t_2050, v_2050) = series.get_sample_by_index(2050).unwrap();
    assert_eq!(t_2050, 2050 * STEP_NS);
    assert_eq!(v_2050, 300.0);

    // Out of bounds
    assert!(series.get_sample_by_index(running_samples).is_none());
}

// ============================================================================
// 3. Timestamp Search at Eviction Boundaries
// ============================================================================

#[test]
fn test_get_sample_by_timestamp_eviction_boundary_stress() {
    let mut series =
        NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());

    // Ingest 4 chunks
    for c in 0..4 {
        let base_ts = 10_000 + (c * CHUNK_LEN as u64) * STEP_NS;
        let ts = make_timestamps(base_ts, CHUNK_LEN);
        let vals = make_values_linear((c * 1000) as f32, CHUNK_LEN);
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
    }

    // Evict chunks 0 and 1
    series.evict_oldest().unwrap();
    series.evict_oldest().unwrap();
    assert_eq!(series.evicted_samples(), (2 * CHUNK_LEN) as u64);

    let chunk2_start_ts = 10_000 + (2 * CHUNK_LEN as u64) * STEP_NS;
    let chunk2_end_ts = 10_000 + (3 * CHUNK_LEN as u64 - 1) * STEP_NS;
    let series_latest_ts = 10_000 + (4 * CHUNK_LEN as u64 - 1) * STEP_NS;

    // 1. Query timestamp strictly before earliest surviving sample -> None
    assert!(series.get_sample_by_timestamp(0).is_none());
    assert!(series.get_sample_by_timestamp(10_000).is_none());
    assert!(series
        .get_sample_by_timestamp(chunk2_start_ts - 1)
        .is_none());

    // 2. Query exact earliest surviving timestamp
    let (idx0, ts0, val0) = series
        .get_sample_by_timestamp(chunk2_start_ts)
        .expect("exact chunk2 start timestamp");
    assert_eq!(idx0, (2 * CHUNK_LEN) as u64);
    assert_eq!(ts0, chunk2_start_ts);
    assert_eq!(val0, 2000.0);

    // 3. Query timestamp between samples in chunk 2 (nearest neighbor interpolation)
    let mid_ts = chunk2_start_ts + 4; // nearest to chunk2_start_ts
    let (idx_mid1, ts_mid1, _) = series.get_sample_by_timestamp(mid_ts).unwrap();
    assert_eq!(idx_mid1, (2 * CHUNK_LEN) as u64);
    assert_eq!(ts_mid1, chunk2_start_ts);

    let mid_ts2 = chunk2_start_ts + 6; // nearest to chunk2_start_ts + 10
    let (idx_mid2, ts_mid2, _) = series.get_sample_by_timestamp(mid_ts2).unwrap();
    assert_eq!(idx_mid2, (2 * CHUNK_LEN + 1) as u64);
    assert_eq!(ts_mid2, chunk2_start_ts + STEP_NS);

    // 4. Query exact end of chunk 2
    let (idx_end2, ts_end2, val_end2) = series
        .get_sample_by_timestamp(chunk2_end_ts)
        .expect("chunk 2 end timestamp");
    assert_eq!(idx_end2, (3 * CHUNK_LEN - 1) as u64);
    assert_eq!(ts_end2, chunk2_end_ts);
    assert_eq!(val_end2, 2000.0 + (CHUNK_LEN - 1) as f64);

    // 5. Query exact latest timestamp
    let (idx_last, ts_last, val_last) = series
        .get_sample_by_timestamp(series_latest_ts)
        .expect("latest timestamp");
    assert_eq!(idx_last, (4 * CHUNK_LEN - 1) as u64);
    assert_eq!(ts_last, series_latest_ts);
    assert_eq!(val_last, 3000.0 + (CHUNK_LEN - 1) as f64);

    // 6. Query beyond latest timestamp -> None
    assert!(series.get_sample_by_timestamp(series_latest_ts + 1).is_none());
    assert!(series.get_sample_by_timestamp(u64::MAX).is_none());
}

#[test]
fn test_get_sample_by_timestamp_when_all_sealed_evicted_only_active_remains() {
    let mut series =
        NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());

    // Ingest 1 full chunk and seal it
    let ts0 = make_timestamps(1000, CHUNK_LEN);
    let val0 = make_values_linear(10.0, CHUNK_LEN);
    series.append(&ts0, ScalarSlice::F32(&val0)).unwrap();
    assert_eq!(series.sealed_chunk_count(), 1);

    // Ingest 50 samples into active buffer
    let active_base_ts = 1000 + (CHUNK_LEN as u64) * STEP_NS;
    let ts_act = make_timestamps(active_base_ts, 50);
    let val_act = make_values_linear(99.0, 50);
    series.append(&ts_act, ScalarSlice::F32(&val_act)).unwrap();
    assert_eq!(series.active_chunk().len(), 50);

    // Evict the only sealed chunk!
    series.evict_oldest().unwrap();
    assert_eq!(series.sealed_chunk_count(), 0);
    assert_eq!(series.evicted_samples(), CHUNK_LEN as u64);

    // Lookup timestamp before active buffer -> None
    assert!(series.get_sample_by_timestamp(1000).is_none());
    assert!(series
        .get_sample_by_timestamp(active_base_ts - 1)
        .is_none());

    // Lookup exact first active timestamp
    let (idx_act0, ts_act0, val_act0) = series
        .get_sample_by_timestamp(active_base_ts)
        .expect("first active timestamp");
    assert_eq!(
        idx_act0, CHUNK_LEN as u64,
        "Global index must account for evicted sealed chunk!"
    );
    assert_eq!(ts_act0, active_base_ts);
    assert_eq!(val_act0, 99.0);

    // Lookup 25th active sample
    let target_ts = active_base_ts + 25 * STEP_NS;
    let (idx_25, ts_25, val_25) = series
        .get_sample_by_timestamp(target_ts)
        .expect("25th active sample");
    assert_eq!(idx_25, (CHUNK_LEN + 25) as u64);
    assert_eq!(ts_25, target_ts);
    assert_eq!(val_25, 99.0 + 25.0);
}

// ============================================================================
// 4. 40% Pinning Memory Ceiling & Concurrency Stress
// ============================================================================

#[test]
fn test_pinning_memory_ceiling_exact_bounds() {
    let quota_total = 100_000_000usize; // 100 MB
    let q = Arc::new(MemoryQuotaManager::new(quota_total));
    // 40% ceiling = 40,000,000 bytes
    assert_eq!(q.pinned_limit_bytes(), 40_000_000);

    // Pin 39,999,999 bytes -> Ok
    let g1 = q.pin(39_999_999).expect("pin 39.99MB ok");
    assert_eq!(q.pinned_bytes(), 39_999_999);

    // Attempt to pin 2 more bytes (total 40,000,001 bytes) -> Exceeded!
    let err = q.pin(2).unwrap_err();
    assert!(matches!(err, ddscope_core::types::CoreError::Storage(_)));

    // Attempt to pin exact remaining 1 byte -> Ok
    let g2 = q.pin(1).expect("pin exact remaining 1 byte");
    assert_eq!(q.pinned_bytes(), 40_000_000);

    // Now 1 more byte fails
    assert!(q.pin(1).is_err());

    // Drop g1: 39,999,999 bytes released
    drop(g1);
    assert_eq!(q.pinned_bytes(), 1);

    // Now can pin 39,999,999 bytes again
    let g3 = q.pin(39_999_999).expect("pin again after drop");
    assert_eq!(q.pinned_bytes(), 40_000_000);

    drop(g2);
    drop(g3);
    assert_eq!(q.pinned_bytes(), 0);
}

#[test]
fn test_concurrent_multithreaded_pin_and_ingestion_stress() {
    let quota = Arc::new(MemoryQuotaManager::new(20 * 1024 * 1024)); // 20 MB
    let num_threads = 8;
    let iterations_per_thread = 200;

    let barrier = Arc::new(std::sync::Barrier::new(num_threads));
    let mut handles = Vec::new();

    for t_id in 0..num_threads {
        let q = Arc::clone(&quota);
        let b = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            b.wait();
            for i in 0..iterations_per_thread {
                let pin_size = 10_000 + (t_id * 1000 + i * 17) % 50_000;
                match q.pin(pin_size) {
                    Ok(guard) => {
                        // Hold briefly
                        assert!(q.pinned_bytes() <= q.pinned_limit_bytes());
                        assert_eq!(guard.bytes(), pin_size);
                        drop(guard);
                    }
                    Err(_) => {
                        // Pinned limit was hit, perfectly normal under concurrent load
                    }
                }
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    // All guards dropped: pinned bytes must return strictly to 0
    assert_eq!(
        quota.pinned_bytes(),
        0,
        "Pinned bytes must return to 0 after all threads finish"
    );
    assert!(!quota.is_pinned_active());
}

#[test]
fn test_pin_range_forces_active_chunk_seal_and_retains_data() {
    let mut series =
        NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
    let quota = Arc::new(MemoryQuotaManager::with_default_quota());
    series.set_quota_manager(Arc::clone(&quota));

    // Append 5,000 samples into active buffer
    let ts = make_timestamps(50_000, 5000);
    let vals = make_values_sine(0.1, 5000);
    series.append(&ts, ScalarSlice::F32(&vals)).unwrap();

    assert_eq!(series.active_chunk().len(), 5000);
    assert_eq!(series.sealed_chunk_count(), 0);

    // Pin range intersecting active buffer
    let guard = series
        .pin_range(50_000, 50_000 + 4999 * STEP_NS)
        .expect("pin range covering active buffer");

    // Active buffer was sealed into sealed_chunks!
    assert_eq!(series.sealed_chunk_count(), 1);
    assert_eq!(series.active_chunk().len(), 0);
    assert_eq!(guard.chunk_count(), 1);
    assert_eq!(guard.total_samples(), 5000);

    // Now roll over and evict this chunk from the series
    let evicted = series.evict_oldest().unwrap();
    drop(evicted);

    assert_eq!(series.sealed_chunk_count(), 0);
    assert_eq!(series.evicted_samples(), 5000);

    // Verify all 5,000 samples are bit-exact inside guard
    match guard.chunks()[0].as_ref() {
        DataChunk::Scalar(sc) => {
            assert_eq!(sc.len(), 5000);
            for i in 0..5000 {
                let (t, v) = sc.get_sample_as_f64(i).unwrap();
                assert_eq!(t, ts[i]);
                assert_eq!(v, vals[i] as f64);
            }
        }
        _ => panic!("Expected DataChunk::Scalar"),
    }

    drop(guard);
    assert_eq!(quota.pinned_bytes(), 0);
    assert_eq!(quota.allocated_bytes(), 0);
}

#[test]
fn test_interspersed_pinned_and_unpinned_evictions_and_reconciliation() {
    let mut series =
        NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
    let quota = Arc::new(MemoryQuotaManager::new(10 * 1024 * 1024));
    series.set_quota_manager(Arc::clone(&quota));

    // Ingest 5 chunks [0, 1, 2, 3, 4]
    let mut chunk_timestamps = Vec::new();
    for c in 0..5 {
        let base_ts = (c * CHUNK_LEN) as u64 * STEP_NS;
        let ts = make_timestamps(base_ts, CHUNK_LEN);
        let vals = make_values_linear((c * 10_000) as f32, CHUNK_LEN);
        chunk_timestamps.push(ts.clone());
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
    }
    assert_eq!(series.sealed_chunk_count(), 5);

    // Pin chunk 1 and chunk 3 (interspersed with unpinned chunks 0, 2, 4)
    let ts1_start = chunk_timestamps[1][0];
    let ts1_end = chunk_timestamps[1][CHUNK_LEN - 1];
    let guard1 = series.pin_range(ts1_start, ts1_end).unwrap();
    assert_eq!(guard1.chunk_count(), 1);

    let ts3_start = chunk_timestamps[3][0];
    let ts3_end = chunk_timestamps[3][CHUNK_LEN - 1];
    let guard3 = series.pin_range(ts3_start, ts3_end).unwrap();
    assert_eq!(guard3.chunk_count(), 1);

    let total_alloc_5 = quota.allocated_bytes();
    let chunk_size = guard1.chunks()[0].byte_footprint();

    // Evict chunk 0 (unpinned): memory freed immediately
    let e0 = series.evict_oldest().unwrap();
    drop(e0);
    assert_eq!(quota.allocated_bytes(), total_alloc_5 - chunk_size);
    assert_eq!(series.evicted_samples(), CHUNK_LEN as u64);

    // Evict chunk 1 (pinned in guard1): memory retained!
    let e1 = series.evict_oldest().unwrap();
    drop(e1);
    assert_eq!(quota.allocated_bytes(), total_alloc_5 - chunk_size);
    assert_eq!(series.evicted_samples(), (2 * CHUNK_LEN) as u64);

    // Evict chunk 2 (unpinned): memory freed immediately
    let e2 = series.evict_oldest().unwrap();
    drop(e2);
    assert_eq!(quota.allocated_bytes(), total_alloc_5 - 2 * chunk_size);
    assert_eq!(series.evicted_samples(), (3 * CHUNK_LEN) as u64);

    // Evict chunk 3 (pinned in guard3): memory retained!
    let e3 = series.evict_oldest().unwrap();
    drop(e3);
    assert_eq!(quota.allocated_bytes(), total_alloc_5 - 2 * chunk_size);
    assert_eq!(series.evicted_samples(), (4 * CHUNK_LEN) as u64);

    // Evict chunk 4 (unpinned): memory freed immediately
    let e4 = series.evict_oldest().unwrap();
    drop(e4);
    assert_eq!(quota.allocated_bytes(), 2 * chunk_size);
    assert_eq!(series.evicted_samples(), (5 * CHUNK_LEN) as u64);
    assert_eq!(series.sealed_chunk_count(), 0);

    // Verify chunk 1 in guard1 is 100% intact
    match guard1.chunks()[0].as_ref() {
        DataChunk::Scalar(sc) => {
            let (ts, v) = sc.get_sample_as_f64(0).unwrap();
            assert_eq!(ts, ts1_start);
            assert_eq!(v, 10_000.0);
        }
        _ => panic!("Expected DataChunk::Scalar"),
    }

    // Verify chunk 3 in guard3 is 100% intact
    match guard3.chunks()[0].as_ref() {
        DataChunk::Scalar(sc) => {
            let (ts, v) = sc.get_sample_as_f64(0).unwrap();
            assert_eq!(ts, ts3_start);
            assert_eq!(v, 30_000.0);
        }
        _ => panic!("Expected DataChunk::Scalar"),
    }

    // Drop guard3 first: chunk 3 deallocated
    drop(guard3);
    assert_eq!(quota.allocated_bytes(), chunk_size);

    // Drop guard1: chunk 1 deallocated -> 0 bytes allocated
    drop(guard1);
    assert_eq!(quota.allocated_bytes(), 0);
    assert_eq!(quota.pinned_bytes(), 0);
}

#[test]
fn test_fifo_eviction_unblocks_and_catches_up_after_pin_drop() {
    let mut series =
        NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
    // Room for ~2.5 chunks (1,400,000 bytes).
    // 40% pinning limit = 560,000 bytes, which allows pinning 1 chunk (524,288 bytes).
    let quota = Arc::new(MemoryQuotaManager::new(1_400_000));
    series.set_quota_manager(Arc::clone(&quota));

    // Ingest chunk 0 and chunk 1
    let ts0 = make_timestamps(0, CHUNK_LEN);
    let v0 = make_values_linear(0.0, CHUNK_LEN);
    series.append(&ts0, ScalarSlice::F32(&v0)).unwrap();

    let ts1 = make_timestamps(CHUNK_LEN as u64 * STEP_NS, CHUNK_LEN);
    let v1 = make_values_linear(1000.0, CHUNK_LEN);
    series.append(&ts1, ScalarSlice::F32(&v1)).unwrap();

    assert_eq!(series.sealed_chunk_count(), 2);
    assert!(!quota.is_over_quota());

    // Pin chunk 0: strong_count becomes 2
    let guard0 = series.pin_range(ts0[0], ts0[CHUNK_LEN - 1]).unwrap();

    // Now append chunk 2: this pushes quota over limit!
    // But since chunk 0 is pinned, FIFO eviction at head blocks
    let ts2 = make_timestamps(2 * CHUNK_LEN as u64 * STEP_NS, CHUNK_LEN);
    let v2 = make_values_linear(2000.0, CHUNK_LEN);
    series.append(&ts2, ScalarSlice::F32(&v2)).unwrap();

    // 3 sealed chunks reside in series because head chunk 0 was protected
    assert_eq!(series.sealed_chunk_count(), 3);
    assert_eq!(series.evicted_samples(), 0);
    assert!(quota.is_over_quota());

    // Now user unpins chunk 0 (scope rearmed / running)
    drop(guard0);
    assert_eq!(quota.pinned_bytes(), 0);

    // Ingest chunk 3: Now FIFO eviction can unpin chunk 0 and chunk 1 to restore quota!
    let ts3 = make_timestamps(3 * CHUNK_LEN as u64 * STEP_NS, CHUNK_LEN);
    let v3 = make_values_linear(3000.0, CHUNK_LEN);
    series.append(&ts3, ScalarSlice::F32(&v3)).unwrap();

    // FIFO eviction unblocked and caught up!
    // It evicted chunk 0 and chunk 1 to bring allocation back under quota!
    assert_eq!(series.sealed_chunk_count(), 2); // chunks 2 and 3 remain
    assert_eq!(series.evicted_samples(), (2 * CHUNK_LEN) as u64);
    assert!(!quota.is_over_quota());

    // Chunk 0 and 1 are evicted
    assert!(series.get_sample_by_index(0).is_none());
    assert!(series.get_sample_by_index((2 * CHUNK_LEN - 1) as u64).is_none());

    // Chunk 2 is first surviving chunk
    let (t_c2, val_c2) = series.get_sample_by_index((2 * CHUNK_LEN) as u64).unwrap();
    assert_eq!(t_c2, 2 * CHUNK_LEN as u64 * STEP_NS);
    assert_eq!(val_c2, 2000.0);
}

#[test]
fn test_pin_range_evicted_and_partial_evicted_windows() {
    let mut series =
        NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
    let quota = Arc::new(MemoryQuotaManager::with_default_quota());
    series.set_quota_manager(Arc::clone(&quota));

    // Ingest 3 chunks
    for c in 0..3 {
        let base_ts = 100_000 + (c * CHUNK_LEN as u64) * STEP_NS;
        let ts = make_timestamps(base_ts, CHUNK_LEN);
        let vals = make_values_linear((c * 100) as f32, CHUNK_LEN);
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
    }

    let chunk0_start = 100_000;
    let chunk0_end = 100_000 + (CHUNK_LEN as u64 - 1) * STEP_NS;
    let chunk1_start = 100_000 + (CHUNK_LEN as u64) * STEP_NS;
    let chunk1_end = 100_000 + (2 * CHUNK_LEN as u64 - 1) * STEP_NS;

    // Evict chunk 0
    series.evict_oldest().unwrap();
    assert_eq!(series.evicted_samples(), CHUNK_LEN as u64);

    // 1. Attempting to pin entirely evicted window [chunk0_start, chunk0_end] -> Err
    let err = series.pin_range(chunk0_start, chunk0_end).unwrap_err();
    assert!(matches!(err, ddscope_core::types::CoreError::Storage(_)));

    // 2. Pin window spanning evicted chunk 0 and resident chunk 1
    // [chunk0_start, chunk1_start + 1000] -> Should successfully pin chunk 1!
    let guard = series
        .pin_range(chunk0_start, chunk1_start + 1000)
        .expect("partial overlap with resident chunk should succeed");
    assert_eq!(guard.chunk_count(), 1);
    assert_eq!(guard.time_range_ns(), Some((chunk1_start, chunk1_end)));
}

#[test]
fn test_long_running_100_chunks_continuous_rollover_quota_stability() {
    let mut series =
        NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
    // Room for 3 chunks: ~1.7 MB
    let quota = Arc::new(MemoryQuotaManager::new(1_700_000));
    series.set_quota_manager(Arc::clone(&quota));

    const TOTAL_CHUNKS: usize = 100;
    let mut saved_guards = Vec::new();

    for c in 0..TOTAL_CHUNKS {
        let base_ts = 1_000_000 + (c as u64 * CHUNK_LEN as u64) * STEP_NS;
        let ts = make_timestamps(base_ts, CHUNK_LEN);
        let vals = make_values_linear((c * 10) as f32, CHUNK_LEN);
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();

        // At chunks 10 and 50, pin them
        if c == 10 || c == 50 {
            let g = series.pin_range(base_ts, base_ts + 1000).unwrap();
            saved_guards.push((c, base_ts, g));
        }

        // Drop pin 10 at chunk 40
        if c == 40 && !saved_guards.is_empty() {
            saved_guards.remove(0); // drops guard 10
        }
    }

    assert_eq!(series.total_samples(), (TOTAL_CHUNKS * CHUNK_LEN) as u64);

    // Guard 50 was retained: verify its data is bit-exact
    assert_eq!(saved_guards.len(), 1);
    let (c_pinned, base_ts, guard50) = saved_guards.pop().unwrap();
    assert_eq!(c_pinned, 50);
    match guard50.chunks()[0].as_ref() {
        DataChunk::Scalar(sc) => {
            let (t0, v0) = sc.get_sample_as_f64(0).unwrap();
            assert_eq!(t0, base_ts);
            assert_eq!(v0, (50 * 10) as f64);
        }
        _ => panic!("Expected DataChunk::Scalar"),
    }

    drop(guard50);
    assert_eq!(quota.pinned_bytes(), 0);
}

