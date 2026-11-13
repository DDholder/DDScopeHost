//! Empirical Challenger 2 Adversarial Stress Suite for Milestone TA4.
//!
//! Conforms to Phase 1 Track A Milestone TA4 specification in `docs/04-phase1-design-and-plan.md` §3.5.
//!
//! Specifically validates:
//! 1. `ScopeController` state machine:
//!    - Dynamic mode switching: switch between Roll, Auto, Normal, Single mid-sweep while samples are streaming.
//!      Verify no illegal state transitions, panic, or unpinned corruption.
//!    - Single mode latching stress: verify that once in `LatchedPinned`, streaming 1,000,000 more samples does not
//!      corrupt window or trigger another capture until explicit `rearm()`.
//!    - Auto mode timeout boundary fuzzing: verify transition to `AutoFreerun` happens at and after auto_timeout_ns
//!      (100ms) and resets properly.
//!    - Panning and zooming stress: test negative pan offsets, extreme spans (1ns to 100s), and ratio boundaries.
//! 2. `SyncGroupCoordinator` multi-threaded concurrency stress:
//!    - Concurrently spawn 16 threads mutating timebases, cursors, and scope registrations across multiple sync groups simultaneously.
//!    - Verify zero deadlocks, zero panics, monotonic version increments, and consistent state reads.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use ddscope_core::scope::{
    calculate_subsample_offset, EdgeSlope, ScopeController, ScopeControllerConfig,
    ScopeStepOutcome, SyncGroupCoordinator, TriggerCondition, TriggerRuntimeState,
    TriggerSweepMode,
};
use ddscope_core::types::{ChannelId, ScopeId, SyncGroupId, TimestampNs};

/// Simple deterministic pseudo-random generator (XorShift64) for reproducible fuzzing.
struct XorShift64 {
    state: u64,
}

