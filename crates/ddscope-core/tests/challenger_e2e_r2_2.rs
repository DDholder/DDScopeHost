//! Empirical Mathematical & Algorithmic Stress Harness — Challenger 2 (Iteration 2)
//! DDScopeHost Phase 1 Track A E2E Test Suite.
//!
//! Oracles under empirical test:
//! 1. Mathematical Extrema Retention Oracle (M4 reduction across chunks, buckets, active tails, pyramid levels & viewports)
//! 2. Sub-sample Trigger Precision Oracle (calculate_subsample_offset exact linear interpolation, boundary touches, steep/minimal slopes)
//! 3. Multi-Scope Alignment Oracle (SyncGroupCoordinator timestamp alignment, cursor invariance, and jitter tolerance)

use std::f64::consts::PI;
use std::sync::Arc;
use std::thread;

use ddscope_core::scope::controller::{
    calculate_subsample_offset, ScopeControllerConfig, TriggerHit,
};
use ddscope_core::scope::sync::SyncGroupCoordinator;
use ddscope_core::storage::m4::{
    reduce_raw_f32_slice, reduce_raw_i16_slice, reduce_raw_u8_slice, M4QueryResult,
};
use ddscope_core::storage::NativeScalarSeries;
use ddscope_core::types::{
    ChannelId, LinearScale, ScalarSlice, ScalarType, ScopeId, SyncGroupId, CHUNK_LEN,
};

const SAMPLING_INTERVAL_NS: u64 = 10;

// ============================================================================
// 1. Mathematical Extrema Retention Oracle
// ============================================================================

#[test]
fn test_m4_extrema_retention_exhaustive_boundary_matrix() {
    // Tests that single-sample positive peaks and negative valleys are preserved
    // at all critical structural boundaries:
    // chunk start/end, L1 bucket edges (63/64), L2 bucket edges (4095/4096).
    let boundary_indices = [
        0, 1, 2, 62, 63, 64, 65, 127, 128, 4095, 4096, 4097, 8191, 8192, 65534, 65535,
    ];
    let peak_val = 123_456.0f32;
    let valley_val = -654_321.0f32;

    for &idx in &boundary_indices {
        // --- Positive Peak Challenge ---
        let mut series_peak =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::default());
        let mut ts = Vec::with_capacity(CHUNK_LEN);
        let mut vals_p = vec![0.0f32; CHUNK_LEN];
        for i in 0..CHUNK_LEN {
            ts.push((i as u64) * SAMPLING_INTERVAL_NS);
        }
        vals_p[idx] = peak_val;
        series_peak
            .append(&ts, ScalarSlice::F32(&vals_p))
            .expect("append ok");

        let t_min = 0;
        let t_max = (CHUNK_LEN as u64) * SAMPLING_INTERVAL_NS;
        let res_p = series_peak
            .query_m4_viewport(t_min, t_max, 2000)
            .expect("query ok");

        match res_p {
            M4QueryResult::Columns(cols) => {
                let max_observed = cols.iter().map(|c| c.v_max).fold(f32::NEG_INFINITY, f32::max);
                assert_eq!(
                    max_observed, peak_val,
                    "Peak at boundary index {idx} was lost in M4 decimation! max_observed={max_observed}"
                );
            }
            M4QueryResult::Raw(_) => panic!("Expected Columns for 65536 samples > 2 * 2000"),
        }

        // --- Negative Valley Challenge ---
        let mut series_valley =
            NativeScalarSeries::new(ChannelId(2), ScalarType::F32, LinearScale::default());
        let mut vals_v = vec![0.0f32; CHUNK_LEN];
        vals_v[idx] = valley_val;
        series_valley
            .append(&ts, ScalarSlice::F32(&vals_v))
            .expect("append ok");

        let res_v = series_valley
            .query_m4_viewport(t_min, t_max, 2000)
            .expect("query ok");

        match res_v {
            M4QueryResult::Columns(cols) => {
                let min_observed = cols.iter().map(|c| c.v_min).fold(f32::INFINITY, f32::min);
                assert_eq!(
                    min_observed, valley_val,
                    "Valley at boundary index {idx} was lost in M4 decimation! min_observed={min_observed}"
                );
            }
            M4QueryResult::Raw(_) => panic!("Expected Columns"),
        }
    }
}

