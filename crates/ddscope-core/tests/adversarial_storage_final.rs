//! Adversarial Coverage Hardening & Empirical Stress Suite for DDScopeHost Storage Subsystem.
//!
//! Focus:
//! 1. MultiLevelM4Pyramid hierarchy integrity, L3/L4 accumulation, and eviction interaction.
//! 2. Ring buffer quota tracking, FIFO eviction, and PinnedGuard RAII invariants.
//! 3. Numeric stability (NaN, Inf, negative scale, extreme timestamps).
//! 4. Index resolution under non-uniform chunk sealing and continuous rollover.
//! 5. Concurrency stress on quota allocation, deallocation, and pinning.

use std::sync::Arc;
use std::thread;

use ddscope_core::storage::m4::{feed_bucket_into_slots, M4QueryResult, PixelSlot};
use ddscope_core::storage::quota::MemoryQuotaManager;
use ddscope_core::storage::scalar_series::NativeScalarSeries;
use ddscope_core::types::{
    ChannelId, LinearScale, M4Bucket, ScalarSlice, ScalarType, CHUNK_LEN,
};

/// Helper: appends N chunks each of CHUNK_LEN (65,536) samples with constant increment.
fn append_chunks(series: &mut NativeScalarSeries, num_chunks: usize, start_val: f32, val_step: f32) {
    let mut ts_buf = Vec::with_capacity(CHUNK_LEN);
    let mut val_buf = Vec::with_capacity(CHUNK_LEN);

    let base_ts = series.latest_timestamp().map(|t| t + 10).unwrap_or(1_000);
    let mut curr_val = start_val;

    for chunk_idx in 0..num_chunks {
        ts_buf.clear();
        val_buf.clear();
        let chunk_base_ts = base_ts + (chunk_idx * CHUNK_LEN) as u64 * 10;
        for i in 0..CHUNK_LEN {
            ts_buf.push(chunk_base_ts + (i as u64) * 10);
            val_buf.push(curr_val);
            curr_val += val_step;
        }
        series.append(&ts_buf, ScalarSlice::F32(&val_buf)).expect("append chunk");
    }
}

// ===========================================================================
// Group 1: Pyramid Hierarchy & Eviction Interaction
// ===========================================================================

#[test]
fn adv_pyramid_l3_l4_accumulation_exact_powers() {
    let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());

    // 4 chunks = 64 L2 buckets -> exactly 1 L3 bucket
    append_chunks(&mut series, 4, 1.0, 0.0);
    assert_eq!(series.pyramid().l3.len(), 1);
    assert_eq!(series.pyramid().l4.len(), 0);
    assert_eq!(series.pyramid().accum_l2.len(), 0);
    assert_eq!(series.pyramid().accum_l3.len(), 1);

    // Append 3 more chunks (48 L2 buckets in accum_l2)
    append_chunks(&mut series, 3, 1.0, 0.0);
    assert_eq!(series.pyramid().l3.len(), 1);
    assert_eq!(series.pyramid().accum_l2.len(), 48);

    // 1 more chunk completes the second L3 bucket
    append_chunks(&mut series, 1, 1.0, 0.0);
    assert_eq!(series.pyramid().l3.len(), 2);
    assert_eq!(series.pyramid().accum_l2.len(), 0);
    assert_eq!(series.pyramid().accum_l3.len(), 2);
}

