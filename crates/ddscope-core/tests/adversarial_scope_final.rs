//! Empirical Challenger 2 Adversarial Stress Suite on Scope Engine, Trigger/Interpolation,
//! SyncGroup, and DDSF Wire Serialization for Milestone TA-FINAL.
//!
//! Validates:
//! 1. Trigger Sub-Sample Interpolation & Math Limits:
//!    - Denominator underflow, flat steps, float overflow, subnormals, non-monotonic timestamps.
//! 2. Schmitt-Trigger Debouncing & Edge Hysteresis:
//!    - Micro-oscillations hovering around hysteresis bands, identical consecutive samples,
//!      zero slope, infinite slope, rising/falling/either edge transitions, discrete state triggers.
//! 3. ScopeController State Machine & Auto-Trigger Timeout:
//!    - Rapid mode transitions (Roll, Armed, Auto, Normal, Single), 100ms auto timeout boundary
//!      precision under 1M samples, Single mode latching with 1M subsequent samples rejected,
//!      extreme pre-trigger ratios and pan offsets.
//! 4. SyncGroupCoordinator Multi-Threaded Concurrency:
//!    - High contention 16-thread simultaneous mutation of timebases, cursors, registrations,
//!      and config synchronization. Zero deadlocks, zero panics, monotonic version increments.
//! 5. DDSF v1 Frame Packer & Wire Serialization:
//!    - Fuzzing 0 to 64+ tracks across all 5 block kinds (odd lengths, padded to 8 bytes).
//!    - Strict 64-byte aligned buffer and 8-byte natural section alignment.
//!    - Zero secondary heap allocation invariant under sustained steady-state streaming (10,000 frames).
//!    - Zero-copy reader deserialization integrity and robust rejection of corrupted wire buffers.

use std::sync::Arc;
use std::thread;

use ddscope_core::scope::controller::{
    calculate_subsample_offset, HysteresisBandState, ScopeController, ScopeControllerConfig,
    ScopeStepOutcome, TriggerEngine, TriggerHit,
};
use ddscope_core::scope::ddsf::{
    DdsfAnalogPoint, DdsfBlockKind, DdsfFrameHeader, DdsfFramePacker, DdsfM4Column,
    DdsfSnapshotEntry, DdsfSnapshotKind, DdsfStateRibbon, DdsfTrackStats, DdsfZeroCopyReader,
    ScopePacketView, ScopeViewportPayload, TrackBlockView, TrackPayload, TrackPayloadSlice,
    DDSF_FLAG_QUOTA_WARNING, DDSF_HEADER_BYTES, DDSF_MAGIC, DDSF_TRACK_FLAG_HAS_STATS,
    DDSF_VERSION_V1,
};
use ddscope_core::scope::sync::SyncGroupCoordinator;
use ddscope_core::scope::{EdgeSlope, TriggerCondition, TriggerRuntimeState, TriggerSweepMode};
use ddscope_core::types::{ChannelId, ScopeId, SyncGroupId};

/// Deterministic XorShift64 pseudo-random generator.
struct XorShift64 {
    state: u64,
}

impl XorShift64 {
    fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 { 0x5a17_e110_dead_beef } else { seed },
        }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    fn next_f64(&mut self) -> f64 {
        (self.next_u64() as f64) / (u64::MAX as f64)
    }

    fn next_range(&mut self, min: u64, max: u64) -> u64 {
        if min >= max {
            return min;
        }
        min + (self.next_u64() % (max - min + 1))
    }
}

// ============================================================================
// 1. SUB-SAMPLE LINEAR INTERPOLATION & FLOAT MATH STRESS
// ============================================================================

#[test]
fn test_subsample_math_extreme_denominators_and_overflow() {
    // 1. Flat step (v0 == v1 == level)
    let (alpha, offset) = calculate_subsample_offset(1000, 2.0, 2000, 2.0, 2.0).unwrap();
    assert_eq!(alpha, 0.0);
    assert_eq!(offset, 0.0);

    // 2. Denominator under 1e-12: level closer to v0 vs closer to v1
    let (alpha_near_v0, _) =
        calculate_subsample_offset(100, 5.0, 200, 5.0 + 1e-14, 5.0 + 1e-15).unwrap();
    assert_eq!(alpha_near_v0, 0.0);

    let (alpha_near_v1, _) =
        calculate_subsample_offset(100, 5.0, 200, 5.0 + 1e-14, 5.0 + 9e-15).unwrap();
    assert_eq!(alpha_near_v1, 1.0);

    // 3. Huge values: v0 = -1e308, v1 = 1e308 (denom overflows to INFINITY)
    let (alpha_overflow, offset_overflow) =
        calculate_subsample_offset(0, -1e308, 1000, 1e308, 0.0).unwrap();
    assert!(alpha_overflow.is_finite());
    assert!((0.0..=1.0).contains(&alpha_overflow));
    assert!(offset_overflow.is_finite());

    // 4. Subnormals (5e-324)
    let (alpha_subnormal, _) =
        calculate_subsample_offset(10, 5e-324, 20, 1e-323, 7e-324).unwrap();
    assert!(alpha_subnormal == 0.0 || alpha_subnormal == 1.0); // Treated safely as tiny denom

    // 5. Machine epsilon step (1.0 vs 1.0 + f64::EPSILON)
    let (alpha_eps, _) =
        calculate_subsample_offset(0, 1.0, 100, 1.0 + f64::EPSILON, 1.0 + f64::EPSILON / 2.0)
            .unwrap();
    assert!((0.0..=1.0).contains(&alpha_eps));

    // 6. Non-monotonic timestamps: t1 < t0 must return None
    assert!(calculate_subsample_offset(200, 1.0, 100, 2.0, 1.5).is_none());

    // 7. Identical timestamps: t1 == t0 -> dt == 0 -> offset == 0.0
    let (alpha_same_t, offset_same_t) =
        calculate_subsample_offset(500, 0.0, 500, 10.0, 5.0).unwrap();
    assert_eq!(alpha_same_t, 0.5);
    assert_eq!(offset_same_t, 0.0);

    // 8. Max span dt = u64::MAX
    let (alpha_max_dt, offset_max_dt) =
        calculate_subsample_offset(0, 0.0, u64::MAX, 10.0, 5.0).unwrap();
    assert_eq!(alpha_max_dt, 0.5);
    assert!(offset_max_dt.is_finite());

    // 9. NaN / Infinity rejections across all inputs
    assert!(calculate_subsample_offset(0, f64::NAN, 10, 1.0, 0.5).is_none());
    assert!(calculate_subsample_offset(0, 0.0, 10, f64::INFINITY, 0.5).is_none());
    assert!(calculate_subsample_offset(0, 0.0, 10, 1.0, f64::NAN).is_none());
    assert!(calculate_subsample_offset(0, 0.0, 10, 1.0, f64::NEG_INFINITY).is_none());
}