impl XorShift64 {
    fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 { 0xdeadbeefcafe } else { seed },
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
// 1. DYNAMIC MODE SWITCHING MID-STREAM FUZZING
// ============================================================================

#[test]
fn test_scope_controller_dynamic_mode_switching_fuzz() {
    let mut rng = XorShift64::new(0x12345678abcdef01);

    let config = ScopeControllerConfig {
        scope_id: ScopeId(1),
        canvas_width_px: 2000,
        time_span_ns: 10_000_000, // 10 ms
        pre_trigger_ratio: 0.5,
        pan_offset_ns: 0,
        sweep_mode: TriggerSweepMode::Auto,
        auto_timeout_ns: 100_000_000,
        trigger: Some(TriggerCondition::Edge {
            channel: ChannelId(1),
            level: 0.0,
            hysteresis: 0.5,
            slope: EdgeSlope::Rising,
        }),
        sync_group_id: None,
        paused: false,
        title: None,
    };

    let mut ctrl = ScopeController::new(config);
    let mut current_time_ns: TimestampNs = 1_000_000;
    let modes = [
        TriggerSweepMode::Roll,
        TriggerSweepMode::Auto,
        TriggerSweepMode::Normal,
        TriggerSweepMode::Single,
    ];

    let total_steps = 30_000;
    for step in 0..total_steps {
        // Advance time by 100ns to 10,000ns
        let dt = rng.next_range(100, 10_000);
        current_time_ns = current_time_ns.saturating_add(dt);

        // Generate scalar sample alternating around trigger level
        let sample_val = match step % 4 {
            0 => -1.5, // primes armed low
            1 => -0.2, // in band
            2 => 1.5,  // crossing trigger
            _ => 0.1,  // in band
        };

        // Random mode switch with 5% probability
        if rng.next_u64() % 20 == 0 {
            let next_mode = modes[(rng.next_u64() % 4) as usize];
            ctrl.set_sweep_mode(next_mode);
        }

        // Random control actions with low probability
        let action = rng.next_u64() % 100;
        if action == 1 {
            ctrl.rearm();
        } else if action == 2 {
            ctrl.force_trigger(current_time_ns);
        } else if action == 3 {
            let new_span = rng.next_range(1_000, 50_000_000);
            ctrl.set_time_span_ns(new_span);
        } else if action == 4 {
            let new_pan = (rng.next_range(0, 2_000_000) as i64) - 1_000_000;
            ctrl.set_pan_offset_ns(new_pan);
        }

        // Process sample
        let outcome = ctrl.process_scalar_sample(ChannelId(1), current_time_ns, sample_val);

        // Invariant checks:
        let state = ctrl.current_state();
        let mode = ctrl.sweep_mode();
        let is_pinned = ctrl.is_pinned();

        // 1. Roll mode invariant: state must be Roll
        if mode == TriggerSweepMode::Roll {
            assert_eq!(
                state,
                TriggerRuntimeState::Roll,
                "Roll mode must always remain in Roll state"
            );
            assert!(
                !is_pinned,
                "Roll mode must never be pinned"
            );
        }

        // 2. Memory pinning invariant: is_pinned == true ONLY when in LatchedPinned
        if is_pinned {
            assert_eq!(
                state,
                TriggerRuntimeState::LatchedPinned,
                "is_pinned must only be true in LatchedPinned state"
            );
            assert_eq!(
                mode,
                TriggerSweepMode::Single,
                "LatchedPinned can only be reached in Single mode"
            );
        }

        // 3. State transition validity for non-Roll modes
        if mode != TriggerSweepMode::Roll {
            match state {
                TriggerRuntimeState::Armed
                | TriggerRuntimeState::AutoFreerun
                | TriggerRuntimeState::Triggered
                | TriggerRuntimeState::LatchedPinned => {}
                TriggerRuntimeState::Roll => {
                    panic!("Non-roll mode {mode:?} cannot be in Roll state!");
                }
            }
        }

        // 4. In LatchedPinned state, outcome must be NoChange
        if state == TriggerRuntimeState::LatchedPinned {
            assert_eq!(
                outcome,
                ScopeStepOutcome::NoChange,
                "LatchedPinned state must reject further sample updates with NoChange"
            );
        }

        // 5. Computed window validity
        if let Some(win) = ctrl.current_window() {
            assert!(
                win.win_t_span_ns >= 1000,
                "Window span must be >= 1000 ns, got {}",
                win.win_t_span_ns
            );
            assert!(
                win.subpixel_offset_ns >= 0.0,
                "Subpixel offset must be >= 0, got {}",
                win.subpixel_offset_ns
            );
            assert!(
                win.subpixel_offset_ns.is_finite(),
                "Subpixel offset must be finite"
            );
        }
    }
}

// ============================================================================
// 2. SINGLE MODE LATCHING STRESS (1,000,000 SAMPLES)
// ============================================================================

#[test]
fn test_single_mode_latching_stress_one_million_samples() {
    let config = ScopeControllerConfig {
        scope_id: ScopeId(10),
        time_span_ns: 1_000_000, // 1 ms span
        pre_trigger_ratio: 0.5,   // 500 us pre-trigger, 500 us post-trigger
        sweep_mode: TriggerSweepMode::Single,
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

    // 1. Prime debouncer low at t = 100_000
    let out0 = ctrl.process_scalar_sample(ChannelId(1), 100_000, -1.0);
    assert_eq!(out0, ScopeStepOutcome::WaitingForTrigger);

    // 2. Edge crossing at t = 200_000 (v = 1.0). Target end = 200_000 + 500_000 = 700_000.
    let out1 = ctrl.process_scalar_sample(ChannelId(1), 200_000, 1.0);
    assert!(
        matches!(out1, ScopeStepOutcome::TriggerDetected { .. }),
        "Expected TriggerDetected, got {:?}",
        out1
    );
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::Triggered);

    // 3. Post-trigger sample at t = 750_000 completes acquisition window -> enters LatchedPinned!
    let out2 = ctrl.process_scalar_sample(ChannelId(1), 750_000, 1.0);
    let latched_window = match out2 {
        ScopeStepOutcome::CaptureCompleted {
            window,
            latched_pinned,
        } => {
            assert!(latched_pinned, "Single mode must latch pinned");
            assert_eq!(window.mode, TriggerSweepMode::Single);
            assert_eq!(window.trigger_state, TriggerRuntimeState::LatchedPinned);
            assert_eq!(window.trigger_timestamp_ns, Some(100_000));
            window
        }
        _ => panic!("Expected CaptureCompleted, got {:?}", out2),
    };

    assert_eq!(ctrl.current_state(), TriggerRuntimeState::LatchedPinned);
    assert!(ctrl.is_pinned(), "Controller must report is_pinned() == true");
    assert_eq!(ctrl.current_window(), Some(latched_window));

    // 4. Stream 1,000,000 subsequent adversarial samples:
    //    Include threshold crossings, negative values, spikes, NaN, Inf, extreme timestamps.
    let mut rng = XorShift64::new(0xabcdef9876543210);
    let mut ts = 750_000_u64;

    for i in 0..1_000_000 {
        ts = ts.saturating_add(1000);
        let val = match i % 5 {
            0 => -100.0,
            1 => 100.0,
            2 => 0.0,
            3 => f64::NAN,
            _ => (rng.next_f64() - 0.5) * 1000.0,
        };

        let outcome = ctrl.process_scalar_sample(ChannelId(1), ts, val);
        assert_eq!(
            outcome,
            ScopeStepOutcome::NoChange,
            "LatchedPinned controller must emit NoChange at sample {i}"
        );
        // Verify state is not corrupted
        if i % 100_000 == 0 {
            assert_eq!(ctrl.current_state(), TriggerRuntimeState::LatchedPinned);
            assert!(ctrl.is_pinned());
            assert_eq!(ctrl.current_window(), Some(latched_window));
        }
    }

    // 5. Step timer into distant future
    let step_out = ctrl.step(1_000_000_000_000);
    assert_eq!(step_out, ScopeStepOutcome::NoChange);
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::LatchedPinned);
    assert!(ctrl.is_pinned());
    assert_eq!(ctrl.current_window(), Some(latched_window));

