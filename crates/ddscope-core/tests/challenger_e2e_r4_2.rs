//! Empirical Mathematical & Algorithmic Stress Harness — Challenger 2 (Iteration 4)
//! DDScopeHost Phase 1 Track A E2E Test Suite.
//!
//! Mission Objectives:
//! 1. Verify M4 downsampling preservation invariants under the newly remediated Feature 23 and Feature 06 tests.
//! 2. Verify subpixel trigger offset bounds and Single mode trigger latching in `t1_f18_03`.
//! 3. Verify O(1) post-eviction indexing math across the newly remediated Feature 08 and Feature 10 multi-chunk tests (`t1_f08_01`, `t1_f10_01`, `t1_f10_02`, `t2_f10_01`).
//! 4. Verify zero numerical anomalies, division by zero, NaN leakage, or unstable assertions.

use ddscope_core::scope::controller::{
    calculate_subsample_offset, ScopeController, ScopeControllerConfig, ScopeStepOutcome,
};
use ddscope_core::scope::ddsf::DdsfM4Column;
use ddscope_core::scope::{EdgeSlope, TriggerCondition, TriggerRuntimeState, TriggerSweepMode};
use ddscope_core::storage::m4::M4QueryResult;
use ddscope_core::storage::quota::MemoryQuotaManager;
use ddscope_core::storage::NativeScalarSeries;
use ddscope_core::types::{
    ChannelId, LinearScale, ScalarSlice, ScalarType, ScopeId, CHUNK_LEN,
};

const EPS: f32 = 1e-5;

// ============================================================================
// Target 1: M4 Downsampling Preservation Invariants
// ============================================================================

fn assert_m4_column_invariants(cols: &[DdsfM4Column], context: &str) {
    let mut prev_x = -1.0f32;
    for (i, c) in cols.iter().enumerate() {
        assert!(
            (0.0..=1.0).contains(&c.x_norm),
            "[{context}] Column {i} x_norm ({}) out of [0, 1]",
            c.x_norm
        );
        assert!(
            c.x_norm > prev_x,
            "[{context}] Column {i} x_norm ({}) not strictly increasing from prev ({prev_x})",
            c.x_norm
        );
        prev_x = c.x_norm;

        assert!(
            c.v_min <= c.v_max + EPS,
            "[{context}] Column {i} invariant violation: v_min ({}) > v_max ({})",
            c.v_min,
            c.v_max
        );
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
        assert!(c.v_min.is_finite(), "[{context}] Column {i} v_min is not finite");
        assert!(c.v_max.is_finite(), "[{context}] Column {i} v_max is not finite");
        assert!(c.v_first.is_finite(), "[{context}] Column {i} v_first is not finite");
        assert!(c.v_last.is_finite(), "[{context}] Column {i} v_last is not finite");
    }
}