#[test]
fn test_m4_extrema_retention_all_viewport_widths() {
    // Challenge M4 decimation across extreme and prime viewport widths
    let mut series =
        NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::default());
    let mut ts = Vec::with_capacity(CHUNK_LEN);
    let mut vals = vec![10.0f32; CHUNK_LEN];
    for i in 0..CHUNK_LEN {
        ts.push((i as u64) * SAMPLING_INTERVAL_NS);
    }
    let spike_idx = 7777;
    let spike_val = 98_765.0f32;
    vals[spike_idx] = spike_val;
    series.append(&ts, ScalarSlice::F32(&vals)).expect("append ok");

    let t_min = 0;
    let t_max = (CHUNK_LEN as u64) * SAMPLING_INTERVAL_NS;

    // Test a wide spectrum of pixel widths
    let test_widths = [1, 2, 3, 7, 17, 32, 64, 100, 256, 500, 1000, 1999, 2000, 4096, 32768, 65536];

    for &w_px in &test_widths {
        let res = series.query_m4_viewport(t_min, t_max, w_px).expect("query ok");
        match res {
            M4QueryResult::Columns(cols) => {
                assert_eq!(cols.len(), w_px, "Column count mismatch for w_px={w_px}");
                let max_observed = cols.iter().map(|c| c.v_max).fold(f32::NEG_INFINITY, f32::max);
                assert_eq!(
                    max_observed, spike_val,
                    "Spike missed for viewport width {w_px}! max_observed={max_observed}"
                );
            }
            M4QueryResult::Raw(pts) => {
                // Occurs when N <= 2 * w_px (e.g. w_px >= 32768)
                assert!(
                    CHUNK_LEN <= 2 * w_px,
                    "Raw mode returned but N > 2 * w_px for w_px={w_px}"
                );
                let max_observed = pts.iter().map(|p| p.y_val).fold(f32::NEG_INFINITY, f32::max);
                assert_eq!(
                    max_observed, spike_val,
                    "Spike missed in Raw mode for w_px={w_px}"
                );
            }
        }
    }
}

#[test]
fn test_m4_extrema_retention_active_chunk_and_unsealed_tail() {
    // Challenge active chunk and unsealed partial tail handling:
    // A series with partial active data (< 64 samples, > 64 samples, etc.)
    let partial_lens = [1, 10, 63, 64, 65, 127, 128, 500, 4095, 4096, 5000];

    for &len in &partial_lens {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::default());
        let mut ts = Vec::with_capacity(len);
        let mut vals = vec![5.0f32; len];
        for i in 0..len {
            ts.push((i as u64) * 10);
        }
        let spike_idx = len - 1; // spike at the very last sample of active tail
        let spike_val = 888_888.0f32;
        vals[spike_idx] = spike_val;

        series.append(&ts, ScalarSlice::F32(&vals)).expect("append ok");

        // Query with small w_px so that if len > 2 * w_px it tests Columns mode
        let w_px = (len / 3).max(1);
        let t_min = 0;
        let t_max = ((len - 1) as u64) * 10;
        if t_max > t_min {
            let res = series.query_m4_viewport(t_min, t_max, w_px).expect("query ok");
            match res {
                M4QueryResult::Columns(cols) => {
                    let max_observed = cols.iter().map(|c| c.v_max).fold(f32::NEG_INFINITY, f32::max);
                    assert_eq!(
                        max_observed, spike_val,
                        "Spike at active tail (len={len}, w_px={w_px}) was lost in Columns mode"
                    );
                }
                M4QueryResult::Raw(pts) => {
                    let max_observed = pts.iter().map(|p| p.y_val).fold(f32::NEG_INFINITY, f32::max);
                    assert_eq!(
                        max_observed, spike_val,
                        "Spike at active tail (len={len}, w_px={w_px}) was lost in Raw mode"
                    );
                }
            }
        }
    }
}

