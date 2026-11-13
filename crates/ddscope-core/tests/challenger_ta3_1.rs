//! Adversarial challenge stress test suite for Milestone TA3:
//! Ring Memory Quota & Chunk Pinning (`MemoryQuotaManager` and `PinnedGuard`).
//!
//! Author: challenger_ta3_1 (Empirical Challenger)

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

use ddscope_core::scope::ddsf::{DDSF_FLAG_PINNED_ACTIVE, DDSF_FLAG_QUOTA_WARNING};
use ddscope_core::storage::quota::{
    MemoryQuotaManager, PinnedGuard, QuotaError, DEFAULT_MEMORY_QUOTA_BYTES,
};
use ddscope_core::storage::scalar_series::NativeScalarSeries;
use ddscope_core::types::{
    ChannelId, LinearScale, ScalarSlice, ScalarType, CHUNK_LEN,
};

// ============================================================================
// 1. BOUNDARY & FRACTIONAL PIN CEILING TESTS
// ============================================================================

#[test]
fn test_boundary_pinning_exact_and_over_cap() {
    let q = MemoryQuotaManager::new(10_000);
    assert_eq!(q.quota_bytes(), 10_000);
    assert_eq!(q.pinned_limit_bytes(), 4_000);
    assert_eq!(q.pinned_ceiling_bytes(), 4_000);

    // Exact pin up to 40% cap
    assert!(q.try_pin(4_000).is_ok());
    assert_eq!(q.pinned_bytes(), 4_000);
    assert!(q.is_pinned_active());

    // 1 byte over cap should fail with QuotaError::PinnedLimitExceeded
    let err = q.try_pin(1).unwrap_err();
    match err {
        QuotaError::PinnedLimitExceeded {
            requested_bytes,
            current_pinned_bytes,
            pinned_limit_bytes,
        } => {
            assert_eq!(requested_bytes, 1);
            assert_eq!(current_pinned_bytes, 4_000);
            assert_eq!(pinned_limit_bytes, 4_000);
        }
        other => panic!("Unexpected error variant: {other:?}"),
    }
    assert_eq!(q.pinned_bytes(), 4_000);

    // 0-byte pin request should succeed without mutating counters
    assert!(q.try_pin(0).is_ok());
    assert_eq!(q.pinned_bytes(), 4_000);

    // Release 1 byte, now 1 byte should succeed
    q.release_pin(1);
    assert_eq!(q.pinned_bytes(), 3_999);
    assert!(q.try_pin(1).is_ok());
    assert_eq!(q.pinned_bytes(), 4_000);

    // Release all
    q.release_pin(4_000);
    assert_eq!(q.pinned_bytes(), 0);
    assert!(!q.is_pinned_active());

    // Default 2 GiB quota. 40% cap = 858,993,459 bytes.
    let q2 = MemoryQuotaManager::with_default_quota();
    assert_eq!(q2.quota_bytes(), DEFAULT_MEMORY_QUOTA_BYTES);
    assert_eq!(q2.pinned_limit_bytes(), 858_993_459);
    assert_eq!(q2.warning_threshold_bytes(), 1_825_361_100);

    // Pin exactly up to 2GB 40% limit
    assert!(q2.try_pin(858_993_459).is_ok());
    assert_eq!(q2.pinned_bytes(), 858_993_459);

    // Pin 1 byte over
    let err2 = q2.try_pin(1).unwrap_err();
    match err2 {
        QuotaError::PinnedLimitExceeded {
            requested_bytes,
            current_pinned_bytes,
            pinned_limit_bytes,
        } => {
            assert_eq!(requested_bytes, 1);
            assert_eq!(current_pinned_bytes, 858_993_459);
            assert_eq!(pinned_limit_bytes, 858_993_459);
        }
        other => panic!("Unexpected error variant: {other:?}"),
    }
    assert_eq!(q2.pinned_bytes(), 858_993_459);

    // Clean release
    q2.release_pin(858_993_459);
    assert_eq!(q2.pinned_bytes(), 0);
}