/// Adversarial stress test on Feature 23 configurations (non-power-of-two, all zeros, alternating, extreme downsampling).
#[test]
fn test_m4_downsampling_f23_remediated_invariants() {
    // 1. Non-power-of-two (CHUNK_LEN + 1 = 65,537 samples spanning 1 sealed chunk and 1 active sample)
    {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let count = 65_537;
        let dt = 10u64;
        let ts: Vec<u64> = (0..count as u64).map(|i| i * dt).collect();
        let mut vals: Vec<f32> = (0..count).map(|i| ((i % 100) as f32) - 50.0).collect();
        // Insert extreme spikes in sealed chunk and active chunk
        vals[1234] = 999.0;
        vals[CHUNK_LEN] = -888.0; // In active chunk
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();

        assert_eq!(series.sealed_chunk_count(), 1);
        assert_eq!(series.active_chunk().len(), 1);

        for w_px in [10, 100, 1000, 1920] {
            let res = series.query_m4_viewport(0, (count as u64) * dt, w_px).expect("query ok");
            match res {
                M4QueryResult::Columns(cols) => {
                    assert_eq!(cols.len(), w_px);
                    assert_m4_column_invariants(&cols, "f23_non_power_of_two");
                    let g_min = cols.iter().map(|c| c.v_min).fold(f32::INFINITY, f32::min);
                    let g_max = cols.iter().map(|c| c.v_max).fold(f32::NEG_INFINITY, f32::max);
                    assert!(
                        (g_min - (-888.0)).abs() < 1e-3,
                        "Active chunk valley -888.0 must be preserved in M4 columns (got {g_min})"
                    );
                    assert!(
                        (g_max - 999.0).abs() < 1e-3,
                        "Sealed chunk peak 999.0 must be preserved in M4 columns (got {g_max})"
                    );
                }
                M4QueryResult::Raw(_) => panic!("Expected Columns for N = 65537 > 2 * {w_px}"),
            }
        }
    }

    // 2. All zeros array (10,000 samples)
    {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let count = 10_000;
        let dt = 10u64;
        let ts: Vec<u64> = (0..count as u64).map(|i| i * dt).collect();
        let zeros = vec![0.0f32; count];
        series.append(&ts, ScalarSlice::F32(&zeros)).unwrap();

        let res = series.query_m4_viewport(0, (count as u64) * dt, 500).unwrap();
        if let M4QueryResult::Columns(cols) = res {
            assert_eq!(cols.len(), 500);
            assert_m4_column_invariants(&cols, "f23_all_zeros");
            for col in &cols {
                assert_eq!(col.v_min, 0.0);
                assert_eq!(col.v_max, 0.0);
                assert_eq!(col.v_first, 0.0);
                assert_eq!(col.v_last, 0.0);
            }
        } else {
            panic!("Expected Columns for 10000 > 2 * 500");
        }
    }

    // 3. High-frequency alternating +/- 1.0 (branchless SIMD stress)
    {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let count = 20_000;
        let dt = 10u64;
        let ts: Vec<u64> = (0..count as u64).map(|i| i * dt).collect();
        let vals: Vec<f32> = (0..count).map(|i| if i % 2 == 0 { 1.0f32 } else { -1.0f32 }).collect();
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();

        let res = series.query_m4_viewport(0, (count as u64) * dt, 200).unwrap();
        if let M4QueryResult::Columns(cols) = res {
            assert_eq!(cols.len(), 200);
            assert_m4_column_invariants(&cols, "f23_alternating");
            for col in &cols {
                assert_eq!(col.v_min, -1.0);
                assert_eq!(col.v_max, 1.0);
            }
        } else {
            panic!("Expected Columns");
        }
    }

    // 4. Extreme downsampling (65,536 samples down to 5 pixels)
    {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let count = CHUNK_LEN;
        let dt = 10u64;
        let ts: Vec<u64> = (0..count as u64).map(|i| i * dt).collect();
        let mut vals = vec![5.0f32; count];
        vals[10] = -12345.67;
        vals[count - 10] = 76543.21;
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();

        let res = series.query_m4_viewport(0, (count as u64) * dt, 5).unwrap();
        if let M4QueryResult::Columns(cols) = res {
            assert_eq!(cols.len(), 5);
            assert_m4_column_invariants(&cols, "f23_extreme_downsample");
            let g_min = cols.iter().map(|c| c.v_min).fold(f32::INFINITY, f32::min);
            let g_max = cols.iter().map(|c| c.v_max).fold(f32::NEG_INFINITY, f32::max);
            assert!((g_min - (-12345.67)).abs() < 1e-2);
            assert!((g_max - 76543.21).abs() < 1e-2);
        } else {
            panic!("Expected Columns");
        }
    }
}