    // 6. Explicit rearm() unlocks the latch
    ctrl.rearm();
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);
    assert!(!ctrl.is_pinned());

    // 7. Verify subsequent acquisition captures a new window
    let base_t = 1_000_000_000_000_u64;
    ctrl.process_scalar_sample(ChannelId(1), base_t, -1.0);
    let out_new_trig = ctrl.process_scalar_sample(ChannelId(1), base_t + 10_000, 1.0);
    assert!(matches!(
        out_new_trig,
        ScopeStepOutcome::TriggerDetected { .. }
    ));
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::Triggered);

    let out_new_comp = ctrl.process_scalar_sample(ChannelId(1), base_t + 600_000, 1.0);
    assert!(matches!(
        out_new_comp,
        ScopeStepOutcome::CaptureCompleted {
            latched_pinned: true,
            ..
        }
    ));
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::LatchedPinned);
    assert!(ctrl.is_pinned());
}

// ============================================================================
// 3. AUTO MODE TIMEOUT BOUNDARY FUZZING
// ============================================================================

#[test]
fn test_auto_mode_timeout_boundary_fuzzing() {
    let timeout_ns = 100_000_000; // 100 ms
    let config = ScopeControllerConfig {
        scope_id: ScopeId(5),
        time_span_ns: 10_000_000, // 10 ms
        auto_timeout_ns: timeout_ns,
        sweep_mode: TriggerSweepMode::Auto,
        trigger: Some(TriggerCondition::Edge {
            channel: ChannelId(1),
            level: 0.0,
            hysteresis: 0.5,
            slope: EdgeSlope::Rising,
        }),
        ..Default::default()
    };

    let mut ctrl = ScopeController::new(config);
    let start_t = 1_000_000_000_u64; // 1.0s

    // First sample initializes armed_time_ns
    let out0 = ctrl.process_scalar_sample(ChannelId(1), start_t, -0.1);
    assert_eq!(out0, ScopeStepOutcome::WaitingForTrigger);
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);

    // Test 1: Step at 99,999,999 ns elapsed -> MUST NOT FIRE
    let out_before = ctrl.step(start_t + timeout_ns - 1);
    assert_eq!(
        out_before,
        ScopeStepOutcome::WaitingForTrigger,
        "Auto timeout must not fire at 99.999999 ms"
    );
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);

    // Test 2: Step at exactly 100,000,000 ns elapsed -> MUST FIRE
    let out_exact = ctrl.step(start_t + timeout_ns);
    match out_exact {
        ScopeStepOutcome::AutoFreerunFired { window } => {
            assert_eq!(window.mode, TriggerSweepMode::Auto);
            assert_eq!(window.trigger_state, TriggerRuntimeState::AutoFreerun);
            assert_eq!(window.subpixel_offset_ns, 0.0);
            assert!(window.trigger_timestamp_ns.is_none());
            // Window start: (latest_ns - span) = (start_t + timeout_ns) - 10ms
            assert_eq!(window.win_t_start_ns, start_t + timeout_ns - 10_000_000);
        }
        _ => panic!("Expected AutoFreerunFired at exactly 100ms, got {:?}", out_exact),
    }

    // Immediately re-armed
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);

    // Test 3: Next boundary is at 200 ms. Step at 150 ms -> still waiting
    let out_mid = ctrl.step(start_t + timeout_ns + 50_000_000);
    assert_eq!(out_mid, ScopeStepOutcome::WaitingForTrigger);

    // Step at 200 ms -> fires again!
    let out_exact2 = ctrl.step(start_t + timeout_ns * 2);
    assert!(matches!(
        out_exact2,
        ScopeStepOutcome::AutoFreerunFired { .. }
    ));
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);

    // Test 4: Incoming scalar sample triggers timeout if elapsed >= 100ms
    let t_now = start_t + timeout_ns * 2;
    // Advance sample to t_now + 99,999,999 -> waiting
    let out_s1 = ctrl.process_scalar_sample(ChannelId(1), t_now + timeout_ns - 1, -0.1);
    assert_eq!(out_s1, ScopeStepOutcome::WaitingForTrigger);

    // Advance sample to t_now + 100,000,000 -> fires auto freerun!
    let out_s2 = ctrl.process_scalar_sample(ChannelId(1), t_now + timeout_ns, -0.1);
    assert!(matches!(
        out_s2,
        ScopeStepOutcome::AutoFreerunFired { .. }
    ));

    // Test 5: Real trigger takes precedence over auto-timeout
    let t_base = t_now + timeout_ns;
    // Prime low at t_base + 10ms
    ctrl.process_scalar_sample(ChannelId(1), t_base + 10_000_000, -1.0);
    // Real trigger fires at t_base + 12ms (post_span is 5ms -> target_end is 10ms + 5ms = 15ms)
    let trig_out = ctrl.process_scalar_sample(ChannelId(1), t_base + 12_000_000, 1.0);
    assert!(matches!(
        trig_out,
        ScopeStepOutcome::TriggerDetected { .. }
    ));
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::Triggered);

    // Complete window at 16ms (>= 15ms target_end)
    let comp_out = ctrl.process_scalar_sample(ChannelId(1), t_base + 16_000_000, 1.0);
    assert!(matches!(
        comp_out,
        ScopeStepOutcome::CaptureCompleted {
            latched_pinned: false,
            ..
        }
    ));
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);

    // After capture completion, the timeout timer resets from latest_sample_time (t_base + 16ms).
    // So stepping at t_base + 100ms must NOT fire freerun (only 84ms elapsed since last capture)!
    let step_no_freerun = ctrl.step(t_base + 100_000_000);
    assert_eq!(
        step_no_freerun,
        ScopeStepOutcome::WaitingForTrigger,
        "Auto-timeout must reset after a genuine capture"
    );

    // But stepping at t_base + 116ms (100ms after last capture at 16ms) MUST fire freerun!
    let step_freerun_after = ctrl.step(t_base + 116_000_000);
    assert!(matches!(
        step_freerun_after,
        ScopeStepOutcome::AutoFreerunFired { .. }
    ));
}

