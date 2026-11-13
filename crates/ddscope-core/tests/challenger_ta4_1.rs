//! Adversarial empirical and property-based verification test suite for Milestone TA4.
//!
//! Evaluates:
//! 1. Sub-sample linear interpolation (`calculate_subsample_offset` and `TriggerHit`):
//!    - Extreme delta values: tiny slopes (|v1 - v0| < 1e-12), flat lines (v1 == v0), steep steps (v1 - v0 = 1e9),
//!      boundary touches (alpha = 0.0, alpha = 1.0).
//!    - Non-monotonic timestamps (t1 < t0), identical timestamps (t1 == t0).
//!    - Pathological float inputs: NaN, +Inf, -Inf, subnormal float inputs.
//!    - Universal mathematical invariants: alpha in [0.0, 1.0], subpixel_offset_ns in [0.0, (t1 - t0) as f32].
//! 2. Schmitt-trigger hysteresis debouncing under high noise (`TriggerEngine`):
//!    - Intra-band noise immunity: zero spurious triggers when noise is confined to [L - h, L + h].
//!    - Multi-cycle square and triangle waves with and without noise: exact trigger counting across Rising, Falling, Either.
//!    - Discrete state triggering (`StateEquals`) across state code boundaries and edge case values.
//! 3. ScopeController state machine & synchronization:
//!    - 4 sweep modes (Roll, Auto, Normal, Single) and 5 runtime states.
//!    - 100ms auto freerun timeout in Auto mode, indefinite wait in Normal mode.
//!    - Memory pinning in Single mode, release on rearm or mode switch.
//!    - Concurrent `SyncGroupCoordinator` zero-parallax timebase alignment.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

use ddscope_core::scope::{
    calculate_subsample_offset, EdgeSlope, HysteresisBandState, ScopeController,
    ScopeControllerConfig, ScopeStepOutcome, SyncGroupCoordinator, TriggerCondition, TriggerEngine,
    TriggerHit, TriggerRuntimeState, TriggerSweepMode,
};
use ddscope_core::types::{ChannelId, ScopeId, SyncGroupId};

// =========================================================================
// 1. Sub-sample Linear Interpolation: Extreme Deltas & Boundary Touches
// =========================================================================

#[test]
fn test_subsample_linear_interpolation_extreme_slopes() {
    println!("\n=== Testing sub-sample linear interpolation: extreme slopes ===");

    // 1. Tiny slopes (|v1 - v0| < 1e-12)
    let tiny_deltas = [1e-13, 1e-14, 1e-15, 1e-20, 1e-300];
    for &d in &tiny_deltas {
        let v0 = 1.0;
        let v1 = 1.0 + d;
        let level = 1.0 + d * 0.5;
        let res = calculate_subsample_offset(100, v0, 200, v1, level);
        assert!(res.is_some(), "tiny delta {d} must not fail");
        let (alpha, offset) = res.unwrap();
        assert!(
            (0.0..=1.0).contains(&alpha),
            "alpha must be in [0.0, 1.0], got {alpha} for delta {d}"
        );
        assert!(
            (0.0..=100.0).contains(&offset),
            "offset must be in [0.0, 100.0], got {offset} for delta {d}"
        );
        assert!(!alpha.is_nan() && !offset.is_nan());
    }

    // 2. Flat lines (v1 == v0)
    let flat_levels = [-5.0, 0.0, 2.0, 2.0, 4.0, 10.0];
    for &level in &flat_levels {
        let (alpha, offset) = calculate_subsample_offset(100, 2.0, 200, 2.0, level)
            .expect("flat line must evaluate safely");
        assert!(
            alpha == 0.0 || alpha == 1.0,
            "flat line alpha must snap to 0.0 or 1.0, got {alpha}"
        );
        assert!(offset == 0.0 || offset == 100.0);
    }

    // 3. Steep steps (|v1 - v0| = 1e9, 1e15, 1e30)
    let steep_deltas = [1e9, 1e15, 1e30];
    for &span in &steep_deltas {
        let v0 = 0.0;
        let v1 = span;
        let level = span * 0.25;
        let (alpha, offset) = calculate_subsample_offset(1_000, v0, 2_000, v1, level)
            .expect("steep step must evaluate");
        assert!(
            (alpha - 0.25).abs() < 1e-6,
            "alpha should be ~0.25, got {alpha}"
        );
        assert!(
            (offset - 250.0).abs() < 1e-3,
            "offset should be ~250.0 ns, got {offset}"
        );
    }

    // 4. Threshold exactly at v0 (alpha = 0.0)
    let (alpha_at_v0, offset_at_v0) =
        calculate_subsample_offset(100, 5.0, 200, 10.0, 5.0).expect("threshold at v0");
    assert_eq!(alpha_at_v0, 0.0);
    assert_eq!(offset_at_v0, 0.0);

    // 5. Threshold exactly at v1 (alpha = 1.0)
    let (alpha_at_v1, offset_at_v1) =
        calculate_subsample_offset(100, 5.0, 200, 10.0, 10.0).expect("threshold at v1");
    assert_eq!(alpha_at_v1, 1.0);
    assert_eq!(offset_at_v1, 100.0);

    // 6. Falling boundary crossings
    let (alpha_fall_v0, offset_fall_v0) =
        calculate_subsample_offset(100, 10.0, 200, 5.0, 10.0).expect("falling threshold at v0");
    assert_eq!(alpha_fall_v0, 0.0);
    assert_eq!(offset_fall_v0, 0.0);

    let (alpha_fall_v1, offset_fall_v1) =
        calculate_subsample_offset(100, 10.0, 200, 5.0, 5.0).expect("falling threshold at v1");
    assert_eq!(alpha_fall_v1, 1.0);
    assert_eq!(offset_fall_v1, 100.0);
}