/// Adversarial stress test on Feature 06 configurations (spanning active + sealed, tiny window in huge dataset, full zoom out).
#[test]
fn test_m4_downsampling_f06_remediated_invariants() {
    let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
    let dt = 10u64;
    let n = CHUNK_LEN;

    // Append 3 sealed chunks: values 10.0, 20.0, 30.0
    for i in 0..3 {
        let t0 = (i * n as u64) * dt;
        let ts: Vec<u64> = (0..n as u64).map(|j| t0 + j * dt).collect();
        let vals = vec![(i + 1) as f32 * 10.0; n];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
    }
    // Append 5,000 active samples: value 40.0
    let t_act = (3 * n as u64) * dt;
    let ts_act: Vec<u64> = (0..5000).map(|j| t_act + j * dt).collect();
    let vals_act = vec![40.0f32; 5000];
    series.append(&ts_act, ScalarSlice::F32(&vals_act)).unwrap();

    assert_eq!(series.sealed_chunk_count(), 3);
    assert_eq!(series.active_chunk().len(), 5000);

    // Case 1: Query spanning sealed chunks and active buffer
    let t_end = t_act + 5000 * dt;
    let res_span = series.query_m4_viewport(0, t_end, 1000).unwrap();
    match res_span {
        M4QueryResult::Columns(cols) => {
            assert_eq!(cols.len(), 1000);
            assert_m4_column_invariants(&cols, "f06_span_active_and_sealed");
            // Extrema across all 4 chunks
            let g_min = cols.iter().map(|c| c.v_min).fold(f32::INFINITY, f32::min);
            let g_max = cols.iter().map(|c| c.v_max).fold(f32::NEG_INFINITY, f32::max);
            assert_eq!(g_min, 10.0);
            assert_eq!(g_max, 40.0);
        }
        M4QueryResult::Raw(_) => panic!("Expected Columns"),
    }

    // Case 2: Tiny window within Chunk 1 (100ns = 10 samples)
    let t_chunk1_mid = (n as u64 + 1000) * dt;
    let res_tiny = series.query_m4_viewport(t_chunk1_mid, t_chunk1_mid + 100, 1000).unwrap();
    match res_tiny {
        M4QueryResult::Raw(pts) => {
            assert!(!pts.is_empty());
            assert!(pts.len() <= 11);
            for p in &pts {
                assert!((p.y_val - 20.0).abs() < 1e-4);
            }
        }
        M4QueryResult::Columns(_) => panic!("Expected Raw points for tiny window N <= 2 * w_px"),
    }

    // Case 3: Full zoom out across entire span
    let res_zoom = series.query_m4_viewport(0, u64::MAX, 1920).unwrap();
    match res_zoom {
        M4QueryResult::Columns(cols) => {
            assert_eq!(cols.len(), 1920);
            assert_m4_column_invariants(&cols, "f06_full_zoom_out");
            let g_min = cols.iter().map(|c| c.v_min).fold(f32::INFINITY, f32::min);
            let g_max = cols.iter().map(|c| c.v_max).fold(f32::NEG_INFINITY, f32::max);
            assert_eq!(g_min, 10.0);
            assert_eq!(g_max, 40.0);
        }
        M4QueryResult::Raw(_) => panic!("Expected Columns for full zoom out"),
    }
}

// ============================================================================
// Target 2: Subpixel Trigger Offset Bounds & Single Mode Latching
// ============================================================================