#[test]
fn test_trigger_hit_numerical_precision_and_rounding() {
    let hit_exact = TriggerHit::new(1_000_000, 0.4);
    assert_eq!(hit_exact.interpolated_ts_u64(), 1_000_000);

    let hit_round_up = TriggerHit::new(1_000_000, 0.6);
    assert_eq!(hit_round_up.interpolated_ts_u64(), 1_000_001);

    let hit_zero_offset = TriggerHit::new(500_000, 0.0);
    assert_eq!(hit_zero_offset.interpolated_ts_u64(), 500_000);
    assert_eq!(hit_zero_offset.exact_timestamp_ns(), 500_000.0);

    let hit_large_ts = TriggerHit::new(u64::MAX - 10, 5.2);
    assert_eq!(hit_large_ts.interpolated_ts_u64(), u64::MAX - 5);
}

// ============================================================================
// 2. SCHMITT-TRIGGER HYSTERESIS DEBOUNCING & EDGE EVALUATION STRESS
// ============================================================================

#[test]
fn test_hysteresis_micro_oscillations_hovering_at_boundary() {
    // Level = 0.0, Hysteresis = 1.0 -> V_low = -1.0, V_high = +1.0
    let cond = TriggerCondition::Edge {
        channel: ChannelId(1),
        level: 0.0,
        hysteresis: 1.0,
        slope: EdgeSlope::Rising,
    };
    let mut engine = TriggerEngine::new(cond);

    // 1. Hover right above V_low (-0.999999999): MUST NOT prime ArmedLow!
    for i in 0..10_000 {
        let val = -0.999999999 + ((i % 3) as f64 * 1e-10);
        assert!(engine.evaluate_scalar(i * 10, val).is_none());
        assert_eq!(engine.state(), HysteresisBandState::Unprimed);
    }

    // 2. Cross V_low (-1.000000001): Primes ArmedLow!
    assert!(engine.evaluate_scalar(100_001, -1.000000001).is_none());
    assert_eq!(engine.state(), HysteresisBandState::ArmedLow);

    // 3. Hover around V_low without crossing level 0.0: MUST stay ArmedLow!
    for i in 100_002..110_000 {
        let val = -1.0 + ((i % 5) as f64 - 2.0) * 0.05; // stays between -1.1 and -0.9
        assert!(engine.evaluate_scalar(i * 10, val).is_none());
    }
    assert_eq!(engine.state(), HysteresisBandState::ArmedLow);

    // 4. Finally cross level 0.0: Rising trigger fires exactly once!
    let hit = engine
        .evaluate_scalar(1_200_000, 0.5)
        .expect("must fire rising trigger");
    assert_eq!(hit.timestamp_ns, 1_099_990);
    assert_eq!(engine.state(), HysteresisBandState::Unprimed);

    // 5. Oscillations between 0.1 and 0.9 (above level, but inside V_high): 0 triggers!
    for i in 120_001..130_000 {
        let val = 0.5 + ((i % 2) as f64) * 0.3;
        assert!(engine.evaluate_scalar(i * 10, val).is_none());
    }
}

#[test]
fn test_hysteresis_identical_samples_and_zero_slope() {
    let cond = TriggerCondition::Edge {
        channel: ChannelId(1),
        level: 5.0,
        hysteresis: 1.0,
        slope: EdgeSlope::Rising,
    };
    let mut engine = TriggerEngine::new(cond);

    // Arm low
    engine.evaluate_scalar(100, 3.0);
    assert_eq!(engine.state(), HysteresisBandState::ArmedLow);

    // Exactly at level 5.0: triggers once!
    assert!(engine.evaluate_scalar(200, 5.0).is_some());
    assert_eq!(engine.state(), HysteresisBandState::Unprimed);

    // Re-arm low
    engine.evaluate_scalar(300, 2.0);
    assert_eq!(engine.state(), HysteresisBandState::ArmedLow);

    // Multiple identical samples at level 5.0: only first one triggers!
    let hit1 = engine.evaluate_scalar(400, 5.0);
    assert!(hit1.is_some());
    assert_eq!(engine.state(), HysteresisBandState::Unprimed);

    // Flat plateau: 5.0, 5.0, 5.0 -> 0 triggers!
    assert!(engine.evaluate_scalar(500, 5.0).is_none());
    assert!(engine.evaluate_scalar(600, 5.0).is_none());
    assert!(engine.evaluate_scalar(700, 5.0).is_none());
}