// =========================================================================
// 2. Sub-sample Linear Interpolation: Timestamps & Pathological Floats
// =========================================================================

#[test]
fn test_subsample_linear_interpolation_timestamps_and_invalids() {
    println!("\n=== Testing sub-sample linear interpolation: timestamps & invalids ===");

    // 1. Non-monotonic timestamps (t1 < t0) -> MUST return None
    assert!(calculate_subsample_offset(500, 0.0, 499, 10.0, 5.0).is_none());
    assert!(calculate_subsample_offset(1_000_000, 1.0, 0, 2.0, 1.5).is_none());

    // 2. Identical timestamps (t1 == t0) -> dt = 0
    let (alpha_eq, offset_eq) = calculate_subsample_offset(500, 0.0, 500, 10.0, 5.0)
        .expect("identical timestamps must evaluate");
    assert_eq!(alpha_eq, 0.5);
    assert_eq!(offset_eq, 0.0);

    // 3. Pathological float inputs: NaN, +Inf, -Inf
    let invalids = [f64::NAN, f64::INFINITY, f64::NEG_INFINITY];
    for &bad in &invalids {
        assert!(calculate_subsample_offset(100, bad, 200, 5.0, 2.0).is_none());
        assert!(calculate_subsample_offset(100, 1.0, 200, bad, 2.0).is_none());
        assert!(calculate_subsample_offset(100, 1.0, 200, 5.0, bad).is_none());
        assert!(calculate_subsample_offset(100, bad, 200, bad, bad).is_none());
    }

    // 4. Subnormal floats (close to f64::MIN_POSITIVE ~ 2.225e-308)
    let subnormal_1 = f64::MIN_POSITIVE / 4.0;
    let subnormal_2 = f64::MIN_POSITIVE / 2.0;
    let (alpha_sub, offset_sub) =
        calculate_subsample_offset(100, subnormal_1, 200, subnormal_2, subnormal_1 * 1.5)
            .expect("subnormal floats must evaluate");
    assert!((0.0..=1.0).contains(&alpha_sub));
    assert!((0.0..=100.0).contains(&offset_sub));
    assert!(!alpha_sub.is_nan() && !offset_sub.is_nan());

    // 5. Very large timestamp delta (dt = u64::MAX / 2)
    let t0 = 0_u64;
    let t1 = u64::MAX / 4;
    let (alpha_huge_dt, offset_huge_dt) =
        calculate_subsample_offset(t0, 0.0, t1, 10.0, 5.0).expect("huge dt");
    assert_eq!(alpha_huge_dt, 0.5);
    assert!(offset_huge_dt >= 0.0);
    assert!(offset_huge_dt <= (t1 as f32));
    assert!(!offset_huge_dt.is_nan());
}