// ============================================================================
// 2. SATURATION UNDERFLOW ATTACK RESISTANCE
// ============================================================================

#[test]
fn test_underflow_attack_resistance() {
    let q = MemoryQuotaManager::new(1_000_000);

    // Initial state: 0 allocated, 0 pinned
    assert_eq!(q.allocated_bytes(), 0);
    assert_eq!(q.pinned_bytes(), 0);

    // Direct underflow attack on empty quota manager
    q.record_deallocation(1);
    assert_eq!(q.allocated_bytes(), 0);

    q.record_deallocation(1_000_000);
    assert_eq!(q.allocated_bytes(), 0);

    q.record_deallocation(usize::MAX);
    assert_eq!(q.allocated_bytes(), 0);

    q.release_pin(1);
    assert_eq!(q.pinned_bytes(), 0);

    q.release_pin(1_000_000);
    assert_eq!(q.pinned_bytes(), 0);

    q.release_pin(usize::MAX);
    assert_eq!(q.pinned_bytes(), 0);

    // Allocate 100 bytes, then deallocate 200 bytes -> saturates to 0
    assert!(q.record_allocation(100));
    assert_eq!(q.allocated_bytes(), 100);
    q.record_deallocation(200);
    assert_eq!(q.allocated_bytes(), 0);

    // Pin 50 bytes, then release 100 bytes -> saturates to 0
    assert!(q.try_pin(50).is_ok());
    assert_eq!(q.pinned_bytes(), 50);
    q.release_pin(100);
    assert_eq!(q.pinned_bytes(), 0);

    // Concurrent underflow attack: 16 threads simultaneously hammering dealloc and release_pin
    let q_arc = Arc::new(MemoryQuotaManager::new(10_000));
    let mut handles = Vec::new();
    for _ in 0..16 {
        let q_clone = Arc::clone(&q_arc);
        handles.push(thread::spawn(move || {
            for _ in 0..1_000 {
                q_clone.record_deallocation(500);
                q_clone.release_pin(500);
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    assert_eq!(q_arc.allocated_bytes(), 0);
    assert_eq!(q_arc.pinned_bytes(), 0);
    assert!(!q_arc.is_warning());
    assert!(!q_arc.is_over_quota());
    assert!(!q_arc.is_pinned_active());
}

// ============================================================================
// 3. WARNING THRESHOLD & DDSF FLAGS TRANSITIONS
// ============================================================================

#[test]
fn test_warning_and_quota_flag_transitions() {
    let q = MemoryQuotaManager::new(10_000);
    assert_eq!(q.warning_threshold_bytes(), 8_500);
    assert_eq!(q.pinned_limit_bytes(), 4_000);

    // At 8,499: No warning
    assert!(q.record_allocation(8_499));
    assert!(!q.is_warning());
    assert!(!q.is_over_quota());
    assert_eq!(q.flags(), 0);

    // At 8,500: Warning triggered exactly
    assert!(q.record_allocation(1));
    assert!(q.is_warning());
    assert!(!q.is_over_quota());
    assert_eq!(q.flags(), DDSF_FLAG_QUOTA_WARNING);

    // Pin 100 bytes: Both warning and pinned flags active
    assert!(q.try_pin(100).is_ok());
    assert!(q.is_pinned_active());
    assert_eq!(q.flags(), DDSF_FLAG_QUOTA_WARNING | DDSF_FLAG_PINNED_ACTIVE);

    // Deallocate 1 byte -> drops back to 8,499 (warning clears)
    q.record_deallocation(1);
    assert!(!q.is_warning());
    assert!(q.is_pinned_active());
    assert_eq!(q.flags(), DDSF_FLAG_PINNED_ACTIVE);

    // Release pinned bytes -> both clear
    q.release_pin(100);
    assert!(!q.is_pinned_active());
    assert_eq!(q.flags(), 0);

    // Over-quota transitions:
    // Re-allocate up to exactly 10,000 (quota limit)
    assert!(q.record_allocation(1_501)); // 8499 + 1501 = 10,000
    assert_eq!(q.allocated_bytes(), 10_000);
    assert!(!q.is_over_quota());

    // Allocate 1 byte beyond quota
    let within = q.record_allocation(1); // 10,001
    assert!(!within);
    assert!(q.is_over_quota());

    let stats = q.stats();
    assert!(stats.is_over_quota);
    assert!(stats.is_warning);
}

// ============================================================================
// 4. PANIC UNWINDING & RAII GUARD INVARIANTS
// ============================================================================

#[test]
fn test_panic_unwinding_releases_pinned_guard() {
    let q = Arc::new(MemoryQuotaManager::new(10_000));

    // Panic unwinding with single PinnedGuard
    let q_clone = Arc::clone(&q);
    let result = std::panic::catch_unwind(AssertUnwindSafe(move || {
        let _guard = q_clone.pin(2_500).unwrap();
        assert_eq!(q_clone.pinned_bytes(), 2_500);
        assert!(q_clone.is_pinned_active());
        panic!("simulated worker thread crash!");
    }));

    assert!(result.is_err(), "Thread must have panicked");
    assert_eq!(
        q.pinned_bytes(),
        0,
        "Pinned bytes must be restored to 0 after panic unwinding"
    );
    assert!(!q.is_pinned_active());

    // Panic unwinding with multiple nested guards
    let q_clone2 = Arc::clone(&q);
    let result2 = std::panic::catch_unwind(AssertUnwindSafe(move || {
        let _g1 = q_clone2.pin(1_000).unwrap();
        let _g2 = q_clone2.pin(1_500).unwrap();
        let _g3 = q_clone2.pin(500).unwrap();
        assert_eq!(q_clone2.pinned_bytes(), 3_000);
        panic!("simulated nested crash!");
    }));

    assert!(result2.is_err());
    assert_eq!(
        q.pinned_bytes(),
        0,
        "All nested pinned guards must release their reservation upon panic unwinding"
    );
}

// ============================================================================
// 5. HIGH-CONCURRENCY RACE CONDITION STRESS TEST
// ============================================================================

#[test]
fn test_high_concurrency_race_condition() {
    const NUM_THREADS: usize = 32;
    const OPS_PER_THREAD: usize = 2_000;
    const QUOTA_LIMIT: usize = 50_000_000; // 50MB
    let q = Arc::new(MemoryQuotaManager::new(QUOTA_LIMIT));
    let pinned_ceiling = q.pinned_limit_bytes();

    let barrier = Arc::new(Barrier::new(NUM_THREADS));
    let stop_signal = Arc::new(AtomicBool::new(false));
    let mut handles = Vec::new();

    // 8 threads allocating and deallocating memory
    for t_idx in 0..8 {
        let q_c = Arc::clone(&q);
        let b_c = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            b_c.wait();
            for i in 0..OPS_PER_THREAD {
                let size = 1_000 + (t_idx * 137 + i * 17) % 50_000;
                q_c.record_allocation(size);
                if i % 3 == 0 {
                    thread::yield_now();
                }
                q_c.record_deallocation(size);
            }
        }));
    }

    // 8 threads trying to pin and releasing via raw try_pin/release_pin
    for t_idx in 0..8 {
        let q_c = Arc::clone(&q);
        let b_c = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            b_c.wait();
            for i in 0..OPS_PER_THREAD {
                let size = 500 + (t_idx * 73 + i * 29) % 20_000;
                if q_c.try_pin(size).is_ok() {
                    let cur = q_c.pinned_bytes();
                    assert!(
                        cur <= pinned_ceiling,
                        "Race condition: pinned_bytes ({cur}) exceeded pinned_ceiling ({pinned_ceiling})"
                    );
                    if i % 2 == 0 {
                        thread::yield_now();
                    }
                    q_c.release_pin(size);
                }
            }
        }));
    }

    // 8 threads acquiring PinnedGuard (RAII drop)
    for t_idx in 0..8 {
        let q_c = Arc::clone(&q);
        let b_c = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            b_c.wait();
            for i in 0..OPS_PER_THREAD {
                let size = 1_000 + (t_idx * 97 + i * 31) % 30_000;
                if let Ok(guard) = q_c.pin(size) {
                    let cur = q_c.pinned_bytes();
                    assert!(
                        cur <= pinned_ceiling,
                        "Race condition: pinned_bytes ({cur}) exceeded pinned_ceiling ({pinned_ceiling})"
                    );
                    assert_eq!(guard.pinned_bytes(), size);
                    if i % 4 == 0 {
                        thread::yield_now();
                    }
                    drop(guard);
                }
            }
        }));
    }

    // 8 observer threads asserting global invariants at high frequency
    for _ in 0..8 {
        let q_c = Arc::clone(&q);
        let b_c = Arc::clone(&barrier);
        let stop_c = Arc::clone(&stop_signal);
        handles.push(thread::spawn(move || {
            b_c.wait();
            while !stop_c.load(Ordering::Relaxed) {
                // Assert invariant: pinned_bytes never exceeds pinned_ceiling
                let cur_pinned = q_c.pinned_bytes();
                assert!(
                    cur_pinned <= pinned_ceiling,
                    "Observer detected pinned_bytes ({cur_pinned}) > pinned_ceiling ({pinned_ceiling})"
                );

                let flags = q_c.flags();
                // Flags can only have valid defined bitmasks
                assert_eq!(
                    flags & !(DDSF_FLAG_QUOTA_WARNING | DDSF_FLAG_PINNED_ACTIVE),
                    0,
                    "Invalid undefined bit set in flags"
                );

                let stats = q_c.stats();
                assert!(stats.pinned_bytes <= pinned_ceiling);
                thread::yield_now();
            }
        }));
    }

    // Wait for the 24 mutating threads to finish
    for h in handles.drain(0..24) {
        h.join().unwrap();
    }

    // Signal observer threads to terminate
    stop_signal.store(true, Ordering::Relaxed);
    for h in handles {
        h.join().unwrap();
    }

    // Verify all counters have cleanly returned to 0
    assert_eq!(
        q.allocated_bytes(),
        0,
        "Leaked allocated_bytes after concurrent stress"
    );
    assert_eq!(
        q.pinned_bytes(),
        0,
        "Leaked pinned_bytes after concurrent stress"
    );
    assert!(!q.is_pinned_active());
    assert!(!q.is_warning());
    assert!(!q.is_over_quota());
    assert_eq!(q.flags(), 0);
}