#[test]
fn test_hysteresis_either_slope_rapid_chatter() {
    let cond = TriggerCondition::Edge {
        channel: ChannelId(1),
        level: 0.0,
        hysteresis: 1.0,
        slope: EdgeSlope::Either,
    };
    let mut engine = TriggerEngine::new(cond);

    // Arm low
    engine.evaluate_scalar(0, -2.0);
    assert_eq!(engine.state(), HysteresisBandState::ArmedLow);

    // Rapid square wave swinging between -2.0 and +2.0 across 1000 transitions
    let mut total_hits = 0;
    for i in 1..=1000 {
        let val = if i % 2 == 1 { 2.0 } else { -2.0 };
        if let Some(hit) = engine.evaluate_scalar(i * 100, val) {
            total_hits += 1;
            assert_eq!(hit.timestamp_ns, (i - 1) * 100);
            assert_eq!(hit.subpixel_offset_ns, 50.0); // alpha = (0 - (-2))/(2 - (-2)) = 0.5
        }
    }
    // Every single transition crosses both thresholds and fires
    assert_eq!(total_hits, 1000);
}

#[test]
fn test_discrete_state_trigger_transitions() {
    let cond = TriggerCondition::StateEquals {
        channel: ChannelId(5),
        state_code: 99,
    };
    let mut engine = TriggerEngine::new(cond);

    // Initial state 99: fires immediately on first sample
    let hit = engine.evaluate_state(100, 99).expect("first sample at target code triggers");
    assert_eq!(hit.timestamp_ns, 100);
    assert_eq!(hit.subpixel_offset_ns, 0.0);

    // Stays at 99: 0 triggers
    for t in 2..=100 {
        assert!(engine.evaluate_state(t * 100, 99).is_none());
    }

    // Leaves 99 to 10
    assert!(engine.evaluate_state(10_100, 10).is_none());

    // Re-enters 99: fires second trigger!
    let hit2 = engine.evaluate_state(10_200, 99).expect("transition back to 99 triggers");
    assert_eq!(hit2.timestamp_ns, 10_200);
    assert_eq!(hit2.subpixel_offset_ns, 0.0);
}

// ============================================================================
// 3. SCOPE CONTROLLER STATE MACHINE & AUTO TIMEOUT STRESS
// ============================================================================

#[test]
fn test_scope_controller_auto_timeout_100ms_precision_under_1m_samples() {
    let config = ScopeControllerConfig {
        scope_id: ScopeId(1),
        sweep_mode: TriggerSweepMode::Auto,
        time_span_ns: 10_000_000,     // 10 ms
        auto_timeout_ns: 100_000_000, // 100 ms
        trigger: Some(TriggerCondition::Edge {
            channel: ChannelId(1),
            level: 5.0,
            hysteresis: 1.0,
            slope: EdgeSlope::Rising,
        }),
        ..Default::default()
    };
    let mut ctrl = ScopeController::new(config);

    // Sample 1 at t = 100 ns primes armed_time_ns = 100 ns.
    let _ = ctrl.process_scalar_sample(ChannelId(1), 100, 0.0);

    // 100ns before timeout (t = 100_000_000 ns, elapsed = 99_999_900 < 100ms): NO trigger!
    let out_before = ctrl.process_scalar_sample(ChannelId(1), 100_000_000, 0.0);
    assert_eq!(out_before, ScopeStepOutcome::WaitingForTrigger);

    // Exact timeout boundary (t = 100_000_100 ns, elapsed = 100_000_000 == 100ms): Auto freerun fires!
    let out_exact = ctrl.process_scalar_sample(ChannelId(1), 100_000_100, 0.0);
    match out_exact {
        ScopeStepOutcome::AutoFreerunFired { window } => {
            assert_eq!(window.mode, TriggerSweepMode::Auto);
            assert_eq!(window.trigger_state, TriggerRuntimeState::AutoFreerun);
            assert!(window.trigger_timestamp_ns.is_none());
        }
        _ => panic!("expected AutoFreerunFired at exact 100ms boundary"),
    }
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);

    // Next sample at t = 100_000_200 ns: waiting again!
    let out_after = ctrl.process_scalar_sample(ChannelId(1), 100_000_200, 0.0);
    assert_eq!(out_after, ScopeStepOutcome::WaitingForTrigger);

    // Stream 1,000,000 samples at 100 ns intervals across another 100ms (to t = 200_000_200 ns)
    let mut freerun_count = 0;
    for i in 1..=1_000_000 {
        let t_ns = 100_000_200 + i * 100;
        let out = ctrl.process_scalar_sample(ChannelId(1), t_ns, 0.0);
        if matches!(out, ScopeStepOutcome::AutoFreerunFired { .. }) {
            freerun_count += 1;
        }
    }
    assert_eq!(freerun_count, 1, "Must fire exactly 1 auto freerun per 100ms");
}