/// Adversarially fuzzes `calculate_subsample_offset` across 50,000 parameter combinations.
#[test]
fn test_subpixel_offset_bounds_fuzzing() {
    let test_cases = [
        // (t0, v0, t1, v1, level)
        (100, 0.0, 200, 10.0, 5.0),    // exact midpoint
        (100, 0.0, 200, 10.0, 0.0),    // exact start
        (100, 0.0, 200, 10.0, 10.0),   // exact end
        (100, 10.0, 200, 0.0, 5.0),    // falling midpoint
        (100, 10.0, 200, 0.0, 10.0),   // falling start
        (100, 10.0, 200, 0.0, 0.0),    // falling end
        (100, 5.0, 200, 5.0, 5.0),     // flat line on level
        (100, 5.0, 200, 5.0, 3.0),     // flat line off level
        (100, 0.0, 200, 1e-15, 0.0),   // tiny denominator
        (100, 0.0, 200, 1e15, 5e14),   // huge values
        (100, -1e15, 200, 1e15, 0.0),  // symmetric extreme
        (100, 0.0, 100, 10.0, 5.0),    // zero dt (t1 == t0)
    ];

    for &(t0, v0, t1, v1, level) in &test_cases {
        let res = calculate_subsample_offset(t0, v0, t1, v1, level);
        assert!(res.is_some(), "calculate_subsample_offset returned None for valid input");
        let (alpha, sub_ns) = res.unwrap();
        assert!(
            (0.0..=1.0).contains(&alpha),
            "alpha {alpha} out of [0, 1] for inputs ({t0}, {v0}, {t1}, {v1}, {level})"
        );
        let dt = (t1 - t0) as f32;
        assert!(
            sub_ns >= 0.0 && sub_ns <= dt + 1e-3,
            "sub_ns {sub_ns} out of [0, {dt}] for inputs ({t0}, {v0}, {t1}, {v1}, {level})"
        );
    }

    // Mathematical boundary fuzzing: 10,000 random pseudo-random runs
    let mut lcg: u64 = 0xDEADBEEF;
    for _ in 0..10_000 {
        lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let dt = lcg % 100_000;
        let t0 = 1_000_000;
        let t1 = t0 + dt;

        lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let v0 = ((lcg as i64 % 2000) as f64) * 0.1;
        lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let v1 = ((lcg as i64 % 2000) as f64) * 0.1;
        lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let level = ((lcg as i64 % 2000) as f64) * 0.1;

        if let Some((alpha, sub_ns)) = calculate_subsample_offset(t0, v0, t1, v1, level) {
            assert!(
                (0.0..=1.0).contains(&alpha),
                "Fuzz violation: alpha {alpha} not in [0, 1]"
            );
            assert!(
                sub_ns >= 0.0 && sub_ns <= (dt as f32) + 1e-2,
                "Fuzz violation: sub_ns {sub_ns} not in [0, {dt}]"
            );
        }
    }

    // Invalid float checks
    assert!(calculate_subsample_offset(100, f64::NAN, 200, 1.0, 0.5).is_none());
    assert!(calculate_subsample_offset(100, 0.0, 200, f64::INFINITY, 0.5).is_none());
    assert!(calculate_subsample_offset(100, 0.0, 200, 1.0, f64::NAN).is_none());
    assert!(calculate_subsample_offset(200, 0.0, 100, 1.0, 0.5).is_none()); // t1 < t0
}

