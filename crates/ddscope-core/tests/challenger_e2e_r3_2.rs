//! Empirical Mathematical & Algorithmic Stress Harness — Challenger 2 (Round 3)
//! DDScopeHost Phase 1 Track A E2E Test Suite.
//!
//! Mission Objectives:
//! 1. Verify M4 downsampling preservation invariants (v_min <= v_first, v_min <= v_last, v_max >= v_first, v_max >= v_last)
//!    under multi-channel and multi-scope workloads.
//! 2. Verify subpixel trigger offset mathematical bounds (offset strictly in [0, dt]).
//! 3. Verify that the timebase calculations across synchronized scopes in t3_p14 and scenario tests preserve exact mathematical time alignment.
//! 4. Verify that O(1) post-eviction indexing math in t4_s03 and t1_f08_02 correctly maps global sample indices to surviving chunk offsets.
//! 5. Verify zero numerical anomalies, division by zero, NaN leakage, or unstable assertions.

use std::sync::Arc;
use std::thread;

use ddscope_core::scope::controller::{
    calculate_subsample_offset, ScopeController, ScopeControllerConfig, ScopeStepOutcome,
};
use ddscope_core::scope::ddsf::{
    DdsfFrameHeader, DdsfFramePacker, DdsfM4Column, DdsfScopePacketHeader, ScopeViewportPayload,
    TrackPayload, DDSF_FLAG_PINNED_ACTIVE, DDSF_FLAG_QUOTA_WARNING,
};
use ddscope_core::scope::sync::SyncGroupCoordinator;
use ddscope_core::scope::TriggerSweepMode;
use ddscope_core::storage::m4::{
    aggregate_buckets_to_columns, compute_track_stats_from_columns, M4QueryResult,
};
use ddscope_core::storage::quota::MemoryQuotaManager;
use ddscope_core::storage::NativeScalarSeries;
use ddscope_core::types::{
    ChannelId, LinearScale, M4Bucket, ScalarSlice, ScalarType, ScopeId, SyncGroupId, CHUNK_LEN,
};

const EPS: f32 = 1e-5;

// ============================================================================
// Target 1: M4 Downsampling Preservation Invariants
// Invariants:
// - v_min <= v_first
// - v_min <= v_last
// - v_max >= v_first
// - v_max >= v_last
// - v_min <= v_max
// ============================================================================

fn assert_m4_invariants(cols: &[DdsfM4Column], context: &str) {
    for (i, c) in cols.iter().enumerate() {
        assert!(
            c.v_min <= c.v_first + EPS,
            "[{context}] Column {i} invariant violation: v_min ({}) > v_first ({})",
            c.v_min,
            c.v_first
        );
        assert!(
            c.v_min <= c.v_last + EPS,
            "[{context}] Column {i} invariant violation: v_min ({}) > v_last ({})",
            c.v_min,
            c.v_last
        );
        assert!(
            c.v_max >= c.v_first - EPS,
            "[{context}] Column {i} invariant violation: v_max ({}) < v_first ({})",
            c.v_max,
            c.v_first
        );
        assert!(
            c.v_max >= c.v_last - EPS,
            "[{context}] Column {i} invariant violation: v_max ({}) < v_last ({})",
            c.v_max,
            c.v_last
        );
        assert!(
            c.v_min <= c.v_max + EPS,
            "[{context}] Column {i} invariant violation: v_min ({}) > v_max ({})",
            c.v_min,
            c.v_max
        );
        assert!(
            c.x_norm >= 0.0 && c.x_norm <= 1.0,
            "[{context}] Column {i} x_norm ({}) out of bounds [0, 1]",
            c.x_norm
        );
    }
}