struct Lcg(u64);
impl Lcg {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1);
        self.0
    }
    fn next_f64(&mut self) -> f64 {
        let u = self.next_u64() >> 11;
        (u as f64) / ((1_u64 << 53) as f64)
    }
}

// =========================================================================
// 3. Sub-sample Universal Invariant Property-Based Testing
// =========================================================================

#[test]
fn test_subsample_property_based_invariants_200k() {
    println!("\n=== Running 200,000 randomized property-based tests for sub-sample offset ===");

    let mut rng = Lcg(0xDEADBEEFCAFE);
    let iterations = 200_000;
    for i in 0..iterations {
        let t0 = (rng.next_u64() % 10_000_000) + 1_000;
        let dt = rng.next_u64() % 1_000_000;
        let t1 = t0 + dt;

        // Spread values across huge dynamic range [-1e6, 1e6]
        let v0 = (rng.next_f64() - 0.5) * 2e6;
        let v1 = (rng.next_f64() - 0.5) * 2e6;
        let ratio = rng.next_f64();
        let level = v0 + (v1 - v0) * ratio;

        if let Some((alpha, offset)) = calculate_subsample_offset(t0, v0, t1, v1, level) {
            // Invariant 1: alpha must be in [0.0, 1.0]
            assert!(
                (0.0..=1.0).contains(&alpha),
                "Iteration {i}: alpha {alpha} not in [0.0, 1.0]"
            );
            assert!(!alpha.is_nan(), "Iteration {i}: alpha is NaN");

            // Invariant 2: offset must be in [0.0, dt as f32]
            let max_offset = dt as f32;
            assert!(
                offset >= 0.0,
                "Iteration {i}: offset {offset} must be >= 0.0"
            );
            // Allow 1 ULP float rounding margin
            assert!(
                offset <= max_offset * 1.00001 + 1e-4,
                "Iteration {i}: offset {offset} exceeds max {max_offset}"
            );
            assert!(!offset.is_nan(), "Iteration {i}: offset is NaN");

            // Invariant 3: TriggerHit timestamp properties
            let hit = TriggerHit::new(t0, offset);
            let exact = hit.exact_timestamp_ns();
            assert!(
                exact >= t0 as f64 - 1e-3 && exact <= t1 as f64 + 1e-3,
                "Iteration {i}: exact ts {exact} outside [{t0}, {t1}]"
            );

            let rounded_ts = hit.interpolated_ts_u64();
            assert!(
                rounded_ts >= t0 && rounded_ts <= t1.max(t0 + 1),
                "Iteration {i}: rounded ts {rounded_ts} outside [{t0}, {t1}]"
            );
        }
    }
    println!("  -> Passed all 200,000 property-based iterations.");
}

// =========================================================================
// 4. Schmitt-Trigger Hysteresis: Intra-Band Noise Immunity (Invariant H1)
// =========================================================================