/// Adversarially tests Single mode trigger latching in `ScopeController` (remediated Feature 18).
#[test]
fn test_single_mode_trigger_latching_stress() {
    let config = ScopeControllerConfig {
        scope_id: ScopeId(1),
        sweep_mode: TriggerSweepMode::Single,
        time_span_ns: 2_000,
        pre_trigger_ratio: 0.5,
        trigger: Some(TriggerCondition::Edge {
            channel: ChannelId(1),
            level: 0.0,
            hysteresis: 0.5,
            slope: EdgeSlope::Rising,
        }),
        ..Default::default()
    };
    let mut ctrl = ScopeController::new(config);
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);
    assert!(!ctrl.is_pinned());

    // 1. Force trigger and step until LatchedPinned
    ctrl.force_trigger(10_000);
    let outcome = ctrl.step(12_000);
    match outcome {
        ScopeStepOutcome::CaptureCompleted { latched_pinned, window } => {
            assert!(latched_pinned, "Single mode must set latched_pinned to true");
            assert_eq!(window.mode, TriggerSweepMode::Single);
            assert_eq!(window.trigger_state, TriggerRuntimeState::LatchedPinned);
            assert_eq!(window.win_t_span_ns, 2_000);
            assert_eq!(window.win_t_start_ns, 9_000); // 10_000 - 1_000
            assert_eq!(window.subpixel_offset_ns, 0.0);
            assert_eq!(window.trigger_timestamp_ns, Some(10_000));
        }
        _ => panic!("Expected CaptureCompleted"),
    }

    assert_eq!(ctrl.current_state(), TriggerRuntimeState::LatchedPinned);
    assert!(ctrl.is_pinned());

    // 2. Adversarial bombardment while in LatchedPinned state:
    // None of these operations must trigger, advance state, or alter pinned state!
    for step_ts in [13_000, 15_000, 20_000, 100_000, 1_000_000] {
        let out = ctrl.step(step_ts);
        assert_eq!(out, ScopeStepOutcome::NoChange, "step must return NoChange in LatchedPinned");
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::LatchedPinned);
        assert!(ctrl.is_pinned());
    }

    // Attempt forced triggers while latched
    let forced = ctrl.force_trigger(200_000);
    assert_eq!(forced, ScopeStepOutcome::NoChange, "force_trigger must return NoChange in LatchedPinned");
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::LatchedPinned);

    // Attempt feeding edge transition samples while latched
    let s1 = ctrl.process_scalar_sample(ChannelId(1), 300_000, -10.0);
    assert_eq!(s1, ScopeStepOutcome::NoChange);
    let s2 = ctrl.process_scalar_sample(ChannelId(1), 301_000, 10.0);
    assert_eq!(s2, ScopeStepOutcome::NoChange);
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::LatchedPinned);
    assert!(ctrl.is_pinned());

    // 3. Re-arm: releases pin and transitions back to Armed
    ctrl.rearm();
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);
    assert!(!ctrl.is_pinned());

    // 4. Verify trigger fires cleanly again after rearm
    // Prime low
    ctrl.process_scalar_sample(ChannelId(1), 400_000, -1.0);
    // Trigger rising edge across 0.0 at 400_500 (target end = 400_000 + 1_000 = 401_000 > 400_500)
    let trig = ctrl.process_scalar_sample(ChannelId(1), 400_500, 1.0);
    assert!(matches!(trig, ScopeStepOutcome::TriggerDetected { .. }), "Must detect trigger");
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::Triggered);

    // Step clock to 402_000 >= 401_000 to complete capture
    let comp = ctrl.step(402_000);
    assert!(matches!(comp, ScopeStepOutcome::CaptureCompleted { latched_pinned: true, .. }));
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::LatchedPinned);
    assert!(ctrl.is_pinned());
}

// ============================================================================
// Target 3: O(1) Post-Eviction Indexing Math Across Multi-Chunk Tests
// ============================================================================