#[test]
fn test_m4_extrema_retention_multi_level_pyramid_hierarchy() {
    // Challenge inter-chunk L3 hierarchy (4 chunks = 262,144 samples)
    let mut series =
        NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::default());

    let num_chunks = 5;
    let spike_chunk = 3;
    let spike_local_idx = 1000;
    let spike_val = 7_777_777.0f32;

    for chunk_idx in 0..num_chunks {
        let mut ts = Vec::with_capacity(CHUNK_LEN);
        let mut vals = vec![1.0f32; CHUNK_LEN];
        let base_t = (chunk_idx * CHUNK_LEN) as u64 * SAMPLING_INTERVAL_NS;
        for i in 0..CHUNK_LEN {
            ts.push(base_t + (i as u64) * SAMPLING_INTERVAL_NS);
        }
        if chunk_idx == spike_chunk {
            vals[spike_local_idx] = spike_val;
        }
        series.append(&ts, ScalarSlice::F32(&vals)).expect("append ok");
    }

    // Force coarsest level L3 by querying 50 pixels over 5 chunks (327,680 samples / 50 = 6553 samples/px >= 4096)
    let t_min = 0;
    let t_max = (num_chunks * CHUNK_LEN) as u64 * SAMPLING_INTERVAL_NS;
    let res = series.query_m4_viewport(t_min, t_max, 50).expect("query ok");

    match res {
        M4QueryResult::Columns(cols) => {
            let max_observed = cols.iter().map(|c| c.v_max).fold(f32::NEG_INFINITY, f32::max);
            assert_eq!(
                max_observed, spike_val,
                "Multi-level L3 pyramid did not preserve peak extrema: max_observed={max_observed}"
            );
        }
        M4QueryResult::Raw(_) => panic!("Expected Columns for 327,680 samples > 2 * 50"),
    }
}

#[test]
fn test_m4_simd_reductions_across_scalar_types_and_inversion() {
    // Challenge SIMD reduction math directly with scale inversion
    let neg_scale = LinearScale {
        scale: -3.0,
        offset: 100.0,
    };

    // f32
    let raw_f32 = [-10.0f32, 0.0, 50.0, -25.0, 100.0];
    let b_f32 = reduce_raw_f32_slice(&raw_f32, 100, 200, &neg_scale);
    // Raw min = -25.0 -> calibrated: -25.0 * (-3.0) + 100.0 = 175.0 (v_max)
    // Raw max = 100.0 -> calibrated: 100.0 * (-3.0) + 100.0 = -200.0 (v_min)
    // First = -10.0 -> calibrated: 130.0
    // Last = 100.0 -> calibrated: -200.0
    assert_eq!(b_f32.v_first, 130.0);
    assert_eq!(b_f32.v_last, -200.0);
    assert_eq!(b_f32.v_min, -200.0);
    assert_eq!(b_f32.v_max, 175.0);

    // i16
    let raw_i16 = [-1000i16, 0, 500, -2000, 3000];
    let b_i16 = reduce_raw_i16_slice(&raw_i16, 100, 200, &neg_scale);
    // Raw min = -2000 -> calibrated: -2000 * (-3.0) + 100 = 6100.0 (v_max)
    // Raw max = 3000 -> calibrated: 3000 * (-3.0) + 100 = -8900.0 (v_min)
    assert_eq!(b_i16.v_max, 6100.0);
    assert_eq!(b_i16.v_min, -8900.0);

    // u8
    let raw_u8 = [10u8, 20, 255, 0, 50];
    let b_u8 = reduce_raw_u8_slice(&raw_u8, 100, 200, &LinearScale::default());
    assert_eq!(b_u8.v_min, 0.0);
    assert_eq!(b_u8.v_max, 255.0);
}

// ============================================================================
// 2. Sub-sample Trigger Precision Oracle
// ============================================================================

#[test]
fn test_subsample_linear_interpolation_exact_rational_oracle() {
    // Verify linear interpolation formula: alpha = (V_th - V0) / (V1 - V0)
    // across 50,000 steps with pseudo-random generator
    let mut seed = 0x1234_5678_u64;
    let mut next_f64 = || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let u = (seed >> 11) as f64;
        (u / (1u64 << 53) as f64) * 200.0 - 100.0 // range [-100.0, 100.0]
    };

    for _ in 0..50_000 {
        let v0 = next_f64();
        let mut v1 = next_f64();
        while (v1 - v0).abs() < 1e-6 {
            v1 = next_f64();
        }
        // Choose level strictly between v0 and v1
        let ratio = (next_f64() + 100.0) / 200.0; // in [0, 1]
        let level = v0 + ratio * (v1 - v0);

        let t0 = 1_000_000u64;
        let t1 = 2_000_000u64;
        let dt = (t1 - t0) as f64;

        let (alpha, offset_ns) =
            calculate_subsample_offset(t0, v0, t1, v1, level).expect("valid calculation");

        let expected_alpha = (level - v0) / (v1 - v0);
        let expected_offset = (expected_alpha * dt) as f32;

        assert!(
            (alpha - expected_alpha).abs() < 1e-11,
            "Alpha error: alpha={alpha}, expected={expected_alpha}"
        );
        assert!(
            (offset_ns - expected_offset).abs() < 1e-2,
            "Offset error: offset={offset_ns}, expected={expected_offset}"
        );
        assert!((0.0..=1.0).contains(&alpha), "Alpha out of bounds: {alpha}");
    }
}