#[test]
fn adv_pyramid_evicted_chunk_spike_leak_investigation() {
    // Investigate boundary behavior when a chunk inside an L3 bucket is evicted
    let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());

    // Ingest chunks 0..3 (normal values = 1.0)
    append_chunks(&mut series, 4, 1.0, 0.0);
    assert_eq!(series.pyramid().l3.len(), 1);

    // Ingest chunk 4 with a massive spike (+99_999.0)
    let mut ts_buf = Vec::with_capacity(CHUNK_LEN);
    let mut val_buf = Vec::with_capacity(CHUNK_LEN);
    let base_ts = series.latest_timestamp().unwrap() + 10;
    for i in 0..CHUNK_LEN {
        ts_buf.push(base_ts + (i as u64) * 10);
        val_buf.push(if i == 500 { 99_999.0 } else { 1.0 });
    }
    series.append(&ts_buf, ScalarSlice::F32(&val_buf)).unwrap();

    // Ingest chunks 5, 6, 7 (values = 1.0) -> completes L3 bucket 1
    append_chunks(&mut series, 3, 1.0, 0.0);
    assert_eq!(series.pyramid().l3.len(), 2);

    // Now evict chunks 0, 1, 2, 3, 4 (5 chunks evicted)
    for _ in 0..5 {
        assert!(series.evict_oldest().is_some());
    }
    assert_eq!(series.evicted_samples(), 5 * CHUNK_LEN as u64);

    // Chunks 5, 6, 7 remain in memory.
    // Query viewport for the strictly retained range [chunk 5 start, chunk 7 end]
    let (t_start, t_end) = series.time_range_ns().unwrap();

    // Choose w_px = 10 so samples_per_pixel = (3 * 65536) / 10 = 19,660 < 262,144 (Selects Level 2)
    let res_l2 = series.query_m4_viewport_with_stats(t_start, t_end, 10).unwrap();
    // At Level 2 (intra-chunk in memory), chunk 4 is NOT in memory, so max should be 1.0!
    assert_eq!(res_l2.stats.max, 1.0, "Level 2 correctly does NOT see evicted chunk 4 spike");

    // But what if w_px = 2 and we have enough samples to select Level 3?
    // Let's append more chunks (8..15) with value 1.0
    append_chunks(&mut series, 8, 1.0, 0.0);
    let (t_start_retained, t_end_retained) = series.time_range_ns().unwrap();
    let retained_samples = series.total_samples() - series.evicted_samples();
    assert_eq!(retained_samples, 11 * CHUNK_LEN as u64); // chunks 5..15 (720,896 samples)

    // With w_px = 2, samples_per_pixel = 720,896 / 2 = 360,448 >= 262,144 -> Selects Level 3!
    let res_l3 = series.query_m4_viewport_with_stats(t_start_retained, t_end_retained, 2).unwrap();

    // In Level 3: self.pyramid.l3[1] covers chunks 4, 5, 6, 7.
    // Chunk 4 had the spike +99_999.0, but was evicted!
    println!("Level 3 query max on retained range: {}", res_l3.stats.max);
    let leaked = (res_l3.stats.max - 99_999.0).abs() < 1e-2;
    println!("Spike leaked into Level 3 query of retained data: {}", leaked);
    assert!(!leaked, "Spike from evicted chunk 4 must NOT leak into retained range Level 3 query");
    assert_eq!(res_l3.stats.max, 1.0, "Retained range max must strictly reflect retained chunks");
}

#[test]
fn adv_feed_bucket_into_slots_u64_timestamp_overflow_panic() {
    let mut slots = vec![PixelSlot::EMPTY; 10];
    let b = M4Bucket {
        t_first_ns: u64::MAX - 100,
        t_last_ns: u64::MAX - 10,
        v_first: 1.0,
        v_min: 0.0,
        v_max: 2.0,
        v_last: 1.0,
    };
    // (u64::MAX - 100) + (u64::MAX - 10) must not overflow u64 via overflow-safe midpoint calculation
    feed_bucket_into_slots(&mut slots, &b, u64::MAX - 200, u64::MAX, 1.0, 10);
    assert!(slots[9].has_data, "Slot 9 must have received bucket data safely without overflow panic");
    assert_eq!(slots[9].v_min, 0.0);
    assert_eq!(slots[9].v_max, 2.0);
}

#[test]
fn adv_pyramid_unbounded_growth_under_ring_buffer_rollover() {
    let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
    // Configure quota for at most 2 chunks (~1MB)
    let quota = Arc::new(MemoryQuotaManager::new(2 * 512 * 1024));
    series.set_quota_manager(Arc::clone(&quota));

    // Append 100 chunks. Because quota is 2 chunks, at any point only 1-2 chunks remain in RAM.
    append_chunks(&mut series, 100, 1.0, 0.0);

    // Sealed chunks in memory is kept bounded by FIFO eviction:
    assert!(series.sealed_chunk_count() <= 3, "Sealed chunks must be bounded by quota");
    assert_eq!(series.evicted_samples(), (100 - series.sealed_chunk_count() as u64) * CHUNK_LEN as u64);

    // Pyramid memory must also be kept bounded by pruning buckets older than oldest retained chunk:
    let l3_buckets = series.pyramid().l3.len();
    println!("Sealed chunks in memory: {}", series.sealed_chunk_count());
    println!("Pyramid L3 bucket count: {}", l3_buckets);
    assert!(l3_buckets <= 1, "Pyramid L3 must be bounded during rollover, got {}", l3_buckets);
}