#[test]
fn test_hysteresis_intra_band_noise_immunity_100k() {
    println!("\n=== Testing Schmitt hysteresis debouncing: intra-band noise immunity ===");

    let level = 1.65; // e.g. 3.3V logic midpoint
    let h = 0.35; // Hysteresis band [1.30, 2.00]

    // 1. Unprimed start: noise strictly within [1.35, 1.95]
    for &slope in &[EdgeSlope::Rising, EdgeSlope::Falling, EdgeSlope::Either] {
        let cond = TriggerCondition::Edge {
            channel: ChannelId(1),
            level,
            hysteresis: h,
            slope,
        };
        let mut engine = TriggerEngine::new(cond);

        let mut trigger_count = 0;
        let mut t = 1_000_u64;

        // 50,000 samples of rapid oscillation within the band
        for i in 0..50_000 {
            t += 10;
            // Oscillates between 1.35 and 1.95
            let val = level + 0.30 * ((i as f64 * 0.1).sin());
            if engine.evaluate_scalar(t, val).is_some() {
                trigger_count += 1;
            }
        }

        assert_eq!(
            trigger_count, 0,
            "Unprimed trigger engine for slope {slope:?} must fire ZERO triggers under intra-band noise"
        );
        assert_eq!(engine.state(), HysteresisBandState::Unprimed);
    }

    // 2. Primed start: signal dips below 1.30, crosses level to 1.70, then rattles within band for 50,000 samples
    for &slope in &[EdgeSlope::Rising, EdgeSlope::Either] {
        let cond = TriggerCondition::Edge {
            channel: ChannelId(1),
            level,
            hysteresis: h,
            slope,
        };
        let mut engine = TriggerEngine::new(cond);

        let mut t = 1_000_u64;

        // Step 1: Prime low (1.20 <= 1.30)
        assert!(engine.evaluate_scalar(t, 1.20).is_none());
        assert_eq!(engine.state(), HysteresisBandState::ArmedLow);

        // Step 2: Cross threshold to 1.70 -> Fires EXACTLY 1 trigger!
        t += 10;
        let hit = engine
            .evaluate_scalar(t, 1.70)
            .expect("must fire single valid trigger");
        assert_eq!(hit.timestamp_ns, 1_000);
        // alpha = (1.65 - 1.20) / (1.70 - 1.20) = 0.45 / 0.50 = 0.90 -> offset = 9.0 ns
        assert!((hit.subpixel_offset_ns - 9.0).abs() < 1e-4);

        // Step 3: 50,000 intra-band noise samples between 1.35 and 1.95
        let mut spurious_count = 0;
        for i in 0..50_000 {
            t += 10;
            let val = level + 0.30 * ((i as f64 * 0.2).cos());
            if engine.evaluate_scalar(t, val).is_some() {
                spurious_count += 1;
            }
        }

        assert_eq!(
            spurious_count, 0,
            "Engine for slope {slope:?} must fire ZERO spurious triggers after primary trigger"
        );
    }
}

// =========================================================================
// 5. Multi-Cycle Square Waves: Ground Truth Counting With & Without Noise
// =========================================================================