#[test]
fn test_subsample_linear_interpolation_boundary_touches_and_flat() {
    // 1. Lower boundary touch: level == v0
    let (a1, off1) = calculate_subsample_offset(100, 2.5, 200, 10.0, 2.5).unwrap();
    assert_eq!(a1, 0.0);
    assert_eq!(off1, 0.0);

    // 2. Upper boundary touch: level == v1
    let (a2, off2) = calculate_subsample_offset(100, 2.5, 200, 10.0, 10.0).unwrap();
    assert_eq!(a2, 1.0);
    assert_eq!(off2, 100.0);

    // 3. Falling edge lower boundary touch: level == v1
    let (a3, off3) = calculate_subsample_offset(100, 10.0, 200, 2.5, 2.5).unwrap();
    assert_eq!(a3, 1.0);
    assert_eq!(off3, 100.0);

    // 4. Falling edge upper boundary touch: level == v0
    let (a4, off4) = calculate_subsample_offset(100, 10.0, 200, 2.5, 10.0).unwrap();
    assert_eq!(a4, 0.0);
    assert_eq!(off4, 0.0);

    // 5. Flat signal at threshold: v0 == v1 == level
    let (a5, off5) = calculate_subsample_offset(100, 5.0, 200, 5.0, 5.0).unwrap();
    assert_eq!(a5, 0.0);
    assert_eq!(off5, 0.0);

    // 6. Flat signal away from threshold: v0 == v1 != level
    let (a6, _) = calculate_subsample_offset(100, 5.0, 200, 5.0, 7.0).unwrap();
    assert!(a6 == 0.0 || a6 == 1.0);
}

#[test]
fn test_subsample_linear_interpolation_steep_and_minimal_slopes() {
    // Steep slopes (huge denominator)
    let (a_steep1, _) =
        calculate_subsample_offset(0, 0.0, 100, 1e15, 5e14).expect("steep slope 1");
    assert!((a_steep1 - 0.5).abs() < 1e-9);

    let (a_steep2, _) =
        calculate_subsample_offset(0, 0.0, 100, 1e100, 1e99).expect("steep slope 2");
    assert!((a_steep2 - 0.1).abs() < 1e-9);

    let (a_steep3, _) =
        calculate_subsample_offset(0, 0.0, 100, 1e300, 5e299).expect("steep slope 3");
    assert!((a_steep3 - 0.5).abs() < 1e-9);

    // Minimal slopes just above 1e-12 threshold with base 0.0 (full mantissa precision)
    let (a_min1, off_min1) = calculate_subsample_offset(0, 0.0, 100, 2e-12, 1e-12)
        .expect("minimal slope 1");
    assert!((a_min1 - 0.5).abs() < 1e-9);
    assert!((off_min1 - 50.0).abs() < 1e-4);

    // Minimal slopes below 1e-12 threshold (safe fallback to nearest bound)
    let (a_min2, off_min2) = calculate_subsample_offset(0, 1.0, 100, 1.0 + 1e-14, 1.0)
        .expect("minimal slope 2");
    assert_eq!(a_min2, 0.0);
    assert_eq!(off_min2, 0.0);

    // Non-finite values
    assert!(calculate_subsample_offset(0, f64::NAN, 100, 1.0, 0.5).is_none());
    assert!(calculate_subsample_offset(0, 0.0, 100, f64::INFINITY, 0.5).is_none());
    assert!(calculate_subsample_offset(0, 0.0, 100, 1.0, f64::NAN).is_none());
    assert!(calculate_subsample_offset(100, 0.0, 50, 1.0, 0.5).is_none()); // t1 < t0
}