// ===========================================================================
// Group 2: Quota & Pinning Concurrency & Ceiling Hardening
// ===========================================================================

#[test]
fn adv_quota_concurrency_stress_no_underflow_or_leak() {
    let quota = Arc::new(MemoryQuotaManager::new(10_000_000)); // 10MB limit (pinned limit = 4MB)
    let num_threads = 8;
    let iterations_per_thread = 5_000;
    let mut handles = Vec::new();

    for _ in 0..num_threads {
        let q = Arc::clone(&quota);
        handles.push(thread::spawn(move || {
            for i in 0..iterations_per_thread {
                let alloc_size = (i % 256) + 1;
                // Alternate between record_allocation, try_pin, release_pin, record_deallocation
                if i % 2 == 0 {
                    q.record_allocation(alloc_size);
                    // Pin attempt
                    if let Ok(()) = q.try_pin(alloc_size) {
                        assert!(q.pinned_bytes() <= q.pinned_limit_bytes(), "Pinned bytes must NEVER exceed ceiling");
                        q.release_pin(alloc_size);
                    }
                    q.record_deallocation(alloc_size);
                } else {
                    let _ = q.allocate(alloc_size);
                    q.release(alloc_size);
                }
            }
        }));
    }

    for h in handles {
        h.join().expect("thread join");
    }

    // Invariants post-stress:
    assert_eq!(quota.allocated_bytes(), 0, "Allocated bytes must balance to 0");
    assert_eq!(quota.pinned_bytes(), 0, "Pinned bytes must balance to 0");
    assert!(!quota.is_warning(), "Warning flag must be cleared");
    assert!(!quota.is_over_quota(), "Over quota must be false");
    assert!(!quota.is_pinned_active(), "Pinned active must be false");
}

#[test]
fn adv_pin_range_rejection_active_buffer_side_effect() {
    // When pin_range is rejected by 40% ceiling, does active buffer get sealed anyway?
    let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
    let quota = Arc::new(MemoryQuotaManager::new(1_000_000)); // 1MB limit -> 400KB pinned ceiling
    series.set_quota_manager(Arc::clone(&quota));

    // Append 100 samples into active buffer
    let ts: Vec<u64> = (0..100).map(|i| 1_000 + i * 10).collect();
    let val: Vec<f32> = (0..100).map(|i| i as f32).collect();
    series.append(&ts, ScalarSlice::F32(&val)).unwrap();
    assert_eq!(series.active_chunk().len(), 100);
    assert_eq!(series.sealed_chunk_count(), 0);

    // Artificial exhaustion of pinned quota ceiling to force pin_range failure:
    quota.try_pin(400_000).expect("exhaust pinned ceiling");
    assert_eq!(quota.pinned_bytes(), 400_000);

    // Now call pin_range overlapping the active buffer.
    // Ceiling is exhausted, so try_pin inside pin_range MUST FAIL!
    let res = series.pin_range(1_000, 1_990);
    assert!(res.is_err(), "pin_range must be rejected when pinned ceiling is exceeded");

    // VERIFICATION: Transactional integrity - when pin_range is rejected by quota ceiling,
    // active buffer must NOT be sealed into history.
    let sealed_count = series.sealed_chunk_count();
    let active_len = series.active_chunk().len();
    println!("After rejected pin_range: sealed_chunks={}, active_len={}", sealed_count, active_len);
    assert_eq!(sealed_count, 0, "Active chunk must NOT be sealed before quota check succeeds");
    assert_eq!(active_len, 100, "Active buffer must remain untouched on pin_range error");
}