// ============================================================================
// 4. PANNING AND ZOOMING STRESS TEST
// ============================================================================

#[test]
fn test_panning_and_zooming_edge_cases() {
    let mut config = ScopeControllerConfig {
        scope_id: ScopeId(99),
        time_span_ns: 10_000_000,
        pre_trigger_ratio: 0.5,
        pan_offset_ns: 0,
        sweep_mode: TriggerSweepMode::Roll,
        ..Default::default()
    };

    let mut ctrl = ScopeController::new(config.clone());

    // 1. Extreme negative pan offsets
    let negative_pans = [
        -1_i64,
        -10_000,
        -10_000_000,
        -100_000_000_000, // -100 seconds
        -1_000_000_000_000_000,
    ];

    for &pan in &negative_pans {
        ctrl.set_pan_offset_ns(pan);
        // Small sample time: win_start must saturate safely to 0
        let out = ctrl.process_scalar_sample(ChannelId(1), 5_000, 1.0);
        match out {
            ScopeStepOutcome::Rolling { window } => {
                assert_eq!(
                    window.win_t_start_ns, 0,
                    "Negative pan offset must clamp window start to 0, got {}",
                    window.win_t_start_ns
                );
            }
            _ => panic!("Expected Rolling outcome"),
        }
    }

    // 2. Extreme spans (1 ns to 100 seconds)
    // set_time_span_ns clamps to minimum 1000 ns
    ctrl.set_time_span_ns(1);
    assert_eq!(ctrl.config().time_span_ns, 1000);

    ctrl.set_time_span_ns(100);
    assert_eq!(ctrl.config().time_span_ns, 1000);

    // 100 seconds = 100_000_000_000 ns
    ctrl.set_time_span_ns(100_000_000_000);
    assert_eq!(ctrl.config().time_span_ns, 100_000_000_000);

    ctrl.set_pan_offset_ns(0);
    let out_span = ctrl.process_scalar_sample(ChannelId(1), 200_000_000_000, 2.0);
    match out_span {
        ScopeStepOutcome::Rolling { window } => {
            assert_eq!(window.win_t_span_ns, 100_000_000_000);
            assert_eq!(window.win_t_start_ns, 100_000_000_000);
        }
        _ => panic!("Expected Rolling outcome"),
    }

    // 3. Pre-trigger ratio boundaries
    // Test negative ratio clamp
    config.pre_trigger_ratio = -0.5;
    config.sweep_mode = TriggerSweepMode::Normal;
    config.trigger = Some(TriggerCondition::Edge {
        channel: ChannelId(1),
        level: 0.0,
        hysteresis: 0.1,
        slope: EdgeSlope::Rising,
    });
    let mut ctrl_ratio = ScopeController::new(config);
    // Force trigger at 50_000
    let out_f = ctrl_ratio.force_trigger(50_000);
    // Pre-trigger ratio clamped to 0.0 -> pre-span = 0 -> target_end = 50_000 + 10_000_000
    assert!(matches!(out_f, ScopeStepOutcome::TriggerDetected { .. }));
    ctrl_ratio.step(50_000 + 10_000_000);
    let win = ctrl_ratio.current_window().expect("window exists");
    assert_eq!(win.win_t_start_ns, 50_000);

    // Test ratio > 1.0 clamp
    let config_over = ScopeControllerConfig {
        pre_trigger_ratio: 2.5,
        sweep_mode: TriggerSweepMode::Normal,
        time_span_ns: 10_000,
        ..Default::default()
    };
    let mut ctrl_over = ScopeController::new(config_over);
    let out_f2 = ctrl_over.force_trigger(100_000);
    // Pre-trigger ratio clamped to 1.0 -> pre-span = 10_000, post-span = 0.
    // target_end = 100_000 + 0 = 100_000.
    // Since latest_sample_time == 100_000 >= target_end, completes immediately!
    assert!(matches!(out_f2, ScopeStepOutcome::CaptureCompleted { .. }));
    let win2 = ctrl_over.current_window().expect("window exists");
    assert_eq!(win2.win_t_start_ns, 90_000); // 100_000 - 10_000
}