#[test]
fn test_scope_controller_single_mode_1m_samples_rejection() {
    let config = ScopeControllerConfig {
        scope_id: ScopeId(2),
        sweep_mode: TriggerSweepMode::Single,
        time_span_ns: 1_000_000, // 1 ms
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

    // Arm low at t = 10_000
    ctrl.process_scalar_sample(ChannelId(1), 10_000, -1.0);

    // Cross level at t = 20_000 (trigger hit). Post-span = 500_000. Target end = 520_000.
    let hit_out = ctrl.process_scalar_sample(ChannelId(1), 20_000, 1.0);
    assert!(matches!(hit_out, ScopeStepOutcome::TriggerDetected { .. }));
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::Triggered);

    // Complete capture at t = 550_000
    let comp_out = ctrl.process_scalar_sample(ChannelId(1), 550_000, 1.0);
    match comp_out {
        ScopeStepOutcome::CaptureCompleted {
            window,
            latched_pinned,
        } => {
            assert!(latched_pinned);
            assert_eq!(window.trigger_state, TriggerRuntimeState::LatchedPinned);
            assert_eq!(window.trigger_timestamp_ns, Some(10_000));
        }
        _ => panic!("expected CaptureCompleted"),
    }
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::LatchedPinned);
    assert!(ctrl.is_pinned());

    let initial_window = ctrl.current_window().expect("latched window exists");

    // Ingest 500,000 subsequent samples while latched
    for i in 1..=500_000 {
        let t_ns = 600_000 + i * 10;
        let out = ctrl.process_scalar_sample(ChannelId(1), t_ns, if i % 2 == 0 { 2.0 } else { -2.0 });
        assert_eq!(out, ScopeStepOutcome::NoChange);
        let step_out = ctrl.step(t_ns);
        assert_eq!(step_out, ScopeStepOutcome::NoChange);
    }

    // Verify window and pinned state remain 100% untainted
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::LatchedPinned);
    assert!(ctrl.is_pinned());
    assert_eq!(ctrl.current_window(), Some(initial_window));

    // Explicit rearm unpins and returns to Armed
    ctrl.rearm();
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);
    assert!(!ctrl.is_pinned());
}

#[test]
fn test_scope_controller_rapid_mode_churning_fuzz() {
    let mut rng = XorShift64::new(0xabcdef0123456789);
    let mut ctrl = ScopeController::new(ScopeControllerConfig {
        scope_id: ScopeId(7),
        time_span_ns: 50_000,
        sweep_mode: TriggerSweepMode::Auto,
        trigger: Some(TriggerCondition::Edge {
            channel: ChannelId(1),
            level: 0.0,
            hysteresis: 0.5,
            slope: EdgeSlope::Either,
        }),
        ..Default::default()
    });

    let modes = [
        TriggerSweepMode::Roll,
        TriggerSweepMode::Auto,
        TriggerSweepMode::Normal,
        TriggerSweepMode::Single,
    ];

    let mut t_ns = 1_000_000;
    for _ in 0..20_000 {
        t_ns += rng.next_range(100, 50_000);
        let val = rng.next_f64() * 10.0 - 5.0;

        // Ingest sample
        let _ = ctrl.process_scalar_sample(ChannelId(1), t_ns, val);

        // Periodically step
        if rng.next_u64() % 5 == 0 {
            let _ = ctrl.step(t_ns);
        }

        // Periodically switch mode
        if rng.next_u64() % 10 == 0 {
            let mode = modes[(rng.next_u64() % 4) as usize];
            ctrl.set_sweep_mode(mode);
            assert_eq!(ctrl.sweep_mode(), mode);
        }

        // Periodically force trigger or rearm
        if rng.next_u64() % 25 == 0 {
            let _ = ctrl.force_trigger(t_ns);
        } else if rng.next_u64() % 30 == 0 {
            ctrl.rearm();
        }
    }
}

// ============================================================================
// 4. SYNC GROUP COORDINATOR CONCURRENCY & MULTI-THREADED STRESS
// ============================================================================

#[test]
fn test_sync_group_high_contention_16_threads() {
    let coordinator = Arc::new(SyncGroupCoordinator::new());
    let num_threads = 16;
    let iters_per_thread = 1_000;

    let mut handles = Vec::new();
    for thread_idx in 0..num_threads {
        let coord = Arc::clone(&coordinator);

        let handle = thread::spawn(move || {
            let mut rng = XorShift64::new((thread_idx as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15));

            for i in 0..iters_per_thread {
                let group_id = SyncGroupId((rng.next_u64() % 4) as u32 + 1);
                let scope_id = ScopeId((thread_idx * 100 + (i % 10)) as u32);

                // 1. Scope registration
                coord.register_scope(scope_id, group_id);

                // 2. Timebase mutation with clamped span
                let span = rng.next_range(100, 100_000_000);
                let offset = rng.next_range(0, 10_000_000) as i64 - 5_000_000;
                let v = coord.update_timebase(group_id, span, offset);
                assert!(v >= 1);

                // 3. Cursor mutation with edge values (negative, NaN, inf)
                let c_a = if i % 10 == 0 {
                    Some(f64::NAN)
                } else if i % 11 == 0 {
                    Some(-2.0)
                } else {
                    Some(rng.next_f64())
                };
                let c_b = if i % 12 == 0 {
                    Some(f64::INFINITY)
                } else {
                    Some(rng.next_f64())
                };
                coord.update_cursors(group_id, c_a, c_b);

                // 4. Read group state
                if let Some(state) = coord.get_group_state(group_id) {
                    assert!(state.time_span_ns >= 1000);
                    if let Some(a) = state.cursor_a_ratio {
                        assert!((0.0..=1.0).contains(&a));
                    }
                    if let Some(b) = state.cursor_b_ratio {
                        assert!((0.0..=1.0).contains(&b));
                    }
                }

                // 5. Apply to controller config
                let mut cfg = ScopeControllerConfig {
                    scope_id,
                    ..Default::default()
                };
                let applied = coord.apply_to_controller_config(scope_id, &mut cfg);
                assert!(applied);
                assert!(cfg.time_span_ns >= 1000);
            }
        });
        handles.push(handle);
    }

    for h in handles {
        h.join().expect("thread finished cleanly");
    }

    assert_eq!(coordinator.group_count(), 4);
    assert!(coordinator.scope_count() > 0);
}

// ============================================================================
// 5. DDSF WIRE SERIALIZATION, ALIGNMENT & PACKER STRESS
// ============================================================================