// ============================================================================
// 6. MULTI-THREADED RAII GUARD HANDOFF & DROPS
// ============================================================================

#[test]
fn test_multi_threaded_pinned_guard_handoff() {
    let q = Arc::new(MemoryQuotaManager::new(10_000_000));
    let (tx, rx) = crossbeam_channel::unbounded::<PinnedGuard>();

    // Producer threads creating guards and sending across channel
    let mut prod_handles = Vec::new();
    for t_idx in 0..4 {
        let q_c = Arc::clone(&q);
        let tx_c = tx.clone();
        prod_handles.push(thread::spawn(move || {
            for i in 0..500 {
                let size = 1_000 + (t_idx * 50 + i) % 5_000;
                let guard = q_c.pin(size).unwrap();
                tx_c.send(guard).unwrap();
            }
        }));
    }
    drop(tx); // Close original sender

    // Consumer threads receiving guards on different threads and dropping them
    let mut cons_handles = Vec::new();
    for _ in 0..4 {
        let rx_c = rx.clone();
        cons_handles.push(thread::spawn(move || {
            while let Ok(guard) = rx_c.recv() {
                assert!(guard.pinned_bytes() > 0);
                drop(guard);
            }
        }));
    }

    for h in prod_handles {
        h.join().unwrap();
    }
    for h in cons_handles {
        h.join().unwrap();
    }

    assert_eq!(
        q.pinned_bytes(),
        0,
        "All guards transferred across threads must release pinned bytes"
    );
}