// ============================================================================
// 5. SYNC GROUP COORDINATOR 16-THREAD CONCURRENCY STRESS TEST
// ============================================================================

#[test]
fn test_sync_group_coordinator_16_threads_concurrency() {
    let coordinator = Arc::new(SyncGroupCoordinator::new());
    let num_threads = 16;
    let iterations_per_thread = 2_000;

    let failed = Arc::new(AtomicBool::new(false));
    let mut handles = Vec::with_capacity(num_threads);

    for t_idx in 0..num_threads {
        let coord = Arc::clone(&coordinator);
        let flag = Arc::clone(&failed);

        let handle = thread::spawn(move || {
            let mut rng = XorShift64::new(0xdeadbeef1000 + t_idx as u64);
            let mut last_observed_version = [0_u64; 4];

            for iter in 0..iterations_per_thread {
                let group_id = SyncGroupId(((t_idx + iter) % 4) as u32 + 1);
                let scope_id = ScopeId((t_idx * 10 + (iter % 10)) as u32);

                // Action 1: Register scope
                coord.register_scope(scope_id, group_id);

                // Action 2: Atomic timebase update
                let span = rng.next_range(1_000, 50_000_000);
                let pan = (rng.next_range(0, 10_000) as i64) - 5_000;
                let v_timebase = coord.update_timebase(group_id, span, pan);
                if v_timebase <= last_observed_version[(group_id.0 - 1) as usize] {
                    eprintln!(
                        "Thread {t_idx}: Non-monotonic version {v_timebase} <= {}",
                        last_observed_version[(group_id.0 - 1) as usize]
                    );
                    flag.store(true, Ordering::SeqCst);
                }
                last_observed_version[(group_id.0 - 1) as usize] = v_timebase;

                // Action 3: Atomic cursor update
                let c_a = if rng.next_u64() % 3 == 0 {
                    None
                } else {
                    Some(rng.next_f64() * 1.5 - 0.2) // intentionally includes out-of-bounds to test clamping
                };
                let c_b = if rng.next_u64() % 3 == 0 {
                    None
                } else {
                    Some(rng.next_f64())
                };
                let v_cursor = coord.update_cursors(group_id, c_a, c_b);
                if v_cursor <= last_observed_version[(group_id.0 - 1) as usize] {
                    eprintln!(
                        "Thread {t_idx}: Non-monotonic cursor version {v_cursor} <= {}",
                        last_observed_version[(group_id.0 - 1) as usize]
                    );
                    flag.store(true, Ordering::SeqCst);
                }
                last_observed_version[(group_id.0 - 1) as usize] = v_cursor;

                // Action 4: Read state and verify consistency
                if let Some(state) = coord.get_group_state(group_id) {
                    if state.time_span_ns < 1000 {
                        eprintln!("Invalid span in state: {}", state.time_span_ns);
                        flag.store(true, Ordering::SeqCst);
                    }
                    if let Some(a) = state.cursor_a_ratio {
                        if !(0.0..=1.0).contains(&a) {
                            eprintln!("Unclamped cursor A ratio: {a}");
                            flag.store(true, Ordering::SeqCst);
                        }
                    }
                    if let Some(b) = state.cursor_b_ratio {
                        if !(0.0..=1.0).contains(&b) {
                            eprintln!("Unclamped cursor B ratio: {b}");
                            flag.store(true, Ordering::SeqCst);
                        }
                    }
                }

                // Action 5: Scope group mapping query
                let mapped = coord.get_scope_group(scope_id);
                if let Some(g) = mapped {
                    let scopes = coord.get_group_scopes(g);
                    // Scopes list should contain scope_id or another thread updated it
                    let _ = scopes.len();
                }

                // Action 6: Apply to controller config
                let mut dummy_config = ScopeControllerConfig::default();
                coord.apply_to_controller_config(scope_id, &mut dummy_config);

                // Action 7: Periodic unregister
                if iter % 10 == 0 {
                    coord.unregister_scope(scope_id);
                }
            }
        });

        handles.push(handle);
    }

    for (i, h) in handles.into_iter().enumerate() {
        h.join()
            .unwrap_or_else(|_| panic!("Thread {i} panicked during concurrency stress test"));
    }

    assert!(
        !failed.load(Ordering::SeqCst),
        "Concurrency stress test encountered version monotonicity or state consistency violations!"
    );

    // Verify all 4 groups exist and have huge versions
    assert_eq!(coordinator.group_count(), 4);
    for g in 1..=4 {
        let state = coordinator
            .get_group_state(SyncGroupId(g))
            .expect("group exists");
        assert!(
            state.version > 1_000,
            "Group {g} version must reflect thousands of updates, got {}",
            state.version
        );
    }
}