#[test]
fn test_m4_invariants_across_multi_channel_scalar_types() {
    let scalar_types = [
        ScalarType::F32,
        ScalarType::F64,
        ScalarType::I16,
        ScalarType::I32,
        ScalarType::U8,
        ScalarType::U16,
    ];

    let dt = 10u64;
    let n = 10_000usize;
    let ts: Vec<u64> = (0..n as u64).map(|i| i * dt).collect();

    for (ch_idx, &st) in scalar_types.iter().enumerate() {
        let ch_id = ChannelId((ch_idx + 1) as u32);
        let mut series = NativeScalarSeries::new(ch_id, st, LinearScale::identity());

        match st {
            ScalarType::F32 => {
                let vals: Vec<f32> = (0..n).map(|i| ((i as f32) * 0.05).sin() * 50.0).collect();
                series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
            }
            ScalarType::F64 => {
                let vals: Vec<f64> = (0..n).map(|i| ((i as f64) * 0.05).cos() * 100.0).collect();
                series.append(&ts, ScalarSlice::F64(&vals)).unwrap();
            }
            ScalarType::I16 => {
                let vals: Vec<i16> = (0..n).map(|i| ((i % 200) as i16 - 100) * 10).collect();
                series.append(&ts, ScalarSlice::I16(&vals)).unwrap();
            }
            ScalarType::I32 => {
                let vals: Vec<i32> = (0..n).map(|i| (i as i32) * 2 - 5000).collect();
                series.append(&ts, ScalarSlice::I32(&vals)).unwrap();
            }
            ScalarType::U8 => {
                let vals: Vec<u8> = (0..n).map(|i| (i % 256) as u8).collect();
                series.append(&ts, ScalarSlice::U8(&vals)).unwrap();
            }
            ScalarType::U16 => {
                let vals: Vec<u16> = (0..n).map(|i| ((i * 7) % 65536) as u16).collect();
                series.append(&ts, ScalarSlice::U16(&vals)).unwrap();
            }
            _ => unreachable!(),
        }

        // Test varying viewport widths (coarse, prime, standard, fine)
        for &w_px in &[10, 37, 100, 256, 500, 1024, 2000] {
            let res = series.query_m4_viewport(0, (n as u64) * dt, w_px).unwrap();
            if let M4QueryResult::Columns(cols) = res {
                assert_eq!(cols.len(), w_px);
                assert_m4_invariants(&cols, &format!("st={st:?}, w_px={w_px}"));
            }
        }
    }
}