// ============================================================================
// 7. SERIES INTEGRATION & FIFO EVICTION UNDER QUOTA PRESSURE
// ============================================================================

#[test]
fn test_series_fifo_eviction_under_quota_pressure() {
    let mut series = NativeScalarSeries::new(ChannelId(10), ScalarType::F32, LinearScale::identity());

    // Approximate size of one sealed F32 chunk with M4 pyramid is ~524 KB.
    // Configure quota manager so it can hold roughly 3 chunks before overflowing.
    let single_chunk_bytes = 530_000;
    let quota = Arc::new(MemoryQuotaManager::new(single_chunk_bytes * 3));
    series.set_quota_manager(Arc::clone(&quota));

    let mut ts = Vec::with_capacity(CHUNK_LEN);
    let mut vals = Vec::with_capacity(CHUNK_LEN);

    // Append 8 chunks (will force FIFO eviction of oldest unpinned chunks)
    for c in 0..8 {
        ts.clear();
        vals.clear();
        let base_t = (c * CHUNK_LEN) as u64 * 10;
        for i in 0..CHUNK_LEN {
            ts.push(base_t + (i as u64) * 10);
            vals.push(i as f32);
        }
        series
            .append(&ts, ScalarSlice::F32(&vals))
            .expect("append chunk ok");
    }

    // Check that eviction occurred
    let evicted = series.evicted_samples();
    assert!(
        evicted > 0,
        "Eviction must have taken place under quota overflow"
    );
    assert_eq!(
        evicted % CHUNK_LEN as u64,
        0,
        "Evicted samples count must be integer multiple of CHUNK_LEN"
    );

    // Check resident sealed chunks count does not exceed quota capacity + 1 (active buffer)
    assert!(
        series.sealed_chunks().len() <= 4,
        "Resident sealed chunks ({}) must be bounded by quota",
        series.sealed_chunks().len()
    );

    // Verify O(1) global sample indexing:
    // Any index < evicted should return None
    assert_eq!(series.get_sample_by_index(0), None);
    assert_eq!(series.get_sample_by_index(evicted - 1), None);

    // Any index >= evicted should return valid sample
    let valid_sample = series.get_sample_by_index(evicted);
    assert!(valid_sample.is_some());
}