// ============================================================================
// 6. DISCRETE STATE TRIGGER SINGLE MODE LATCHING STRESS
// ============================================================================

#[test]
fn test_discrete_state_single_mode_latching_stress() {
    let config = ScopeControllerConfig {
        scope_id: ScopeId(77),
        time_span_ns: 10_000, // 10 us span
        pre_trigger_ratio: 0.2, // 2 us pre-trigger, 8 us post-trigger
        sweep_mode: TriggerSweepMode::Single,
        trigger: Some(TriggerCondition::StateEquals {
            channel: ChannelId(2),
            state_code: 42,
        }),
        ..Default::default()
    };

    let mut ctrl = ScopeController::new(config);

    // Initial state 0
    ctrl.process_state_sample(ChannelId(2), 10_000, 0);
    // Transition to 42 triggers! (hit at t = 20_000, target_end = 20_000 + 8_000 = 28_000)
    let trig_out = ctrl.process_state_sample(ChannelId(2), 20_000, 42);
    assert!(matches!(trig_out, ScopeStepOutcome::TriggerDetected { .. }));
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::Triggered);

    // Advance to 30_000 -> completes capture and latches pinned
    let comp_out = ctrl.process_state_sample(ChannelId(2), 30_000, 42);
    let latched_win = match comp_out {
        ScopeStepOutcome::CaptureCompleted {
            window,
            latched_pinned,
        } => {
            assert!(latched_pinned);
            assert_eq!(window.mode, TriggerSweepMode::Single);
            assert_eq!(window.trigger_state, TriggerRuntimeState::LatchedPinned);
            assert_eq!(window.win_t_start_ns, 18_000); // 20_000 - 2_000
            assert_eq!(window.win_t_span_ns, 10_000);
            assert_eq!(window.trigger_timestamp_ns, Some(20_000));
            assert_eq!(window.subpixel_offset_ns, 0.0);
            window
        }
        _ => panic!("Expected CaptureCompleted"),
    };

    assert_eq!(ctrl.current_state(), TriggerRuntimeState::LatchedPinned);
    assert!(ctrl.is_pinned());

    // Stream 100,000 subsequent state transitions with arbitrary codes
    for i in 0..100_000 {
        let t = 30_000 + (i as u64) * 100;
        let code = match i % 6 {
            0 => 42,
            1 => 0,
            2 => -1,
            3 => i32::MAX,
            4 => i32::MIN,
            _ => 100,
        };
        let out = ctrl.process_state_sample(ChannelId(2), t, code);
        assert_eq!(out, ScopeStepOutcome::NoChange);
        if i % 20_000 == 0 {
            assert_eq!(ctrl.current_state(), TriggerRuntimeState::LatchedPinned);
            assert!(ctrl.is_pinned());
            assert_eq!(ctrl.current_window(), Some(latched_win));
        }
    }

    // Rearm
    ctrl.rearm();
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);
    assert!(!ctrl.is_pinned());
}