#[test]
fn test_m4_invariants_under_adversarial_waveforms() {
    let dt = 10u64;
    let n = 20_000usize;
    let ts: Vec<u64> = (0..n as u64).map(|i| i * dt).collect();

    // Adversarial Waveform 1: Extreme isolated spikes and dips at prime indices
    let mut vals1 = vec![0.0f32; n];
    vals1[0] = 999_999.0;       // start peak
    vals1[63] = -888_888.0;     // L1 boundary dip
    vals1[64] = 777_777.0;      // L1 boundary peak
    vals1[4095] = -666_666.0;   // L2 boundary dip
    vals1[4096] = 555_555.0;    // L2 boundary peak
    vals1[n - 1] = -444_444.0;  // tail dip

    let mut s1 = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
    s1.append(&ts, ScalarSlice::F32(&vals1)).unwrap();
    let q1 = s1.query_m4_viewport(0, (n as u64) * dt, 300).unwrap();
    if let M4QueryResult::Columns(cols) = q1 {
        assert_m4_invariants(&cols, "Adversarial Spikes & Dips");
    }

    // Adversarial Waveform 2: Constant signal (v_min == v_max == v_first == v_last everywhere)
    let vals2 = vec![42.42f32; n];
    let mut s2 = NativeScalarSeries::new(ChannelId(2), ScalarType::F32, LinearScale::identity());
    s2.append(&ts, ScalarSlice::F32(&vals2)).unwrap();
    let q2 = s2.query_m4_viewport(0, (n as u64) * dt, 500).unwrap();
    if let M4QueryResult::Columns(cols) = q2 {
        assert_m4_invariants(&cols, "Constant Signal");
        for c in &cols {
            assert!((c.v_min - 42.42).abs() < 1e-4);
            assert!((c.v_max - 42.42).abs() < 1e-4);
            assert!((c.v_first - 42.42).abs() < 1e-4);
            assert!((c.v_last - 42.42).abs() < 1e-4);
        }
    }

    // Adversarial Waveform 3: Highly oscillating alternating Nyquist signal (+1, -1, +1, -1)
    let vals3: Vec<f32> = (0..n).map(|i| if i % 2 == 0 { 10.0 } else { -10.0 }).collect();
    let mut s3 = NativeScalarSeries::new(ChannelId(3), ScalarType::F32, LinearScale::identity());
    s3.append(&ts, ScalarSlice::F32(&vals3)).unwrap();
    let q3 = s3.query_m4_viewport(0, (n as u64) * dt, 200).unwrap();
    if let M4QueryResult::Columns(cols) = q3 {
        assert_m4_invariants(&cols, "Nyquist Alternating Signal");
        for c in &cols {
            assert!(c.v_max >= 10.0 - EPS);
            assert!(c.v_min <= -10.0 + EPS);
        }
    }

    // Adversarial Waveform 4: Negative linear scale inversion (scale = -2.5, offset = 100.0)
    let neg_scale = LinearScale::new(-2.5, 100.0);
    let mut s4 = NativeScalarSeries::new(ChannelId(4), ScalarType::F32, neg_scale);
    let vals4: Vec<f32> = (0..n).map(|i| (i as f32) * 0.1).collect();
    s4.append(&ts, ScalarSlice::F32(&vals4)).unwrap();
    let q4 = s4.query_m4_viewport(0, (n as u64) * dt, 250).unwrap();
    if let M4QueryResult::Columns(cols) = q4 {
        assert_m4_invariants(&cols, "Negative Scale Inversion");
    }
}

#[test]
fn test_m4_invariants_sparse_buckets_with_zoh_carry_forward() {
    // Challenge bucket aggregation with sparse data where some pixel slots are initially empty
    // finish_pixel_slots must preserve v_min <= v_first, v_min <= v_last, v_max >= v_first, v_max >= v_last
    // across ZOH forward fill and leading backfill.
    let buckets = vec![
        M4Bucket {
            t_first_ns: 10_000,
            t_last_ns: 20_000,
            v_first: 5.0,
            v_min: 1.0,
            v_max: 9.0,
            v_last: 4.0,
        },
        M4Bucket {
            t_first_ns: 80_000,
            t_last_ns: 90_000,
            v_first: 12.0,
            v_min: 2.0,
            v_max: 15.0,
            v_last: 10.0,
        },
    ];

    let cols = aggregate_buckets_to_columns(&buckets, 0, 100_000, 100);
    assert_eq!(cols.len(), 100);
    assert_m4_invariants(&cols, "Sparse Buckets ZOH Invariant");

    // Compute track stats and verify validity
    let stats = compute_track_stats_from_columns(&cols);
    assert!(stats.min <= stats.max);
    assert!(stats.vpp >= 0.0);
    assert!(stats.rms >= 0.0);
}

// ============================================================================
// Target 2: Subpixel Trigger Offset Mathematical Bounds
// Invariant:
// For any calculate_subsample_offset(t0, v0, t1, v1, level) -> Some((alpha, offset_ns)):
// - 0.0 <= alpha <= 1.0
// - 0.0 <= offset_ns <= (t1 - t0)
// ============================================================================