// ============================================================================
// 8. SERIES CHUNK PINNING PROTECTS HEAD FROM EVICTION
// ============================================================================

#[test]
fn test_pinned_chunk_resists_fifo_eviction() {
    let mut series = NativeScalarSeries::new(ChannelId(11), ScalarType::F32, LinearScale::identity());

    // 1 chunk is 524,288 bytes. 40% cap means quota must be at least 524,288 / 0.4 = 1,310,720 bytes.
    // Configure quota for ~3 chunks capacity = 1,600,000 bytes (pinned limit = 640,000 bytes).
    let quota = Arc::new(MemoryQuotaManager::new(1_600_000));
    series.set_quota_manager(Arc::clone(&quota));

    let mut ts = Vec::with_capacity(CHUNK_LEN);
    let mut vals = Vec::with_capacity(CHUNK_LEN);

    // Append chunk 0
    for i in 0..CHUNK_LEN {
        ts.push((i as u64) * 10);
        vals.push(i as f32);
    }
    series
        .append(&ts, ScalarSlice::F32(&vals))
        .expect("append chunk 0 ok");

    // Pin chunk 0
    let pinned_guard = series
        .pin_range(0, (CHUNK_LEN as u64 - 1) * 10)
        .expect("pin chunk 0 ok");
    assert_eq!(pinned_guard.chunk_count(), 1);
    assert!(quota.is_pinned_active());

    // Now append chunks 1, 2, 3, 4 (causing memory pressure: 5 chunks * 524KB = 2.6MB > 1.6MB quota)
    for c in 1..=4 {
        ts.clear();
        vals.clear();
        let base_t = (c * CHUNK_LEN) as u64 * 10;
        for i in 0..CHUNK_LEN {
            ts.push(base_t + (i as u64) * 10);
            vals.push(i as f32);
        }
        series
            .append(&ts, ScalarSlice::F32(&vals))
            .expect("append ok");
    }

    // Because chunk 0 is pinned, FIFO eviction should NOT have evicted chunk 0!
    assert_eq!(
        series.evicted_samples(),
        0,
        "Head chunk 0 is pinned and must NOT be evicted by unpinned FIFO eviction"
    );
    assert_eq!(
        series.sealed_chunks().front().unwrap().chunk_seq,
        0,
        "Oldest resident chunk must still be chunk seq 0"
    );

    // Drop the pinned guard
    drop(pinned_guard);
    assert!(!quota.is_pinned_active());

    // Now manually trigger eviction or append another chunk
    let evicted_handle = series.evict_oldest_unpinned();
    assert!(
        evicted_handle.is_some(),
        "Chunk 0 should now be unpinned and evictable"
    );
    assert_eq!(evicted_handle.unwrap().chunk_seq, 0);
    assert_eq!(series.evicted_samples(), CHUNK_LEN as u64);
}