#[test]
fn test_multi_cycle_square_wave_ground_truth_matching() {
    println!("\n=== Testing multi-cycle square wave ground truth counting ===");

    let num_cycles = 100;
    let samples_per_half_cycle = 20;
    let dt = 10_u64; // 10 ns per sample
    let level = 0.0;
    let h = 0.5; // Band [-0.5, +0.5]
    let high_val = 2.5;
    let low_val = -2.5;

    // Test cases: (noise_amplitude, description)
    let test_cases = [
        (0.0, "clean square wave"),
        (0.35, "square wave with high noise within hysteresis band"),
    ];

    for (noise_amp, desc) in test_cases {
        println!("  Testing {desc} (noise_amp = {noise_amp})");

        // 1. Rising slope
        {
            let cond = TriggerCondition::Edge {
                channel: ChannelId(1),
                level,
                hysteresis: h,
                slope: EdgeSlope::Rising,
            };
            let mut engine = TriggerEngine::new(cond);
            let mut triggers = 0;
            let mut t = 0_u64;

            for _cycle in 0..num_cycles {
                // Low half-cycle: low_val + noise
                for s in 0..samples_per_half_cycle {
                    t += dt;
                    let noise = if noise_amp > 0.0 {
                        ((s as f64 * 1.7).sin()) * noise_amp
                    } else {
                        0.0
                    };
                    if engine.evaluate_scalar(t, low_val + noise).is_some() {
                        triggers += 1;
                    }
                }
                // High half-cycle: high_val + noise
                for s in 0..samples_per_half_cycle {
                    t += dt;
                    let noise = if noise_amp > 0.0 {
                        ((s as f64 * 1.7).cos()) * noise_amp
                    } else {
                        0.0
                    };
                    if engine.evaluate_scalar(t, high_val + noise).is_some() {
                        triggers += 1;
                    }
                }
            }
            assert_eq!(
                triggers, num_cycles,
                "Rising triggers must match exact cycle count {num_cycles} for {desc}"
            );
        }

        // 2. Falling slope
        {
            let cond = TriggerCondition::Edge {
                channel: ChannelId(1),
                level,
                hysteresis: h,
                slope: EdgeSlope::Falling,
            };
            let mut engine = TriggerEngine::new(cond);
            let mut triggers = 0;
            let mut t = 0_u64;

            for _cycle in 0..num_cycles {
                // High half-cycle
                for s in 0..samples_per_half_cycle {
                    t += dt;
                    let noise = if noise_amp > 0.0 {
                        ((s as f64 * 1.7).cos()) * noise_amp
                    } else {
                        0.0
                    };
                    if engine.evaluate_scalar(t, high_val + noise).is_some() {
                        triggers += 1;
                    }
                }
                // Low half-cycle
                for s in 0..samples_per_half_cycle {
                    t += dt;
                    let noise = if noise_amp > 0.0 {
                        ((s as f64 * 1.7).sin()) * noise_amp
                    } else {
                        0.0
                    };
                    if engine.evaluate_scalar(t, low_val + noise).is_some() {
                        triggers += 1;
                    }
                }
            }
            assert_eq!(
                triggers, num_cycles,
                "Falling triggers must match exact cycle count {num_cycles} for {desc}"
            );
        }

        // 3. Either slope -> 2 triggers per cycle (100 rising + 100 falling)
        {
            let cond = TriggerCondition::Edge {
                channel: ChannelId(1),
                level,
                hysteresis: h,
                slope: EdgeSlope::Either,
            };
            let mut engine = TriggerEngine::new(cond);
            let mut triggers = 0;
            let mut t = 0_u64;

            for _cycle in 0..num_cycles {
                // Low half-cycle
                for s in 0..samples_per_half_cycle {
                    t += dt;
                    let noise = if noise_amp > 0.0 {
                        ((s as f64 * 1.7).sin()) * noise_amp
                    } else {
                        0.0
                    };
                    if engine.evaluate_scalar(t, low_val + noise).is_some() {
                        triggers += 1;
                    }
                }
                // High half-cycle
                for s in 0..samples_per_half_cycle {
                    t += dt;
                    let noise = if noise_amp > 0.0 {
                        ((s as f64 * 1.7).cos()) * noise_amp
                    } else {
                        0.0
                    };
                    if engine.evaluate_scalar(t, high_val + noise).is_some() {
                        triggers += 1;
                    }
                }
            }
            // Final transition back to low to complete the 100th falling edge
            for s in 0..samples_per_half_cycle {
                t += dt;
                let noise = if noise_amp > 0.0 {
                    ((s as f64 * 1.7).sin()) * noise_amp
                } else {
                    0.0
                };
                if engine.evaluate_scalar(t, low_val + noise).is_some() {
                    triggers += 1;
                }
            }
            assert_eq!(
                triggers,
                num_cycles * 2,
                "Either triggers must match exactly 2x cycle count {} for {desc}",
                num_cycles * 2
            );
        }
    }
}

// =========================================================================
// 6. Multi-Cycle Triangle Waves: Ramp Crossings & Continuous Offset
// =========================================================================