#[test]
fn test_subpixel_offset_strict_mathematical_bounds() {
    // Property: offset_ns MUST be strictly within [0, dt] for all inputs
    let test_cases = [
        // (t0, v0, t1, v1, level, expected_alpha)
        (100u64, 0.0, 200u64, 10.0, 5.0, 0.5),
        (100, 0.0, 200, 10.0, 0.0, 0.0),          // lower bound touch
        (100, 0.0, 200, 10.0, 10.0, 1.0),         // upper bound touch
        (100, 10.0, 200, 0.0, 5.0, 0.5),          // falling slope
        (100, 10.0, 200, 0.0, 10.0, 0.0),         // falling lower bound touch
        (100, 10.0, 200, 0.0, 0.0, 1.0),          // falling upper bound touch
        (100, 0.0, 200, 10.0, -5.0, 0.0),         // overshoot below (clamped to 0.0)
        (100, 0.0, 200, 10.0, 15.0, 1.0),         // overshoot above (clamped to 1.0)
        (100, 5.0, 200, 5.0, 5.0, 0.0),           // flatline exact level
        (100, 5.0, 200, 5.0 + 1e-15, 5.0, 0.0),   // tiny denominator
        (100, 0.0, 100, 10.0, 5.0, 0.5),          // dt == 0
    ];

    for &(t0, v0, t1, v1, level, exp_alpha) in &test_cases {
        let (alpha, sub_ns) =
            calculate_subsample_offset(t0, v0, t1, v1, level).expect("must calculate");
        let dt = (t1 - t0) as f64;

        assert!(
            (0.0..=1.0).contains(&alpha),
            "alpha {alpha} out of bounds [0, 1] for case ({t0}, {v0}, {t1}, {v1}, {level})"
        );
        assert!(
            sub_ns >= 0.0 && (sub_ns as f64) <= dt + 1e-4,
            "sub_ns {sub_ns} out of bounds [0, {dt}]"
        );
        assert!(
            (alpha - exp_alpha).abs() < 1e-4,
            "alpha {alpha} differs from expected {exp_alpha}"
        );
    }
}

#[test]
fn test_subpixel_offset_randomized_fuzzing_oracle() {
    // 50,000 iterations of adversarial pseudo-random inputs testing mathematical invariants
    let mut state = 0x9876_5432_10FE_DCBA_u64;
    let mut next_f64 = |min_val: f64, max_val: f64| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let unit = (state as f64) / (u64::MAX as f64);
        min_val + unit * (max_val - min_val)
    };

    for _ in 0..50_000 {
        let t0 = (next_f64(0.0, 1_000_000_000.0)) as u64;
        let dt = (next_f64(0.0, 100_000.0)) as u64;
        let t1 = t0 + dt;

        let v0 = next_f64(-1000.0, 1000.0);
        let v1 = next_f64(-1000.0, 1000.0);
        let level = next_f64(-1500.0, 1500.0);

        if let Some((alpha, sub_ns)) = calculate_subsample_offset(t0, v0, t1, v1, level) {
            assert!(
                (0.0..=1.0).contains(&alpha),
                "Fuzz invariant failed: alpha = {alpha} not in [0, 1]"
            );
            assert!(
                sub_ns >= 0.0 && (sub_ns as f64) <= (dt as f64) + 1e-3,
                "Fuzz invariant failed: sub_ns = {sub_ns} not in [0, {dt}]"
            );
            let expected_sub = (alpha * (dt as f64)) as f32;
            assert!(
                (sub_ns - expected_sub).abs() <= 1e-3,
                "Fuzz invariant failed: sub_ns {sub_ns} != expected {expected_sub}"
            );
        }
    }
}

#[test]
fn test_subpixel_offset_rejection_of_invalid_inputs() {
    // Inverted time (t1 < t0)
    assert!(calculate_subsample_offset(200, 1.0, 100, 5.0, 2.0).is_none());

    // Non-finite values
    assert!(calculate_subsample_offset(100, f64::NAN, 200, 5.0, 2.0).is_none());
    assert!(calculate_subsample_offset(100, 1.0, 200, f64::NAN, 2.0).is_none());
    assert!(calculate_subsample_offset(100, 1.0, 200, 5.0, f64::NAN).is_none());
    assert!(calculate_subsample_offset(100, f64::INFINITY, 200, 5.0, 2.0).is_none());
    assert!(calculate_subsample_offset(100, 1.0, 200, f64::NEG_INFINITY, 2.0).is_none());
    assert!(calculate_subsample_offset(100, 1.0, 200, 5.0, f64::INFINITY).is_none());
}