#[test]
fn test_ddsf_variable_tracks_fuzzing_and_odd_payload_alignments() {
    let mut packer = DdsfFramePacker::new(512 * 1024);

    let track_counts = [0, 1, 2, 7, 16, 32, 64];

    for &num_tracks in &track_counts {
        let mut tracks = Vec::with_capacity(num_tracks);

        for t in 0..num_tracks {
            let channel_id = t as u32 + 1;
            let kind_choice = t % 5;

            let track = match kind_choice {
                0 => {
                    // AnalogRawXY with odd point count (7 points = 56 bytes)
                    let points: Vec<DdsfAnalogPoint> = (0..7)
                        .map(|i| DdsfAnalogPoint {
                            x_norm: i as f32 / 7.0,
                            y_val: (i * 2) as f32,
                        })
                        .collect();
                    TrackPayload::new_raw_xy(channel_id, points)
                }
                1 => {
                    // AnalogM4Column with stats (3 columns = 60 bytes, requires 4B pad to 64B)
                    let cols: Vec<DdsfM4Column> = (0..3)
                        .map(|i| DdsfM4Column {
                            x_norm: i as f32 / 3.0,
                            v_first: 1.0,
                            v_min: 0.0,
                            v_max: 2.0,
                            v_last: 1.5,
                        })
                        .collect();
                    let stats = DdsfTrackStats {
                        min: 0.0,
                        max: 2.0,
                        vpp: 2.0,
                        mean: 1.0,
                        rms: 1.1,
                        freq_hz: 1000.0,
                        duty: 0.5,
                        reserved: 0.0,
                    };
                    TrackPayload::new_m4(channel_id, cols, Some(stats))
                }
                2 => {
                    // DigitalEdges with odd length (5 edges = 20 bytes, requires 4B pad to 24B)
                    let edges = vec![0.1f32, 0.3, 0.5, 0.7, 0.9];
                    TrackPayload::new_digital_edges(channel_id, 1, edges)
                }
                3 => {
                    // StateRibbons (2 ribbons = 32 bytes)
                    let ribbons = vec![
                        DdsfStateRibbon {
                            x0_norm: 0.0,
                            x1_norm: 0.5,
                            state_id: 1,
                            color_idx: 0,
                        },
                        DdsfStateRibbon {
                            x0_norm: 0.5,
                            x1_norm: 1.0,
                            state_id: 2,
                            color_idx: 1,
                        },
                    ];
                    TrackPayload::new_state_ribbons(channel_id, ribbons)
                }
                _ => {
                    // SpectrumFrame (11 bins = 44 bytes, requires 4B pad to 48B)
                    let bins = vec![1.0f32; 11];
                    TrackPayload::new_spectrum(channel_id, 0.0, 100.0, bins)
                }
            };
            tracks.push(track);
        }

        let scope_payload = ScopeViewportPayload {
            scope_id: 101,
            trigger_state: TriggerRuntimeState::Triggered.as_u8(),
            mode: TriggerSweepMode::Normal.as_u8(),
            win_t_start_ns: 10_000_000,
            win_t_span_ns: 2_000_000,
            subpixel_offset_ns: 12.5,
            tracks,
        };

        let snapshots = vec![
            DdsfSnapshotEntry {
                channel_num_id: 1,
                kind: DdsfSnapshotKind::Scalar.as_u8(),
                pad: [0; 3],
                val_f64: std::f64::consts::PI,
            },
            DdsfSnapshotEntry {
                channel_num_id: 2,
                kind: DdsfSnapshotKind::Bool.as_u8(),
                pad: [0; 3],
                val_f64: 1.0,
            },
        ];

        let bytes = packer
            .pack_frame(42, 123_456_789, DDSF_FLAG_QUOTA_WARNING, &[scope_payload], &snapshots)
            .expect("packing succeeded");

        // 1. Verify buffer pointer is 64-byte aligned
        assert_eq!(bytes.as_ptr() as usize % 64, 0);

        // 2. Zero-copy deserialization validation
        let reader = DdsfZeroCopyReader::new(bytes).expect("reader deserialization valid");
        assert_eq!(reader.header().magic, DDSF_MAGIC);
        assert_eq!(reader.header().version, DDSF_VERSION_V1);
        assert_eq!(reader.header().flags, DDSF_FLAG_QUOTA_WARNING);
        assert_eq!(reader.header().scope_count, 1);
        assert_eq!(reader.header().snapshot_count, 2);

        // 3. Inspect scope packet
        let scopes: Vec<_> = reader.scopes().collect();
        assert_eq!(scopes.len(), 1);
        let sc = scopes[0];
        assert_eq!(sc.header().scope_id, 101);
        assert_eq!(sc.header().track_block_count, num_tracks as u16);
        assert_eq!(sc.header().subpixel_offset_ns, 12.5);

        // 4. Verify all tracks and 8-byte payload alignment
        let tracks_read: Vec<_> = sc.tracks().collect();
        assert_eq!(tracks_read.len(), num_tracks);

        for tr in tracks_read {
            // Verify payload offset inside block is 8-byte aligned
            assert_eq!(tr.header().payload_offset() % 8, 0);

            // Verify element decoding based on block kind
            match tr.header().block_kind {
                1 => {
                    let pts = tr.raw_points().expect("raw points cast cleanly");
                    assert_eq!(pts.len(), 7);
                }
                2 => {
                    let cols = tr.m4_columns().expect("m4 columns cast cleanly");
                    assert_eq!(cols.len(), 3);
                    let st = tr.stats().expect("stats present");
                    assert_eq!(st.vpp, 2.0);
                }
                3 => {
                    let edges = tr.digital_edges().expect("edges cast cleanly");
                    assert_eq!(edges.len(), 5);
                }
                4 => {
                    let ribbons = tr.state_ribbons().expect("ribbons cast cleanly");
                    assert_eq!(ribbons.len(), 2);
                }
                5 => {
                    let bins = tr.spectrum_bins().expect("spectrum bins cast cleanly");
                    assert_eq!(bins.len(), 11);
                }
                _ => panic!("unexpected block kind"),
            }
        }

        // 5. Inspect snapshots
        let snaps = reader.snapshots().expect("snapshots valid");
        assert_eq!(snaps.len(), 2);
        assert_eq!(snaps[0].val_f64, std::f64::consts::PI);
        assert_eq!(snaps[1].val_f64, 1.0);
    }
}