#[test]
fn test_multi_cycle_triangle_wave_ground_truth_matching() {
    println!("\n=== Testing multi-cycle triangle wave ramp crossings ===");

    let num_cycles = 50;
    let points_per_ramp = 100;
    let dt = 10_u64;
    let level = 0.5;
    let h = 0.8; // Band [-0.3, 1.3]
    let peak_high = 3.0;
    let peak_low = -3.0;

    // Test clean and noisy triangle waves
    for &noise_amp in &[0.0, 0.4] {
        let cond = TriggerCondition::Edge {
            channel: ChannelId(1),
            level,
            hysteresis: h,
            slope: EdgeSlope::Rising,
        };
        let mut engine = TriggerEngine::new(cond);
        let mut triggers = 0;
        let mut t = 0_u64;

        for _cycle in 0..num_cycles {
            // Ramp up from peak_low to peak_high
            for p in 0..points_per_ramp {
                t += dt;
                let progress = p as f64 / points_per_ramp as f64;
                let base_val = peak_low + (peak_high - peak_low) * progress;
                let noise = if noise_amp > 0.0 {
                    (p as f64 * 0.9).sin() * noise_amp
                } else {
                    0.0
                };
                if let Some(hit) = engine.evaluate_scalar(t, base_val + noise) {
                    triggers += 1;
                    // Subpixel offset must be valid
                    assert!(hit.subpixel_offset_ns >= 0.0 && hit.subpixel_offset_ns <= dt as f32);
                }
            }
            // Ramp down from peak_high to peak_low
            for p in 0..points_per_ramp {
                t += dt;
                let progress = p as f64 / points_per_ramp as f64;
                let base_val = peak_high - (peak_high - peak_low) * progress;
                let noise = if noise_amp > 0.0 {
                    (p as f64 * 0.9).cos() * noise_amp
                } else {
                    0.0
                };
                if let Some(hit) = engine.evaluate_scalar(t, base_val + noise) {
                    triggers += 1;
                    assert!(hit.subpixel_offset_ns >= 0.0 && hit.subpixel_offset_ns <= dt as f32);
                }
            }
        }

        assert_eq!(
            triggers, num_cycles,
            "Triangle wave must produce exactly {num_cycles} rising triggers (noise={noise_amp})"
        );
    }
}

// =========================================================================
// 7. Discrete State Triggering: State Code Edge Cases & Invariants
// =========================================================================

#[test]
fn test_discrete_state_trigger_adversarial() {
    println!("\n=== Testing discrete state triggering: StateEquals invariants ===");

    // Test various target state codes: negative, zero, positive, extremes
    let target_codes = [i32::MIN, -42, -1, 0, 1, 100, i32::MAX];

    for &target in &target_codes {
        let cond = TriggerCondition::StateEquals {
            channel: ChannelId(5),
            state_code: target,
        };
        let mut engine = TriggerEngine::new(cond);

        // Initial sample matching target -> Fires immediately
        let hit0 = engine
            .evaluate_state(100, target)
            .expect("initial state match must trigger");
        assert_eq!(hit0.timestamp_ns, 100);
        assert_eq!(
            hit0.subpixel_offset_ns, 0.0,
            "discrete triggers must have subpixel offset 0.0"
        );

        // Consecutive samples with same state -> ZERO additional triggers
        for t in (200..=1000).step_by(100) {
            assert!(
                engine.evaluate_state(t, target).is_none(),
                "dwelling in state must not re-trigger"
            );
        }

        // Transition away from target
        let other = if target == 0 { 1 } else { 0 };
        assert!(engine.evaluate_state(1100, other).is_none());
        assert!(engine.evaluate_state(1200, other).is_none());

        // Transition back to target -> Fires second trigger!
        let hit1 = engine
            .evaluate_state(1300, target)
            .expect("re-entry into state must trigger");
        assert_eq!(hit1.timestamp_ns, 1300);
        assert_eq!(hit1.subpixel_offset_ns, 0.0);

        // Cross-domain safety checks:
        // Calling evaluate_scalar on state condition must return None
        assert!(engine.evaluate_scalar(1400, 1.23).is_none());
    }

    // Calling evaluate_state on edge condition must return None
    let edge_cond = TriggerCondition::Edge {
        channel: ChannelId(1),
        level: 1.0,
        hysteresis: 0.1,
        slope: EdgeSlope::Rising,
    };
    let mut edge_engine = TriggerEngine::new(edge_cond);
    assert!(edge_engine.evaluate_state(100, 42).is_none());
}

// =========================================================================
// 8. ScopeController State Machine: Sweep Modes, Timers & Latch Pinning
// =========================================================================