// ============================================================================
// Target 3: Timebase Calculations Across Synchronized Scopes
// Tests:
// - Exact zero-parallax timebase alignment across scopes in t3_p14 and scenarios
// - Window calculation consistency (Rolling, Freerun, Triggered)
// - Concurrency and atomic updates
// ============================================================================

#[test]
fn test_timebase_exact_mathematical_alignment_multi_scope() {
    let sync = SyncGroupCoordinator::new();
    let gid = SyncGroupId(1);
    let s1 = ScopeId(1);
    let s2 = ScopeId(2);
    let s3 = ScopeId(3);

    sync.register_scope(s1, gid);
    sync.register_scope(s2, gid);
    sync.register_scope(s3, gid);

    let time_span_ns = 50_000_000u64; // 50 ms
    let pan_offset_ns = 5_000_000i64;  // +5 ms
    sync.update_timebase(gid, time_span_ns, pan_offset_ns);

    let mut cfg1 = ScopeControllerConfig {
        scope_id: s1,
        sweep_mode: TriggerSweepMode::Roll,
        ..Default::default()
    };
    let mut cfg2 = ScopeControllerConfig {
        scope_id: s2,
        sweep_mode: TriggerSweepMode::Roll,
        ..Default::default()
    };
    let mut cfg3 = ScopeControllerConfig {
        scope_id: s3,
        sweep_mode: TriggerSweepMode::Roll,
        ..Default::default()
    };

    assert!(sync.apply_to_controller_config(s1, &mut cfg1));
    assert!(sync.apply_to_controller_config(s2, &mut cfg2));
    assert!(sync.apply_to_controller_config(s3, &mut cfg3));

    // Verify exact equality of timebase configs
    assert_eq!(cfg1.time_span_ns, time_span_ns);
    assert_eq!(cfg1.pan_offset_ns, pan_offset_ns);
    assert_eq!(cfg2.time_span_ns, time_span_ns);
    assert_eq!(cfg2.pan_offset_ns, pan_offset_ns);
    assert_eq!(cfg3.time_span_ns, time_span_ns);
    assert_eq!(cfg3.pan_offset_ns, pan_offset_ns);

    let mut ctrl1 = ScopeController::new(cfg1);
    let mut ctrl2 = ScopeController::new(cfg2);

    // Roll mode alignment: feed both controllers the same latest timestamp
    let t_now = 200_000_000u64;
    let out1 = ctrl1.step(t_now);
    let out2 = ctrl2.step(t_now);

    match (out1, out2) {
        (ScopeStepOutcome::Rolling { window: w1 }, ScopeStepOutcome::Rolling { window: w2 }) => {
            // win_start = (200_000_000 - 50_000_000) + 5_000_000 = 155_000_000
            assert_eq!(w1.win_t_start_ns, 155_000_000);
            assert_eq!(w2.win_t_start_ns, 155_000_000);
            assert_eq!(w1.win_t_span_ns, time_span_ns);
            assert_eq!(w2.win_t_span_ns, time_span_ns);
            assert_eq!(w1.win_t_start_ns, w2.win_t_start_ns);
            assert_eq!(w1.win_t_span_ns, w2.win_t_span_ns);
        }
        _ => panic!("Expected Rolling outcomes for both scopes"),
    }
}