#[test]
fn test_subsample_anti_jitter_sine_wave_oracle() {
    // Empirical proof of anti-jitter stabilization:
    // Continuous sine wave: s(t) = sin(2 * pi * f * t)
    // Sampled at discrete interval Ts = 1000 ns (1 MHz sampling, 100 kHz sine -> 10 samples/cycle)
    // Over arbitrary phase offsets phi, compare discrete jitter vs sub-sample interpolation error
    let f = 100_000.0f64; // 100 kHz
    let t_s = 1000.0f64; // 1000 ns sampling period
    let omega = 2.0 * PI * f / 1e9; // rad / ns

    let mut max_discrete_jitter = 0.0f64;
    let mut max_subsample_error = 0.0f64;

    for trial in 0..100 {
        let phi = (trial as f64 / 100.0) * (2.0 * PI); // phase offset
        // Find exact continuous zero crossing: omega * t_exact + phi = 2 * pi * k
        // For first zero crossing after t = 0:
        // t_exact = (2 * pi - phi) / omega
        let t_exact = if phi == 0.0 {
            0.0
        } else {
            (2.0 * PI - phi) / omega
        };

        // Find discrete sample preceding t_exact
        let k = (t_exact / t_s).floor() as u64;
        let t0 = k * (t_s as u64);
        let t1 = (k + 1) * (t_s as u64);

        let v0 = (omega * (t0 as f64) + phi).sin();
        let v1 = (omega * (t1 as f64) + phi).sin();

        if v0 <= 0.0 && v1 >= 0.0 && v1 > v0 {
            let discrete_error = ((t1 as f64) - t_exact).abs();
            if discrete_error > max_discrete_jitter {
                max_discrete_jitter = discrete_error;
            }

            let (_, sub_ns) =
                calculate_subsample_offset(t0, v0, t1, v1, 0.0).expect("interpolation ok");
            let t_interp = (t0 as f64) + (sub_ns as f64);
            let interp_error = (t_interp - t_exact).abs();
            if interp_error > max_subsample_error {
                max_subsample_error = interp_error;
            }
        }
    }

    // Discrete sampling jitter can be up to Ts (up to 1000 ns)
    assert!(max_discrete_jitter > 200.0, "Discrete jitter expected to be large");
    // Sub-sample linear interpolation reduces error to < 20 ns (second-order curvature error for 10 samples/cycle)
    assert!(
        max_subsample_error < 25.0,
        "Sub-sample error too high: {max_subsample_error} ns (expected < 25 ns)"
    );
    // Improvement factor > 10x
    assert!(
        max_discrete_jitter / max_subsample_error > 10.0,
        "Sub-sample anti-jitter did not improve trigger precision by >10x"
    );
}

// ============================================================================
// 3. Multi-Scope Alignment Oracle
// ============================================================================

#[test]
fn test_sync_group_multi_scope_timebase_and_cursor_invariance() {
    let coord = SyncGroupCoordinator::new();
    let s1 = ScopeId(1);
    let s2 = ScopeId(2);
    let s3 = ScopeId(3);
    let g1 = SyncGroupId(100);

    coord.register_scope(s1, g1);
    coord.register_scope(s2, g1);
    coord.register_scope(s3, g1);

    assert_eq!(coord.scope_count(), 3);
    assert_eq!(coord.group_count(), 1);

    // Atomically set timebase
    let span_ns = 20_000_000u64; // 20 ms
    let center_offset = -1_500_000i64; // -1.5 ms
    let v_tb = coord.update_timebase(g1, span_ns, center_offset);

    // Atomically set cursors A & B
    let cursor_a = 0.25;
    let cursor_b = 0.75;
    let v_cur = coord.update_cursors(g1, Some(cursor_a), Some(cursor_b));
    assert!(v_cur > v_tb);

    // Propagate timebase to ScopeControllerConfigs
    let mut cfg1 = ScopeControllerConfig {
        scope_id: s1,
        time_span_ns: 10_000,
        pan_offset_ns: 0,
        sync_group_id: None,
        ..Default::default()
    };
    let mut cfg2 = ScopeControllerConfig {
        scope_id: s2,
        time_span_ns: 50_000,
        pan_offset_ns: 100,
        sync_group_id: None,
        ..Default::default()
    };
    let mut cfg3 = ScopeControllerConfig {
        scope_id: s3,
        time_span_ns: 100_000,
        pan_offset_ns: 200,
        sync_group_id: None,
        ..Default::default()
    };

    assert!(coord.apply_to_controller_config(s1, &mut cfg1));
    assert!(coord.apply_to_controller_config(s2, &mut cfg2));
    assert!(coord.apply_to_controller_config(s3, &mut cfg3));

    // Mathematical Invariance 1: Identical span and offset across all scopes
    for cfg in [&cfg1, &cfg2, &cfg3] {
        assert_eq!(cfg.time_span_ns, span_ns);
        assert_eq!(cfg.pan_offset_ns, center_offset);
        assert_eq!(cfg.sync_group_id, Some(g1));
    }

    // Mathematical Invariance 2: Cursor delta invariant
    let state = coord.get_group_state(g1).unwrap();
    let delta_ratio = state.cursor_b_ratio.unwrap() - state.cursor_a_ratio.unwrap();
    let physical_delta_t = delta_ratio * (state.time_span_ns as f64);
    assert_eq!(delta_ratio, 0.50);
    assert_eq!(physical_delta_t, 10_000_000.0); // exactly 10 ms
}