#[test]
fn test_scope_controller_sweep_modes_and_states_adversarial() {
    println!("\n=== Testing ScopeController state machine & lifecycle invariants ===");

    // 1. Single Mode Pinning Lifecycle
    {
        let config = ScopeControllerConfig {
            scope_id: ScopeId(10),
            sweep_mode: TriggerSweepMode::Single,
            time_span_ns: 10_000,
            pre_trigger_ratio: 0.25, // pre_span = 2,500 ns, post_span = 7,500 ns
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

        // Prime low: sample at 10_000 ns with val -1.0
        assert_eq!(
            ctrl.process_scalar_sample(ChannelId(1), 10_000, -1.0),
            ScopeStepOutcome::WaitingForTrigger
        );

        // Edge crossing at 11_000 ns to +1.0 (t_trig = 10_000 ns). Target end = 10_000 + 7_500 = 17_500 ns.
        let trig_outcome = ctrl.process_scalar_sample(ChannelId(1), 11_000, 1.0);
        match trig_outcome {
            ScopeStepOutcome::TriggerDetected { hit } => {
                assert_eq!(hit.timestamp_ns, 10_000);
                assert!((hit.subpixel_offset_ns - 500.0).abs() < 1e-3);
                assert_eq!(ctrl.current_state(), TriggerRuntimeState::Triggered);
            }
            _ => panic!("expected TriggerDetected outcome"),
        }

        // Interim samples during post-trigger window collection
        assert_eq!(
            ctrl.process_scalar_sample(ChannelId(1), 15_000, 1.1),
            ScopeStepOutcome::NoChange
        );
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::Triggered);

        // Sample reaching 18_000 ns (>= 17_500 ns target end) -> completes capture and latches memory!
        let comp_outcome = ctrl.process_scalar_sample(ChannelId(1), 18_000, 1.2);
        match comp_outcome {
            ScopeStepOutcome::CaptureCompleted {
                window,
                latched_pinned,
            } => {
                assert!(latched_pinned);
                assert_eq!(window.mode, TriggerSweepMode::Single);
                assert_eq!(window.trigger_state, TriggerRuntimeState::LatchedPinned);
                assert_eq!(window.win_t_start_ns, 10_000 - 2_500);
                assert_eq!(window.win_t_span_ns, 10_000);
            }
            _ => panic!("expected CaptureCompleted"),
        }
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::LatchedPinned);
        assert!(ctrl.is_pinned());

        // In LatchedPinned, any new incoming samples MUST produce NoChange and NOT disturb window
        for t in (20_000..=50_000).step_by(5_000) {
            assert_eq!(
                ctrl.process_scalar_sample(ChannelId(1), t, -2.0),
                ScopeStepOutcome::NoChange
            );
            assert!(ctrl.is_pinned());
        }

        // Calling rearm() unpins memory and returns to Armed
        ctrl.rearm();
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);
        assert!(!ctrl.is_pinned());
    }

    // 2. Auto Mode 100ms Timeout & Freerun Ticks
    {
        let config = ScopeControllerConfig {
            scope_id: ScopeId(11),
            sweep_mode: TriggerSweepMode::Auto,
            time_span_ns: 5_000,
            auto_timeout_ns: 100_000_000, // 100 ms
            trigger: Some(TriggerCondition::Edge {
                channel: ChannelId(2),
                level: 5.0,
                hysteresis: 1.0,
                slope: EdgeSlope::Rising,
            }),
            ..Default::default()
        };
        let mut ctrl = ScopeController::new(config);

        // Feed untriggered signal at 10ms intervals
        let mut freerun_count = 0;
        for tick in 1..=31 {
            let t = tick * 10_000_000; // 10 ms per tick
            let outcome = ctrl.process_scalar_sample(ChannelId(2), t, 0.0);
            if let ScopeStepOutcome::AutoFreerunFired { window } = outcome {
                freerun_count += 1;
                assert_eq!(window.mode, TriggerSweepMode::Auto);
                assert_eq!(window.trigger_state, TriggerRuntimeState::AutoFreerun);
                assert_eq!(window.subpixel_offset_ns, 0.0);
            }
        }
        // Over 310 ms with 100 ms timeout (armed at 10ms -> triggers at 110ms, 210ms, 310ms), freerun fires 3 times
        assert_eq!(freerun_count, 3, "freerun must fire every 100 ms in Auto");
    }

    // 3. Normal Mode Never Times Out
    {
        let config = ScopeControllerConfig {
            scope_id: ScopeId(12),
            sweep_mode: TriggerSweepMode::Normal,
            auto_timeout_ns: 100_000_000,
            trigger: Some(TriggerCondition::Edge {
                channel: ChannelId(3),
                level: 0.0,
                hysteresis: 0.5,
                slope: EdgeSlope::Rising,
            }),
            ..Default::default()
        };
        let mut ctrl = ScopeController::new(config);

        // Step clock past 500 ms (> 5 timeout intervals)
        for t in (100_000_000..=500_000_000).step_by(50_000_000) {
            let outcome = ctrl.step(t);
            assert_eq!(outcome, ScopeStepOutcome::WaitingForTrigger);
            assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);
        }
    }
}