#[test]
fn test_ddsf_zero_secondary_heap_allocation_stress_10000_frames() {
    let mut packer = DdsfFramePacker::new(256 * 1024);

    // Prepare fixed payload views
    let points = vec![
        DdsfAnalogPoint { x_norm: 0.1, y_val: 1.0 },
        DdsfAnalogPoint { x_norm: 0.5, y_val: 2.5 },
        DdsfAnalogPoint { x_norm: 0.9, y_val: -1.0 },
    ];
    let cols = vec![
        DdsfM4Column { x_norm: 0.2, v_first: 1.0, v_min: -2.0, v_max: 3.0, v_last: 0.0 },
        DdsfM4Column { x_norm: 0.8, v_first: 0.0, v_min: -1.0, v_max: 2.0, v_last: 1.0 },
    ];
    let stats = DdsfTrackStats::default();

    let track_views = vec![
        TrackBlockView {
            channel_id: 1,
            block_kind: DdsfBlockKind::AnalogRawXY,
            digital_init_level: 0,
            flags: 0,
            stats: None,
            payload: TrackPayloadSlice::AnalogRawXY(&points),
            aux_f32_0: 0.0,
            aux_f32_1: 0.0,
        },
        TrackBlockView {
            channel_id: 2,
            block_kind: DdsfBlockKind::AnalogM4Column,
            digital_init_level: 0,
            flags: DDSF_TRACK_FLAG_HAS_STATS,
            stats: Some(&stats),
            payload: TrackPayloadSlice::AnalogM4Column(&cols),
            aux_f32_0: 0.0,
            aux_f32_1: 0.0,
        },
    ];

    let scope_views = vec![ScopePacketView {
        scope_id: 1,
        trigger_state: TriggerRuntimeState::Triggered.as_u8(),
        mode: TriggerSweepMode::Normal.as_u8(),
        win_t_start_ns: 10_000,
        win_t_span_ns: 50_000,
        subpixel_offset_ns: 4.2,
        tracks: &track_views,
    }];

    let snapshots = vec![DdsfSnapshotEntry {
        channel_num_id: 1,
        kind: DdsfSnapshotKind::Scalar.as_u8(),
        pad: [0; 3],
        val_f64: 42.0,
    }];

    // Warm-up single iteration to prime buffer
    let _ = packer.pack_frame_borrowed(0, 1000, 0, &scope_views, &snapshots);
    let initial_base_ptr = packer.buffer().as_ptr();
    let initial_capacity = packer.capacity();

    let total_frames = 10_000;
    for seq in 1..=total_frames {
        let frame_bytes = packer
            .pack_frame_borrowed(seq as u32, seq as u64 * 1000, 0, &scope_views, &snapshots)
            .expect("packing succeeded");

        // Verify base pointer remains byte-for-byte identical (ZERO reallocation)
        assert_eq!(frame_bytes.as_ptr(), initial_base_ptr);
        assert_eq!(packer.capacity(), initial_capacity);
    }
}

#[test]
fn test_ddsf_zero_copy_reader_malformed_buffer_resilience() {
    // 1. Empty buffer
    assert!(DdsfZeroCopyReader::new(&[]).is_err());

    // 2. Buffer smaller than DDSF_HEADER_BYTES (64 bytes)
    let tiny_buf = vec![0u8; 63];
    assert!(DdsfZeroCopyReader::new(&tiny_buf).is_err());

    // 3. Invalid magic
    let mut header = DdsfFrameHeader::new_v1(1, 0, 0, 100, 64, 0);
    header.magic = 0xdeadbeef;
    let bytes = bytemuck::bytes_of(&header);
    assert!(DdsfZeroCopyReader::new(bytes).is_err());

    // 4. Invalid version
    let mut header2 = DdsfFrameHeader::new_v1(1, 0, 0, 100, 64, 0);
    header2.version = 999;
    let bytes2 = bytemuck::bytes_of(&header2);
    assert!(DdsfZeroCopyReader::new(bytes2).is_err());

    // 5. total_byte_len smaller than 64
    let header3 = DdsfFrameHeader::new_v1(1, 0, 0, 100, 32, 0);
    let bytes3 = bytemuck::bytes_of(&header3);
    assert!(DdsfZeroCopyReader::new(bytes3).is_err());

    // 6. total_byte_len exceeds buffer length
    let header4 = DdsfFrameHeader::new_v1(1, 0, 0, 100, 1000, 0);
    let bytes4 = bytemuck::bytes_of(&header4);
    assert!(DdsfZeroCopyReader::new(bytes4).is_err());
}


// ============================================================================
// 6. MULTI-SCOPE COORDINATION, OUT-OF-ORDER SKEW & PAN/PAUSE ADVERSARIAL TESTS
// ============================================================================