#[test]
fn test_sync_group_atomic_state_consistency_under_high_concurrency() {
    let coord = Arc::new(SyncGroupCoordinator::new());
    let gid = SyncGroupId(99);
    let threads = 10;
    let iters_per_thread = 500;

    let mut handles = Vec::new();
    for t in 0..threads {
        let c = Arc::clone(&coord);
        let h = thread::spawn(move || {
            let s_id = ScopeId(t as u32 + 1);
            c.register_scope(s_id, gid);

            for i in 0..iters_per_thread {
                let span = 10_000_000 + ((t * 100 + i) as u64) * 1000;
                let offset = (i as i64) * 10 - 2000;
                c.update_timebase(gid, span, offset);
                c.update_cursors(
                    gid,
                    Some(((i % 100) as f64) * 0.005),
                    Some(0.5 + ((i % 100) as f64) * 0.005),
                );

                let state = c.get_group_state(gid).expect("state must exist");
                assert!(state.time_span_ns >= 10_000_000);
                if let (Some(ca), Some(cb)) = (state.cursor_a_ratio, state.cursor_b_ratio) {
                    assert!((0.0..=1.0).contains(&ca));
                    assert!((0.0..=1.0).contains(&cb));
                }
            }
        });
        handles.push(h);
    }

    for h in handles {
        h.join().expect("thread completed successfully");
    }

    assert_eq!(coord.scope_count(), threads);
    let final_state = coord.get_group_state(gid).expect("final state exists");
    assert!(final_state.version >= (threads * iters_per_thread) as u64);
}

// ============================================================================
// Target 4: O(1) Post-Eviction Indexing Math
// Tests:
// - Exact index mapping after FIFO eviction
// - Evicted indices [0, evicted_samples - 1] return None
// - Surviving indices [evicted_samples, total_sealed - 1] return exact values
// - Active tail indices return exact values
// - Boundary condition: sample at exactly evicted_samples
// ============================================================================

#[test]
fn test_post_eviction_o1_indexing_math_exhaustive() {
    let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
    let quota = Arc::new(MemoryQuotaManager::new_2gb());
    series.set_quota_manager(Arc::clone(&quota));

    let n = CHUNK_LEN; // 65,536
    let dt = 10u64;
    let total_chunks = 8usize;

    // Append 8 chunks (524,288 samples). Each chunk j has sample values = (j * 10.0 + k * 0.001)
    for j in 0..total_chunks {
        let t_start = (j * n) as u64 * dt;
        let ts: Vec<u64> = (0..n as u64).map(|k| t_start + k * dt).collect();
        let vals: Vec<f32> = (0..n).map(|k| (j as f32) * 10.0 + (k as f32) * 0.0001).collect();
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
    }
    assert_eq!(series.sealed_chunk_count(), total_chunks);
    assert_eq!(series.evicted_samples(), 0);

    // Evict 3 chunks (chunk 0, 1, 2 = 196,608 samples)
    for expected_chunk in 0..3 {
        let evicted = series.evict_oldest_unpinned().expect("eviction succeeds");
        assert_eq!(
            evicted.t_start_ns,
            (expected_chunk * n) as u64 * dt,
            "FIFO eviction must pop oldest chunk in order"
        );
    }

    let evicted_count = (3 * n) as u64; // 196,608
    assert_eq!(series.evicted_samples(), evicted_count);
    assert_eq!(series.sealed_chunk_count(), 5);

    // Check 1: Evicted indices strictly return None
    assert!(series.get_sample_by_index(0).is_none());
    assert!(series.get_sample_by_index(1).is_none());
    assert!(series.get_sample_by_index(evicted_count - 1).is_none());

    // Check 2: Exact surviving boundary (index == evicted_samples) MUST return Some
    let s_first_surviving = series
        .get_sample_by_index(evicted_count)
        .expect("exact eviction boundary sample must be present");
    assert_eq!(s_first_surviving.0, evicted_count * dt);
    assert!((s_first_surviving.1 - 30.0).abs() < 1e-4); // chunk 3 start value

    // Check 3: Check samples across all 5 surviving sealed chunks
    for chunk_offset in 0..5 {
        let global_chunk_idx = 3 + chunk_offset;
        let test_k = 1234usize;
        let global_idx = (global_chunk_idx * n + test_k) as u64;

        let sample = series
            .get_sample_by_index(global_idx)
            .unwrap_or_else(|| panic!("sample at global index {global_idx} must exist"));

        let expected_ts = global_idx * dt;
        let expected_val = (global_chunk_idx as f32) * 10.0 + (test_k as f32) * 0.0001;

        assert_eq!(sample.0, expected_ts, "Timestamp mismatch at index {global_idx}");
        assert!(
            (sample.1 - expected_val as f64).abs() < 1e-4,
            "Value mismatch at index {global_idx}: got {}, expected {}",
            sample.1,
            expected_val
        );
    }

    // Check 4: Add active chunk data and check indexing across sealed-active boundary
    let active_samples = 500usize;
    let t_act_start = (total_chunks * n) as u64 * dt;
    let ts_act: Vec<u64> = (0..active_samples as u64).map(|k| t_act_start + k * dt).collect();
    let vals_act: Vec<f32> = (0..active_samples).map(|k| 99.0 + (k as f32) * 0.01).collect();
    series.append(&ts_act, ScalarSlice::F32(&vals_act)).unwrap();

    let total_samples = series.total_samples();
    assert_eq!(total_samples, (total_chunks * n + active_samples) as u64);

    // Active sample check
    let s_active = series
        .get_sample_by_index((total_chunks * n + 10) as u64)
        .expect("active sample must exist");
    assert_eq!(s_active.0, t_act_start + 10 * dt);
    assert!((s_active.1 - (99.0 + 0.10)).abs() < 1e-4);

    // Out of bounds check
    assert!(series.get_sample_by_index(total_samples).is_none());
    assert!(series.get_sample_by_index(total_samples + 100).is_none());
    assert!(series.get_sample_by_index(u64::MAX).is_none());
}