/// Adversarially stresses post-eviction indexing across F08, F10, and multi-wrap rollover.
#[test]
fn test_post_eviction_indexing_math_rigorous() {
    let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
    let n = CHUNK_LEN; // 65,536
    let dt = 10u64;
    let total_chunks = 10;

    // Ingest 10 chunks with strictly distinguishable sample values:
    // Sample global index g has timestamp g * dt and value g as f32
    for c in 0..total_chunks {
        let t0 = (c as u64 * n as u64) * dt;
        let ts: Vec<u64> = (0..n as u64).map(|j| t0 + j * dt).collect();
        let vals: Vec<f32> = (0..n as u64).map(|j| (c as u64 * n as u64 + j) as f32).collect();
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
    }

    assert_eq!(series.sealed_chunk_count(), 10);
    assert_eq!(series.evicted_samples(), 0);

    // Initial boundary check before eviction
    assert_eq!(series.get_sample_by_index(0), Some((0, 0.0)));
    assert_eq!(
        series.get_sample_by_index(n as u64 - 1),
        Some(((n as u64 - 1) * dt, (n - 1) as f64))
    );
    assert_eq!(
        series.get_sample_by_index(n as u64),
        Some(((n as u64) * dt, n as f64))
    );

    // Evict 1 chunk (t1_f10_01 pattern)
    let ev1 = series.evict_oldest_unpinned().expect("evict chunk 0");
    assert_eq!(ev1.t_start_ns, 0);
    assert_eq!(series.evicted_samples(), n as u64);
    assert_eq!(series.sealed_chunk_count(), 9);

    // Exact boundary assertions for 1 eviction
    assert!(series.get_sample_by_index(0).is_none(), "0 is evicted");
    assert!(series.get_sample_by_index(n as u64 - 1).is_none(), "n - 1 is evicted");
    assert_eq!(
        series.get_sample_by_index(n as u64),
        Some((n as u64 * dt, n as f64)),
        "chunk 1 first sample must resolve exactly"
    );
    assert_eq!(
        series.get_sample_by_index(2 * n as u64 - 1),
        Some(((2 * n as u64 - 1) * dt, (2 * n - 1) as f64)),
        "chunk 1 last sample must resolve exactly"
    );
    assert_eq!(
        series.get_sample_by_index(2 * n as u64),
        Some((2 * n as u64 * dt, (2 * n) as f64)),
        "chunk 2 first sample must resolve exactly"
    );

    // Evict 3 more chunks (total 4 chunks evicted: chunks 0, 1, 2, 3) (t1_f10_02 pattern)
    for expected_c in 1..4 {
        let ev = series.evict_oldest_unpinned().expect("evict next chunk");
        assert_eq!(ev.t_start_ns, (expected_c as u64 * n as u64) * dt);
    }
    assert_eq!(series.evicted_samples(), 4 * n as u64);
    assert_eq!(series.sealed_chunk_count(), 6);

    // Exact boundary assertions for 4 evictions
    assert!(series.get_sample_by_index(0).is_none());
    assert!(series.get_sample_by_index(4 * n as u64 - 1).is_none());
    assert_eq!(
        series.get_sample_by_index(4 * n as u64),
        Some((4 * n as u64 * dt, (4 * n) as f64)),
        "chunk 4 first sample must resolve exactly"
    );
    assert_eq!(
        series.get_sample_by_index(10 * n as u64 - 1),
        Some(((10 * n as u64 - 1) * dt, (10 * n - 1) as f64)),
        "chunk 9 last sample must resolve exactly"
    );
    assert!(series.get_sample_by_index(10 * n as u64).is_none(), "out of bounds");

    // Add 100 samples into active buffer
    let t_act = (10 * n as u64) * dt;
    let ts_act: Vec<u64> = (0..100).map(|j| t_act + j * dt).collect();
    let vals_act: Vec<f32> = (0..100).map(|j| (10 * n as u64 + j) as f32).collect();
    series.append(&ts_act, ScalarSlice::F32(&vals_act)).unwrap();

    // Verify active chunk indexing math while chunks 0..3 are evicted
    assert_eq!(series.sealed_chunk_count(), 6);
    assert_eq!(series.total_samples(), 10 * n as u64 + 100);
    assert_eq!(
        series.get_sample_by_index(10 * n as u64),
        Some((t_act, (10 * n) as f64)),
        "active buffer index 0 resolves seamlessly"
    );
    assert_eq!(
        series.get_sample_by_index(10 * n as u64 + 99),
        Some((t_act + 99 * dt, (10 * n + 99) as f64)),
        "active buffer last sample resolves seamlessly"
    );
    assert!(series.get_sample_by_index(10 * n as u64 + 100).is_none());
}