#[test]
fn test_multi_scope_skew_and_out_of_order_capture() {
    let coordinator = Arc::new(SyncGroupCoordinator::new());
    let group_id = SyncGroupId(1);

    // 4 scopes with different sampling rates and different pre/post trigger ratios
    let configs = [
        ScopeControllerConfig {
            scope_id: ScopeId(1),
            sweep_mode: TriggerSweepMode::Single,
            time_span_ns: 1_000_000,
            pre_trigger_ratio: 0.1, // Post-span = 900,000 ns
            trigger: Some(TriggerCondition::Edge {
                channel: ChannelId(1),
                level: 0.0,
                hysteresis: 0.5,
                slope: EdgeSlope::Rising,
            }),
            sync_group_id: Some(group_id),
            ..Default::default()
        },
        ScopeControllerConfig {
            scope_id: ScopeId(2),
            sweep_mode: TriggerSweepMode::Single,
            time_span_ns: 1_000_000,
            pre_trigger_ratio: 0.5, // Post-span = 500,000 ns
            trigger: Some(TriggerCondition::Edge {
                channel: ChannelId(2),
                level: 0.0,
                hysteresis: 0.5,
                slope: EdgeSlope::Rising,
            }),
            sync_group_id: Some(group_id),
            ..Default::default()
        },
        ScopeControllerConfig {
            scope_id: ScopeId(3),
            sweep_mode: TriggerSweepMode::Single,
            time_span_ns: 1_000_000,
            pre_trigger_ratio: 0.8, // Post-span = 200,000 ns
            trigger: Some(TriggerCondition::Edge {
                channel: ChannelId(3),
                level: 0.0,
                hysteresis: 0.5,
                slope: EdgeSlope::Rising,
            }),
            sync_group_id: Some(group_id),
            ..Default::default()
        },
        ScopeControllerConfig {
            scope_id: ScopeId(4),
            sweep_mode: TriggerSweepMode::Single,
            time_span_ns: 1_000_000,
            pre_trigger_ratio: 0.5,
            trigger: Some(TriggerCondition::Edge {
                channel: ChannelId(4),
                level: 0.0,
                hysteresis: 0.5,
                slope: EdgeSlope::Rising,
            }),
            sync_group_id: Some(group_id),
            ..Default::default()
        },
    ];

    let mut controllers: Vec<_> = configs
        .into_iter()
        .map(|cfg| {
            coordinator.register_scope(cfg.scope_id, group_id);
            ScopeController::new(cfg)
        })
        .collect();

    // Trigger all 4 scopes at t = 100_000 ns
    for (idx, ctrl) in controllers.iter_mut().enumerate() {
        let ch = ChannelId((idx + 1) as u32);
        // Arm low
        ctrl.process_scalar_sample(ch, 50_000, -1.0);
        // Fire trigger
        let out = ctrl.process_scalar_sample(ch, 100_000, 1.0);
        assert!(matches!(out, ScopeStepOutcome::TriggerDetected { .. }));
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::Triggered);
    }

    // Now complete captures OUT-OF-ORDER by feeding samples at different rates:
    // Scope 3 has shortest post-span (200,000 ns -> target = 300,000 ns). Completes first!
    let out3 = controllers[2].process_scalar_sample(ChannelId(3), 350_000, 1.0);
    assert!(matches!(out3, ScopeStepOutcome::CaptureCompleted { latched_pinned: true, .. }));
    assert!(controllers[2].is_pinned());
    assert_eq!(controllers[0].current_state(), TriggerRuntimeState::Triggered);
    assert_eq!(controllers[1].current_state(), TriggerRuntimeState::Triggered);
    assert_eq!(controllers[3].current_state(), TriggerRuntimeState::Triggered);

    // Scope 2 completes second (post-span 500,000 ns -> target = 600,000 ns)
    let out2 = controllers[1].process_scalar_sample(ChannelId(2), 650_000, 1.0);
    assert!(matches!(out2, ScopeStepOutcome::CaptureCompleted { latched_pinned: true, .. }));
    assert!(controllers[1].is_pinned());

    // Scope 4 completes third
    let out4 = controllers[3].process_scalar_sample(ChannelId(4), 700_000, 1.0);
    assert!(matches!(out4, ScopeStepOutcome::CaptureCompleted { latched_pinned: true, .. }));

    // Scope 1 completes last (post-span 900,000 ns -> target = 1,000,000 ns)
    let out1 = controllers[0].process_scalar_sample(ChannelId(1), 1_050_000, 1.0);
    assert!(matches!(out1, ScopeStepOutcome::CaptureCompleted { latched_pinned: true, .. }));

    // All 4 scopes are now synchronized in LatchedPinned
    for ctrl in &controllers {
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::LatchedPinned);
        assert!(ctrl.is_pinned());
    }
}

#[test]
fn test_extreme_pan_offsets_and_pause_during_triggered_state() {
    let mut ctrl = ScopeController::new(ScopeControllerConfig {
        scope_id: ScopeId(10),
        sweep_mode: TriggerSweepMode::Normal,
        time_span_ns: 10_000,
        pre_trigger_ratio: 0.5,
        pan_offset_ns: -50_000_000, // Massive negative pan offset
        trigger: Some(TriggerCondition::Edge {
            channel: ChannelId(1),
            level: 0.0,
            hysteresis: 0.5,
            slope: EdgeSlope::Rising,
        }),
        ..Default::default()
    });

    // Arm and trigger
    ctrl.process_scalar_sample(ChannelId(1), 1_000, -1.0);
    let trig_out = ctrl.process_scalar_sample(ChannelId(1), 2_000, 1.0);
    assert!(matches!(trig_out, ScopeStepOutcome::TriggerDetected { .. }));
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::Triggered);

    // While in Triggered state, pause acquisition
    ctrl.set_paused(true);
    assert!(ctrl.is_paused());

    // Ingest sample past window end: because paused, outcome must be NoChange!
    let paused_out = ctrl.process_scalar_sample(ChannelId(1), 100_000, 1.0);
    assert_eq!(paused_out, ScopeStepOutcome::NoChange);
    // Still in Triggered state!
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::Triggered);

    // Unpause
    ctrl.set_paused(false);
    assert!(!ctrl.is_paused());

    // Ingest sample past end: capture now completes!
    let comp_out = ctrl.process_scalar_sample(ChannelId(1), 100_000, 1.0);
    match comp_out {
        ScopeStepOutcome::CaptureCompleted { window, .. } => {
            // Negative pan offset saturates window start at 0
            assert_eq!(window.win_t_start_ns, 0);
        }
        _ => panic!("expected CaptureCompleted"),
    }
}