#[test]
fn adv_chunk_byte_footprint_includes_m4_levels() {
    let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
    append_chunks(&mut series, 1, 1.0, 0.0);
    assert_eq!(series.sealed_chunk_count(), 1);

    let quota = Arc::new(MemoryQuotaManager::new(10_000_000));
    series.set_quota_manager(Arc::clone(&quota));

    let guard = series.pin_range(1_000, 2_000_000).expect("pin range");
    let chunk = &guard.chunks()[0];

    // ScalarDataChunk contains timestamps (RelU32 = 65,536 * 4 = 262,144 bytes)
    // values (F32 = 65,536 * 4 = 262,144 bytes)
    // M4 levels: 1024 L1 buckets (32,768 bytes) + 16 L2 buckets (512 bytes) = 33,280 bytes
    // Total footprint = 262,144 + 262,144 + 33,280 = 557,568 bytes.
    let footprint = chunk.byte_footprint();
    println!("Sealed chunk byte footprint: {}", footprint);
    assert!(
        footprint >= 557_568,
        "Chunk byte footprint {} must include M4 pyramid levels (>= 557,568 bytes)",
        footprint
    );
    assert_eq!(guard.pinned_bytes(), footprint);
}

#[test]
fn adv_pinned_ceiling_multi_thread_racer_never_exceeds_40_percent() {
    let quota = Arc::new(MemoryQuotaManager::new(1_000_000)); // 400,000 bytes ceiling
    let ceiling = quota.pinned_limit_bytes();
    assert_eq!(ceiling, 400_000);

    let num_threads = 16;
    let mut handles = Vec::new();
    let success_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    for _ in 0..num_threads {
        let q = Arc::clone(&quota);
        let sc = Arc::clone(&success_count);
        handles.push(thread::spawn(move || {
            for _ in 0..100 {
                // Attempt to pin 50,000 bytes. At most 8 concurrent pins can succeed (8 * 50k = 400k)
                if q.try_pin(50_000).is_ok() {
                    let cur = q.pinned_bytes();
                    assert!(cur <= ceiling, "Pinned bytes {} exceeded ceiling {}", cur, ceiling);
                    sc.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    // brief hold
                    std::thread::yield_now();
                    q.release_pin(50_000);
                }
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    assert_eq!(quota.pinned_bytes(), 0);
    assert!(success_count.load(std::sync::atomic::Ordering::Relaxed) > 0);
}

// ===========================================================================
// Group 3: Extreme Numeric Edge Cases (NaN, Inf, Negative Scale)
// ===========================================================================

#[test]
fn adv_m4_reduction_negative_scale_inversion() {
    let scale = LinearScale { scale: -2.0, offset: 10.0 };
    let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, scale);

    // Input raw: min is 1.0, max is 10.0
    // Scaled: 1.0 * -2.0 + 10.0 = 8.0 (this becomes max!)
    // Scaled: 10.0 * -2.0 + 10.0 = -10.0 (this becomes min!)
    let ts = vec![1000, 1010, 1020, 1030];
    let val = vec![1.0_f32, 5.0, 10.0, 3.0];
    series.append(&ts, ScalarSlice::F32(&val)).unwrap();

    let (t_min, t_max) = series.time_range_ns().unwrap();
    let extract = series.query_m4_viewport_with_stats(t_min, t_max, 10).unwrap();

    // In raw mode:
    match &extract.result {
        M4QueryResult::Raw(pts) => {
            assert_eq!(pts.len(), 4);
            assert_eq!(pts[0].y_val, 8.0);
            assert_eq!(pts[2].y_val, -10.0);
        }
        M4QueryResult::Columns(_) => panic!("expected raw mode for 4 points"),
    }

    assert_eq!(extract.stats.min, -10.0);
    assert_eq!(extract.stats.max, 8.0);
    assert_eq!(extract.stats.vpp, 18.0);
}

#[test]
fn adv_m4_nan_and_infinity_resilience() {
    let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());

    // Chunk with NaN and Infinities
    let ts = vec![100, 200, 300, 400, 500];
    let val = vec![f32::NAN, 1.0, f32::INFINITY, -f32::INFINITY, f32::NAN];

    // Series should accept batch without panicking
    assert!(series.append(&ts, ScalarSlice::F32(&val)).is_ok());

    let (t_min, t_max) = series.time_range_ns().unwrap();
    let extract = series.query_m4_viewport_with_stats(t_min, t_max, 10).unwrap();

    // Stats calculation should not panic on Inf / NaN
    println!("Stats with Inf/NaN: min={}, max={}, mean={}", extract.stats.min, extract.stats.max, extract.stats.mean);
}

// ===========================================================================
// Group 4: Non-Uniform Sealing & Eviction Index Mapping
// ===========================================================================

#[test]
fn adv_non_uniform_sealing_and_eviction_random_access() {
    let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());

    // Create 5 chunks of erratic lengths: 10, 50, 65536, 1, 100
    let lens = [10, 50, CHUNK_LEN, 1, 100];
    let mut cur_ts = 1_000u64;
    let mut cur_val = 0.0f32;
    let mut all_samples = Vec::new();

    for &len in &lens {
        let mut ts_buf = Vec::with_capacity(len);
        let mut val_buf = Vec::with_capacity(len);
        for _ in 0..len {
            ts_buf.push(cur_ts);
            val_buf.push(cur_val);
            all_samples.push((cur_ts, cur_val as f64));
            cur_ts += 10;
            cur_val += 1.0;
        }
        series.append(&ts_buf, ScalarSlice::F32(&val_buf)).unwrap();
        series.seal_active().unwrap();
    }

    assert_eq!(series.sealed_chunk_count(), 5);
    assert_eq!(series.total_samples(), all_samples.len() as u64);

    // Verify 100% exact sample retrieval by index across all non-uniform chunks
    for (i, expected) in all_samples.iter().enumerate() {
        let (ts, val) = series.get_sample_by_index(i as u64).expect("sample exists");
        assert_eq!(ts, expected.0);
        assert_eq!(val, expected.1);
    }

    // Now evict the first 2 chunks (lengths 10 and 50 -> 60 samples total)
    assert!(series.evict_oldest().is_some());
    assert!(series.evict_oldest().is_some());
    assert_eq!(series.evicted_samples(), 60);

    // Access to evicted samples [0..60) MUST return None
    for i in 0..60 {
        assert!(series.get_sample_by_index(i as u64).is_none(), "Evicted sample must return None");
    }

    // Access to remaining samples [60..all_samples.len()) MUST remain 100% accurate
    for (i, expected) in all_samples.iter().enumerate().skip(60) {
        let (ts, val) = series.get_sample_by_index(i as u64).expect("retained sample exists");
        assert_eq!(ts, expected.0);
        assert_eq!(val, expected.1);
    }
}

// ===========================================================================
// Group 5: Viewport Boundary & Precision Invariants
// ===========================================================================

#[test]
fn adv_viewport_adaptive_raw_to_columns_transition() {
    let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
    let w_px = 100;

    // Append exactly 2 * w_px samples (200 samples) -> Must be Raw
    let ts: Vec<u64> = (0..200).map(|i| 1_000 + i * 10).collect();
    let val: Vec<f32> = (0..200).map(|i| (i as f32).sin()).collect();
    series.append(&ts, ScalarSlice::F32(&val)).unwrap();

    let (t_min, t_max) = series.time_range_ns().unwrap();
    let res_raw = series.query_m4_viewport(t_min, t_max, w_px).unwrap();
    match res_raw {
        M4QueryResult::Raw(pts) => assert_eq!(pts.len(), 200, "N == 2*W_px must return Raw"),
        M4QueryResult::Columns(_) => panic!("N == 2*W_px must NOT return Columns"),
    }

    // Append 1 more sample -> 201 samples (N > 2*W_px) -> Must be Columns
    let ts_next = vec![1_000 + 200 * 10];
    let val_next = vec![0.5f32];
    series.append(&ts_next, ScalarSlice::F32(&val_next)).unwrap();

    let (t_min2, t_max2) = series.time_range_ns().unwrap();
    let res_cols = series.query_m4_viewport(t_min2, t_max2, w_px).unwrap();
    match res_cols {
        M4QueryResult::Columns(cols) => {
            assert_eq!(cols.len(), w_px, "N > 2*W_px must return exactly W_px columns");
            for (p, col) in cols.iter().enumerate() {
                let expected_x = (p as f32 + 0.5) / (w_px as f32);
                assert!((col.x_norm - expected_x).abs() < 1e-4);
                assert!(col.v_min <= col.v_max);
            }
        }
        M4QueryResult::Raw(_) => panic!("N > 2*W_px must NOT return Raw"),
    }
}