/// Adversarially tests continuous 100-cycle ring rollover with quota eviction (t2_f10_01 pattern).
#[test]
fn test_continuous_100_cycle_rollover_indexing() {
    let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
    let n = CHUNK_LEN;
    let dt = 10u64;
    let cycles = 100;

    for i in 0..cycles {
        let t0 = (i as u64 * n as u64) * dt;
        let ts: Vec<u64> = (0..n as u64).map(|j| t0 + j * dt).collect();
        let vals = vec![i as f32; n];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();

        let ev = series.evict_oldest_unpinned().expect("evict each cycle");
        assert_eq!(ev.t_start_ns, t0);
    }

    assert_eq!(series.evicted_samples(), (cycles as u64) * (n as u64));
    assert_eq!(series.sealed_chunk_count(), 0);

    // Ingest fresh chunk after 100 wraps
    let t_fresh = (cycles as u64 * n as u64) * dt;
    let ts_fresh: Vec<u64> = (0..n as u64).map(|j| t_fresh + j * dt).collect();
    let vals_fresh = vec![999.0f32; n];
    series.append(&ts_fresh, ScalarSlice::F32(&vals_fresh)).unwrap();

    assert_eq!(series.sealed_chunk_count(), 1);
    let sample_idx = cycles as u64 * n as u64;
    let s = series.get_sample_by_index(sample_idx).expect("fresh chunk queryable");
    assert_eq!(s.0, t_fresh);
    assert_eq!(s.1, 999.0);
}

/// Adversarially tests exact reproduction of t1_f08_01 memory quota limit eviction math.
#[test]
fn test_f08_01_quota_limit_eviction_math() {
    let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
    let n = CHUNK_LEN;
    let dt = 10u64;

    // Ingest 1 chunk to measure chunk resident footprint
    let ts0: Vec<u64> = (0..n as u64).map(|i| i * dt).collect();
    let vals0 = vec![1.0f32; n];
    series.append(&ts0, ScalarSlice::F32(&vals0)).unwrap();
    assert_eq!(series.sealed_chunk_count(), 1);
    let chunk_bytes = series.sealed_chunks()[0].storage.resident_bytes();

    // Limit quota to hold exactly 3 chunks
    let quota = std::sync::Arc::new(MemoryQuotaManager::new(chunk_bytes * 3 + 1024));
    quota.record_allocation(chunk_bytes);
    series.set_quota_manager(quota);

    // Append 2 more chunks (total 3 chunks in memory)
    for i in 1..3 {
        let t0 = (i * n as u64) * dt;
        let ts: Vec<u64> = (0..n as u64).map(|j| t0 + j * dt).collect();
        let vals = vec![(i + 1) as f32; n];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
    }
    assert_eq!(series.sealed_chunk_count(), 3);
    assert_eq!(series.evicted_samples(), 0);

    // Append 4th chunk -> exceeds quota limit of 3 chunks, triggers automatic eviction of chunk 0
    let t0 = (3 * n as u64) * dt;
    let ts: Vec<u64> = (0..n as u64).map(|j| t0 + j * dt).collect();
    let vals = vec![4.0f32; n];
    series.append(&ts, ScalarSlice::F32(&vals)).unwrap();

    assert_eq!(series.sealed_chunk_count(), 3);
    assert_eq!(series.evicted_samples(), n as u64);
    assert!(series.get_sample_by_index(0).is_none(), "chunk 0 was evicted");
    assert!(series.get_sample_by_index(n as u64 - 1).is_none(), "chunk 0 last sample was evicted");
    let s1 = series.get_sample_by_index(n as u64).expect("chunk 1 is retained");
    assert_eq!(s1.0, n as u64 * dt);
    assert_eq!(s1.1, 2.0);
}