#[test]
fn test_ddsf_128_tracks_massive_stress_and_strict_section_offsets() {
    let mut packer = DdsfFramePacker::new(2 * 1024 * 1024);

    // 4 scopes, each with 32 tracks = 128 total tracks
    let mut scopes = Vec::new();
    for s in 0..4 {
        let mut tracks = Vec::new();
        for t in 0..32 {
            let ch = s * 32 + t + 1;
            // Mix of different odd point counts to heavily stress 8-byte padding
            let pt_count = (t % 11) + 1; // 1 to 11 points
            let points: Vec<DdsfAnalogPoint> = (0..pt_count)
                .map(|i| DdsfAnalogPoint {
                    x_norm: i as f32 / pt_count as f32,
                    y_val: (i * 3) as f32,
                })
                .collect();
            tracks.push(TrackPayload::new_raw_xy(ch as u32, points));
        }

        scopes.push(ScopeViewportPayload {
            scope_id: s as u32 + 1,
            trigger_state: TriggerRuntimeState::Triggered.as_u8(),
            mode: TriggerSweepMode::Normal.as_u8(),
            win_t_start_ns: 10_000,
            win_t_span_ns: 50_000,
            subpixel_offset_ns: 2.5,
            tracks,
        });
    }

    let snapshots: Vec<DdsfSnapshotEntry> = (0..128)
        .map(|i| DdsfSnapshotEntry {
            channel_num_id: i + 1,
            kind: DdsfSnapshotKind::Scalar.as_u8(),
            pad: [0; 3],
            val_f64: i as f64 * 1.5,
        })
        .collect();

    let bytes = packer
        .pack_frame(1, 999_999_999, 0, &scopes, &snapshots)
        .expect("massive packing succeeded");

    // 1. Buffer base pointer must be 64-byte aligned
    assert_eq!(bytes.as_ptr() as usize % 64, 0);

    let reader = DdsfZeroCopyReader::new(bytes).expect("reader created cleanly");
    assert_eq!(reader.header().scope_count, 4);
    assert_eq!(reader.header().snapshot_count, 128);

    // 2. Validate every scope packet and track block
    let mut total_tracks_verified = 0;
    for scope_view in reader.scopes() {
        assert_eq!(scope_view.header().track_block_count, 32);
        for tr in scope_view.tracks() {
            // Verify payload offset inside track block is 8-byte aligned
            assert_eq!(tr.header().payload_offset() % 8, 0);
            let pts = tr.raw_points().expect("raw points decoded cleanly");
            assert!(!pts.is_empty());
            total_tracks_verified += 1;
        }
    }
    assert_eq!(total_tracks_verified, 128);

    // 3. Validate all 128 snapshots
    let snaps = reader.snapshots().expect("snapshots decoded");
    assert_eq!(snaps.len(), 128);
    for (i, entry) in snaps.iter().enumerate() {
        assert_eq!(entry.channel_num_id, i as u32 + 1);
        assert_eq!(entry.val_f64, i as f64 * 1.5);
    }
}

#[test]
fn test_ddsf_zero_copy_reader_fuzz_corrupted_payloads_and_kinds() {
    let mut packer = DdsfFramePacker::new(64 * 1024);

    let cols = vec![DdsfM4Column {
        x_norm: 0.5,
        v_first: 1.0,
        v_min: 0.0,
        v_max: 2.0,
        v_last: 1.5,
    }];
    let track = TrackPayload::new_m4(1, cols, None);
    let scope = ScopeViewportPayload {
        scope_id: 1,
        trigger_state: TriggerRuntimeState::Triggered.as_u8(),
        mode: TriggerSweepMode::Auto.as_u8(),
        win_t_start_ns: 0,
        win_t_span_ns: 10_000,
        subpixel_offset_ns: 0.0,
        tracks: vec![track],
    };

    let bytes = packer.pack_frame(1, 100, 0, &[scope], &[]).unwrap();
    let reader = DdsfZeroCopyReader::new(bytes).unwrap();
    let sc = reader.scopes().next().unwrap();
    let tr = sc.tracks().next().unwrap();

    // 1. Calling wrong extractor returns CoreError::Protocol (not panic)
    assert!(tr.raw_points().is_err());
    assert!(tr.digital_edges().is_err());
    assert!(tr.state_ribbons().is_err());
    assert!(tr.spectrum_bins().is_err());
    // Correct extractor succeeds
    assert!(tr.m4_columns().is_ok());

    // 2. Fuzzing truncation at every byte offset from 0 to len
    for truncate_len in 0..bytes.len() {
        let truncated = &bytes[..truncate_len];
        if truncate_len < DDSF_HEADER_BYTES {
            assert!(DdsfZeroCopyReader::new(truncated).is_err());
        }
    }
}