// ============================================================================
// 9. CONCURRENT DROP ON EVICTED CHUNK RECONCILIATION
// ============================================================================

#[test]
fn test_concurrent_guard_drops_on_evicted_chunk() {
    for iter in 0..50 {
        let mut series = NativeScalarSeries::new(ChannelId(12), ScalarType::F32, LinearScale::identity());
        let quota = Arc::new(MemoryQuotaManager::new(50_000_000));
        series.set_quota_manager(Arc::clone(&quota));

        let mut ts = Vec::with_capacity(CHUNK_LEN);
        let mut vals = Vec::with_capacity(CHUNK_LEN);
        for i in 0..CHUNK_LEN {
            ts.push((i as u64) * 10);
            vals.push(i as f32);
        }
        series
            .append(&ts, ScalarSlice::F32(&vals))
            .expect("append chunk ok");

        // Create Guard 1 pinning chunk 0
        let guard1 = series
            .pin_range(0, (CHUNK_LEN as u64 - 1) * 10)
            .expect("pin guard 1 ok");

        // Create Guard 2 pinning chunk 0 (overlapping window)
        let guard2 = series
            .pin_range(0, 100)
            .expect("pin guard 2 ok");

        let resident_bytes = guard1.chunks()[0].byte_footprint();
        let initial_alloc = quota.allocated_bytes();

        // Force unconditional rollover eviction of chunk 0 from series
        let popped = series.evict_oldest();
        drop(popped);

        // Drop guard1 and guard2 concurrently across two threads with a barrier
        let barrier = Arc::new(Barrier::new(2));
        let b1 = Arc::clone(&barrier);
        let b2 = Arc::clone(&barrier);

        let h1 = thread::spawn(move || {
            b1.wait();
            drop(guard1);
        });

        let h2 = thread::spawn(move || {
            b2.wait();
            drop(guard2);
        });

        h1.join().unwrap();
        h2.join().unwrap();

        assert_eq!(
            quota.pinned_bytes(),
            0,
            "Pinned bytes must be 0 after dropping both guards (iter {iter})"
        );

        let final_alloc = quota.allocated_bytes();
        assert_eq!(
            final_alloc,
            initial_alloc.saturating_sub(resident_bytes),
            "Reconciliation mismatch in iter {iter}: expected {}, got {}",
            initial_alloc.saturating_sub(resident_bytes),
            final_alloc
        );
    }
}