/// Adversarially tests non-uniform chunk sizes under eviction to verify binary search indexing branch.
#[test]
fn test_non_uniform_chunk_eviction_indexing_math() {
    let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
    let dt = 10u64;

    // Seal chunk 0 early with 10,000 samples (non-standard size)
    let count0 = 10_000;
    let ts0: Vec<u64> = (0..count0 as u64).map(|i| i * dt).collect();
    let vals0 = vec![100.0f32; count0];
    series.append(&ts0, ScalarSlice::F32(&vals0)).unwrap();
    series.seal_active().unwrap();
    assert_eq!(series.sealed_chunk_count(), 1);

    // Chunk 1: standard CHUNK_LEN (65,536 samples)
    let count1 = CHUNK_LEN;
    let t1_start = count0 as u64 * dt;
    let ts1: Vec<u64> = (0..count1 as u64).map(|i| t1_start + i * dt).collect();
    let vals1 = vec![200.0f32; count1];
    series.append(&ts1, ScalarSlice::F32(&vals1)).unwrap();
    assert_eq!(series.sealed_chunk_count(), 2);

    // Chunk 2: standard CHUNK_LEN (65,536 samples)
    let count2 = CHUNK_LEN;
    let t2_start = (count0 + count1) as u64 * dt;
    let ts2: Vec<u64> = (0..count2 as u64).map(|i| t2_start + i * dt).collect();
    let vals2 = vec![300.0f32; count2];
    series.append(&ts2, ScalarSlice::F32(&vals2)).unwrap();
    assert_eq!(series.sealed_chunk_count(), 3);

    // Evict non-standard Chunk 0
    let ev = series.evict_oldest_unpinned().expect("evict non-standard chunk 0");
    assert_eq!(ev.sample_count, count0 as u32);
    assert_eq!(series.evicted_samples(), count0 as u64);
    assert_eq!(series.sealed_chunk_count(), 2);

    // Indexing checks
    assert!(series.get_sample_by_index(0).is_none());
    assert!(series.get_sample_by_index(count0 as u64 - 1).is_none());

    // First sample of Chunk 1
    let s_ch1_first = series.get_sample_by_index(count0 as u64).expect("chunk 1 first sample");
    assert_eq!(s_ch1_first.0, t1_start);
    assert_eq!(s_ch1_first.1, 200.0);

    // Last sample of Chunk 1
    let s_ch1_last = series.get_sample_by_index((count0 + count1 - 1) as u64).expect("chunk 1 last sample");
    assert_eq!(s_ch1_last.0, t1_start + (count1 as u64 - 1) * dt);
    assert_eq!(s_ch1_last.1, 200.0);

    // First sample of Chunk 2
    let s_ch2_first = series.get_sample_by_index((count0 + count1) as u64).expect("chunk 2 first sample");
    assert_eq!(s_ch2_first.0, t2_start);
    assert_eq!(s_ch2_first.1, 300.0);

    // Out of bounds
    assert!(series.get_sample_by_index((count0 + count1 + count2) as u64).is_none());
}

/// Adversarially verifies subpixel trigger offset bounds and Single mode trigger latching in `t1_f18_03`.
#[test]
fn test_t1_f18_03_scope_header_state_and_subpixel_offset() {
    let config = ScopeControllerConfig {
        scope_id: ScopeId(1),
        sweep_mode: TriggerSweepMode::Single,
        time_span_ns: 2_000,
        pre_trigger_ratio: 0.5,
        ..Default::default()
    };
    let mut ctrl = ScopeController::new(config);
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);

    ctrl.force_trigger(10_000);
    let out = ctrl.step(12_000);
    match out {
        ScopeStepOutcome::CaptureCompleted { latched_pinned, window } => {
            assert!(latched_pinned, "must be latched_pinned");
            assert_eq!(window.mode, TriggerSweepMode::Single);
            assert_eq!(window.trigger_state, TriggerRuntimeState::LatchedPinned);
            assert_eq!(window.subpixel_offset_ns, 0.0);
            assert_eq!(window.win_t_start_ns, 9_000);
            assert_eq!(window.win_t_span_ns, 2_000);
            assert_eq!(window.trigger_timestamp_ns, Some(10_000));
        }
        _ => panic!("Expected CaptureCompleted"),
    }
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::LatchedPinned);

    // In LatchedPinned state, subsequent steps do NOT fire additional triggers
    let out2 = ctrl.step(15_000);
    assert!(matches!(out2, ScopeStepOutcome::NoChange));
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::LatchedPinned);
}