// ============================================================================
// 7. SUBSAMPLE LINEAR INTERPOLATION ADVERSARIAL NUMERICAL STRESS
// ============================================================================

#[test]
fn test_subsample_interpolation_adversarial_numbers() {
    // 1. Extreme large numbers without float overflow
    let res_large = calculate_subsample_offset(1_000, -1e300, 2_000, 1e300, 0.0);
    assert!(res_large.is_some());
    let (alpha, offset) = res_large.unwrap();
    assert!((alpha - 0.5).abs() < 1e-9);
    assert!((offset - 500.0).abs() < 1e-4);

    // 2. Denominator below 1e-12 threshold snaps to nearest boundary (0.0 or 1.0)
    let v0 = 1e-300;
    let v1 = 3e-300;
    let lvl = 2e-300;
    let res_small = calculate_subsample_offset(0, v0, 100, v1, lvl);
    assert!(res_small.is_some());
    let (alpha_s, offset_s) = res_small.unwrap();
    // denom < 1e-12 snaps to nearest bound: level - v0 <= level - v1 -> 0.0
    assert_eq!(alpha_s, 0.0);
    assert_eq!(offset_s, 0.0);

    // When level is closer to v1:
    let (alpha_s2, offset_s2) = calculate_subsample_offset(0, v0, 100, v1, 2.8e-300).unwrap();
    assert_eq!(alpha_s2, 1.0);
    assert_eq!(offset_s2, 100.0);

    // 2b. Denominator above 1e-12 (e.g. 1e-6 microvolts) linearly interpolates accurately
    let v0_u = 1.000_000_0;
    let v1_u = 1.000_004_0; // dt = 4 uV
    let lvl_u = 1.000_001_0; // 1 uV above v0 -> alpha = 0.25
    let (alpha_u, offset_u) = calculate_subsample_offset(0, v0_u, 100, v1_u, lvl_u).unwrap();
    assert!((alpha_u - 0.25).abs() < 1e-9);
    assert!((offset_u - 25.0).abs() < 1e-4);

    // 3. Signed zeroes (-0.0 vs +0.0)
    let res_zeros = calculate_subsample_offset(100, -0.0, 200, 0.0, 0.0);
    assert!(res_zeros.is_some());
    let (alpha_z, offset_z) = res_zeros.unwrap();
    assert_eq!(alpha_z, 0.0);
    assert_eq!(offset_z, 0.0);

    // 4. Exact level match at v0
    let res_at_v0 = calculate_subsample_offset(100, 5.0, 200, 10.0, 5.0);
    let (alpha_v0, offset_v0) = res_at_v0.unwrap();
    assert_eq!(alpha_v0, 0.0);
    assert_eq!(offset_v0, 0.0);

    // 5. Exact level match at v1
    let res_at_v1 = calculate_subsample_offset(100, 5.0, 200, 10.0, 10.0);
    let (alpha_v1, offset_v1) = res_at_v1.unwrap();
    assert_eq!(alpha_v1, 1.0);
    assert_eq!(offset_v1, 100.0);

    // 6. Denominator near threshold (1e-12 boundary)
    let res_just_above = calculate_subsample_offset(100, 1.0, 200, 1.0 + 1e-11, 1.0 + 5e-12);
    assert!(res_just_above.is_some());
    let (alpha_ja, _) = res_just_above.unwrap();
    assert!((alpha_ja - 0.5).abs() < 1e-5);

    let res_just_below = calculate_subsample_offset(100, 1.0, 200, 1.0 + 1e-13, 1.0 + 5e-14);
    assert!(res_just_below.is_some());

    // 7. Reject NaN and Infinities
    assert!(calculate_subsample_offset(100, f64::NAN, 200, 1.0, 0.5).is_none());
    assert!(calculate_subsample_offset(100, 0.0, 200, f64::INFINITY, 0.5).is_none());
    assert!(calculate_subsample_offset(100, 0.0, 200, 1.0, f64::NEG_INFINITY).is_none());

    // 8. Reject backwards timestamps (t1 < t0)
    assert!(calculate_subsample_offset(200, 0.0, 100, 1.0, 0.5).is_none());

    // 9. Identical timestamps (t1 == t0)
    let (alpha_same, offset_same) = calculate_subsample_offset(100, 0.0, 100, 10.0, 5.0).unwrap();
    assert_eq!(alpha_same, 0.5);
    assert_eq!(offset_same, 0.0);
}