// ============================================================================
// Target 5: Numerical Stability and Zero Anomalies
// Stress testing DDSF Frame Packing and Unpacking consistency under full load
// ============================================================================

#[test]
fn test_ddsf_frame_packer_and_unpacking_numerical_stability() {
    let mut packer = DdsfFramePacker::new(32768);

    for seq in 1..=50 {
        let cols: Vec<DdsfM4Column> = (0..200)
            .map(|i| {
                let x = (i as f32) / 200.0;
                let v = ((seq as f32) + x).sin() * 100.0;
                DdsfM4Column {
                    x_norm: x,
                    v_first: v,
                    v_min: v - 5.0,
                    v_max: v + 5.0,
                    v_last: v + 1.0,
                }
            })
            .collect();
        let track = TrackPayload::new_m4(1, cols, None);

        let scope = ScopeViewportPayload {
            scope_id: 1,
            trigger_state: 2,
            mode: 1,
            win_t_start_ns: (seq as u64) * 1_000_000,
            win_t_span_ns: 10_000_000,
            subpixel_offset_ns: 25.5,
            tracks: vec![track],
        };

        let flags = if seq % 2 == 0 {
            DDSF_FLAG_QUOTA_WARNING
        } else {
            DDSF_FLAG_PINNED_ACTIVE
        };

        let buf = packer.pack_frame(seq, 12345, flags, &[scope], &[]).unwrap();
        assert_eq!((buf.as_ptr() as usize) % 64, 0, "64-byte alignment failed");

        let hdr: &DdsfFrameHeader = bytemuck::from_bytes(&buf[0..64]);
        assert_eq!(hdr.frame_seq, seq);
        assert_eq!(hdr.scope_count, 1);
        assert_eq!(hdr.flags, flags);

        let scope_hdr: &DdsfScopePacketHeader = bytemuck::from_bytes(&buf[64..96]);
        assert_eq!(scope_hdr.scope_id, 1);
        assert_eq!(scope_hdr.subpixel_offset_ns, 25.5);
    }
}