// =========================================================================
// 9. SyncGroupCoordinator: Concurrency Stress & Sanitization
// =========================================================================

#[test]
fn test_sync_group_coordinator_stress_and_adversarial() {
    println!("\n=== Testing SyncGroupCoordinator: concurrency stress and bounds sanitization ===");

    let coord = Arc::new(SyncGroupCoordinator::new());
    let group = SyncGroupId(100);

    // 1. Parameter Clamping & NaN/Inf Sanitization
    {
        // Minimum time span clamp to 1000 ns
        coord.update_timebase(group, 0, 0);
        let s0 = coord.get_group_state(group).expect("group state");
        assert_eq!(s0.time_span_ns, 1000);

        coord.update_timebase(group, 500, 100);
        let s1 = coord.get_group_state(group).expect("group state");
        assert_eq!(s1.time_span_ns, 1000);

        // Out-of-bounds cursor ratios clamped to [0.0, 1.0]
        coord.update_cursors(group, Some(-2.5), Some(5.0));
        let s2 = coord.get_group_state(group).expect("group state");
        assert_eq!(s2.cursor_a_ratio, Some(0.0));
        assert_eq!(s2.cursor_b_ratio, Some(1.0));

        // NaN / Inf converted to None
        coord.update_cursors(group, Some(f64::NAN), Some(f64::NEG_INFINITY));
        let s3 = coord.get_group_state(group).expect("group state");
        assert_eq!(s3.cursor_a_ratio, None);
        assert_eq!(s3.cursor_b_ratio, None);
    }

    // 2. High Concurrency Thread Stress (16 threads x 500 iterations)
    {
        let num_threads = 16;
        let iters_per_thread = 500;
        let success_counter = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for t in 0..num_threads {
            let coord_clone = Arc::clone(&coord);
            let counter_clone = Arc::clone(&success_counter);
            let h = thread::spawn(move || {
                let scope_id = ScopeId(t as u32);
                coord_clone.register_scope(scope_id, group);

                for i in 0..iters_per_thread {
                    let span = 10_000 + (t * 50 + i) as u64;
                    let offset = (t as i64 * 100) - (i as i64 * 10);
                    coord_clone.update_timebase(group, span, offset);

                    let ratio_a = Some(((i % 100) as f64) / 100.0);
                    let ratio_b = Some((((i + 50) % 100) as f64) / 100.0);
                    coord_clone.update_cursors(group, ratio_a, ratio_b);

                    if let Some(state) = coord_clone.get_group_state(group) {
                        assert!(state.time_span_ns >= 1000);
                        assert!(state.version >= 1);
                        counter_clone.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });
            handles.push(h);
        }

        for h in handles {
            h.join().expect("thread finished cleanly without panic");
        }

        assert_eq!(
            success_counter.load(Ordering::SeqCst),
            num_threads * iters_per_thread,
            "all concurrent thread updates must succeed"
        );
        assert_eq!(coord.scope_count(), num_threads);
        assert_eq!(coord.group_count(), 1);
    }
}