#[test]
fn test_sync_group_asynchronous_sampling_jitter_alignment() {
    // Challenge multi-scope alignment when Scope 1 and Scope 2 monitor the same physical event
    // with different clock phases (sampling jitter between ADC channels)
    // Scope 1 ADC clock: t_k = k * 10.0 ns
    // Scope 2 ADC clock: t_m = 3.7 ns + m * 10.0 ns (3.7 ns asynchronous clock phase shift)
    // Physical event occurs at t = 100_005.2 ns
    let t_event: f64 = 100_005.2;

    // Scope 1 sampling around event:
    let k0 = (t_event / 10.0f64).floor() as u64;
    let s1_t0 = k0 * 10;
    let s1_t1 = (k0 + 1) * 10;
    // Slope = 1.0 V / ns, level = 0.0 at t_event
    let s1_v0 = (s1_t0 as f64) - t_event;
    let s1_v1 = (s1_t1 as f64) - t_event;
    let (_, s1_sub_ns) = calculate_subsample_offset(s1_t0, s1_v0, s1_t1, s1_v1, 0.0).unwrap();
    let s1_hit = TriggerHit::new(s1_t0, s1_sub_ns);

    // Scope 2 sampling around event (with 3.7 ns phase shift):
    let phase_shift: f64 = 3.7;
    let m0 = ((t_event - phase_shift) / 10.0f64).floor() as u64;
    let s2_t0 = (m0 as f64 * 10.0 + phase_shift).round() as u64;
    let s2_t1 = ((m0 + 1) as f64 * 10.0 + phase_shift).round() as u64;
    let s2_v0 = (s2_t0 as f64) - t_event;
    let s2_v1 = (s2_t1 as f64) - t_event;
    let (_, s2_sub_ns) = calculate_subsample_offset(s2_t0, s2_v0, s2_t1, s2_v1, 0.0).unwrap();
    let s2_hit = TriggerHit::new(s2_t0, s2_sub_ns);

    // Compare discrete timestamps vs interpolated exact timestamps
    let discrete_diff = (s1_hit.timestamp_ns as i64 - s2_hit.timestamp_ns as i64).abs();
    let exact_diff = (s1_hit.exact_timestamp_ns() - s2_hit.exact_timestamp_ns()).abs();

    // Discrete alignment error is at least the phase offset (~4 ns)
    assert!(discrete_diff >= 3, "Discrete diff was {discrete_diff}");
    // Sub-sample alignment error is < 0.01 ns
    assert!(
        exact_diff < 0.01,
        "Sub-sample alignment failed to eliminate clock phase jitter: exact_diff={exact_diff}"
    );
}

#[test]
fn test_sync_group_concurrent_atomic_stress() {
    let coord = Arc::new(SyncGroupCoordinator::new());
    let group = SyncGroupId(77);
    let num_threads = 8;
    let iters = 250;

    let mut handles = Vec::new();
    for t in 0..num_threads {
        let c = Arc::clone(&coord);
        let h = thread::spawn(move || {
            let s_id = ScopeId(t as u32);
            c.register_scope(s_id, group);

            for i in 0..iters {
                let span = 10_000 + ((t * 50 + i) as u64);
                let offset = (t as i64 * 10) - (i as i64);
                c.update_timebase(group, span, offset);
                c.update_cursors(
                    group,
                    Some(0.1 + (i as f64 * 0.001) % 0.4),
                    Some(0.6 + (i as f64 * 0.001) % 0.4),
                );
                let _ = c.get_group_state(group);
            }
        });
        handles.push(h);
    }

    for h in handles {
        h.join().expect("thread finished cleanly");
    }

    assert_eq!(coord.scope_count(), num_threads);
    let state = coord.get_group_state(group).expect("group state exists");
    assert!(state.version >= (num_threads * iters) as u64);
}