// ============================================================================
// 8. SYNC GROUP RAPID SCOPE CHURN CONCURRENCY
// ============================================================================

#[test]
fn test_sync_group_rapid_scope_churn() {
    let coordinator = Arc::new(SyncGroupCoordinator::new());
    let num_threads = 16;
    let churn_iterations = 1_000;

    let mut handles = Vec::with_capacity(num_threads);

    for t_idx in 0..num_threads {
        let coord = Arc::clone(&coordinator);
        let handle = thread::spawn(move || {
            for i in 0..churn_iterations {
                let scope_id = ScopeId((i % 20) as u32);
                let group_id = SyncGroupId(((t_idx + i) % 5) as u32 + 1);

                coord.register_scope(scope_id, group_id);
                let _ = coord.get_scope_group(scope_id);
                let _ = coord.get_group_scopes(group_id);

                if i % 3 == 0 {
                    coord.unregister_scope(scope_id);
                }
            }
        });
        handles.push(handle);
    }

    for (i, h) in handles.into_iter().enumerate() {
        h.join()
            .unwrap_or_else(|_| panic!("Thread {i} panicked during scope churn"));
    }

    // Verify consistency: all scopes reported in a group actually belong to that group
    for g in 1..=5 {
        let group_id = SyncGroupId(g);
        let scopes = coordinator.get_group_scopes(group_id);
        for s in scopes {
            assert_eq!(coordinator.get_scope_group(s), Some(group_id));
        }
    }
}

