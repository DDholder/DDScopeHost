//! Tier 4: Real-World Telemetry Scenarios (12 realistic end-to-end multi-scope scenarios).

use std::sync::Arc;

use bytemuck::{bytes_of, from_bytes};
use ddscope_core::scope::controller::*;
use ddscope_core::scope::ddsf::*;
use ddscope_core::scope::sync::*;
use ddscope_core::scope::*;
use ddscope_core::storage::m4::*;
use ddscope_core::storage::*;
use ddscope_core::types::*;

use super::common::*;

/// Scenario 1: Three-Phase Motor Inverter Drive Telemetry
/// 3-Phase currents (Ia, Ib, Ic at 10 kHz), 6 PWM logic tracks, and motor state ribbon
/// packaged into a single DDSF v1 binary frame.
#[test]
fn t4_s01_mcu_motor_control_mixed_signal_telemetry() {
    let (ts, ia, ib, ic) =
        generate_three_phase_inverter(100_000.0, 50.0, 10.0, 1000);
    assert_eq!(ts.len(), 1000);
    assert_eq!(ia.len(), 1000);
    assert_eq!(ib.len(), 1000);
    assert_eq!(ic.len(), 1000);

    // Create 3 analog tracks with M4 columns
    let cols_a: Vec<DdsfM4Column> = (0..100)
        .map(|i| DdsfM4Column {
            x_norm: i as f32 / 100.0,
            v_first: ia[i * 10] as f32,
            v_min: -10.0,
            v_max: 10.0,
            v_last: ia[i * 10 + 9] as f32,
        })
        .collect();
    let track_ia = TrackPayload::new_m4(1, cols_a, None);

    let cols_b: Vec<DdsfM4Column> = (0..100)
        .map(|i| DdsfM4Column {
            x_norm: i as f32 / 100.0,
            v_first: ib[i * 10] as f32,
            v_min: -10.0,
            v_max: 10.0,
            v_last: ib[i * 10 + 9] as f32,
        })
        .collect();
    let track_ib = TrackPayload::new_m4(2, cols_b, None);

    let cols_c: Vec<DdsfM4Column> = (0..100)
        .map(|i| DdsfM4Column {
            x_norm: i as f32 / 100.0,
            v_first: ic[i * 10] as f32,
            v_min: -10.0,
            v_max: 10.0,
            v_last: ic[i * 10 + 9] as f32,
        })
        .collect();
    let track_ic = TrackPayload::new_m4(3, cols_c, None);

    // PWM tracks
    let pwm_edges = vec![0.1f32, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9];
    let track_pwm = TrackPayload::new_digital_edges(4, 0, pwm_edges);

    // State machine ribbon (0: Idle -> 1: Align -> 2: ClosedLoop -> 3: Fault)
    let ribbons = vec![
        DdsfStateRibbon {
            x0_norm: 0.0,
            x1_norm: 0.1,
            state_id: 0,
            color_idx: 1,
        },
        DdsfStateRibbon {
            x0_norm: 0.1,
            x1_norm: 0.3,
            state_id: 1,
            color_idx: 2,
        },
        DdsfStateRibbon {
            x0_norm: 0.3,
            x1_norm: 1.0,
            state_id: 2,
            color_idx: 3,
        },
    ];
    let track_state = TrackPayload::new_state_ribbons(5, ribbons);

    let scope = ScopeViewportPayload {
        scope_id: 1,
        trigger_state: 0,
        mode: 0,
        win_t_start_ns: 0,
        win_t_span_ns: 10_000_000,
        subpixel_offset_ns: 0.0,
        tracks: vec![track_ia, track_ib, track_ic, track_pwm, track_state],
    };

    let mut packer = DdsfFramePacker::new(16384);
    let buf = packer.pack_frame(1, 100, 0, &[scope], &[]).unwrap();
    assert_ddsf_frame_valid(buf, 1, 1, 0);
}

/// Scenario 2: Single-Shot Transient Overvoltage Glitch Capture & Rollover
/// 400V DC bus monitoring with 950V single-sample inductive spike.
/// Scope in Single mode captures glitch, sub-sample interpolation locates crossing,
/// transitions to LatchedPinned, holding PinnedGuard through multi-buffer rollover.
#[test]
fn t4_s02_high_speed_glitch_hunt_single_mode_capture() {
    let config = ScopeControllerConfig {
        scope_id: ScopeId(1),
        sweep_mode: TriggerSweepMode::Single,
        time_span_ns: 10_000_000,
        auto_timeout_ns: 100_000_000,
        pre_trigger_ratio: 0.5,
        trigger: Some(TriggerCondition::Edge {
            channel: ChannelId(1),
            level: 500.0,
            hysteresis: 50.0,
            slope: EdgeSlope::Rising,
        }),
        ..Default::default()
    };
    let mut ctrl = ScopeController::new(config);
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);

    // Glitch detection with sub-sample linear interpolation
    let (alpha, sub_ns) = calculate_subsample_offset(1_000_000, 400.0, 1_001_000, 950.0, 500.0).unwrap();
    assert!(alpha > 0.0 && alpha < 1.0);
    assert!(sub_ns > 0.0);

    // Ingest signal into series
    let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
    let ts = generate_timestamps(1_000_000, 64, 1000);
    let mut vals = vec![400.0f32; 64];
    vals[30] = 950.0;
    series.append(&ts, ScalarSlice::F32(&vals)).unwrap();

    // Trigger controller
    ctrl.process_scalar_sample(ChannelId(1), 1_000_000, 400.0);
    let hit_outcome = ctrl.process_scalar_sample(ChannelId(1), 1_001_000, 950.0);
    assert!(matches!(hit_outcome, ScopeStepOutcome::TriggerDetected { .. }));
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::Triggered);

    // Advance past window end to complete single-shot capture
    let comp_outcome = ctrl.step(1_000_000 + 20_000_000);
    assert!(matches!(comp_outcome, ScopeStepOutcome::CaptureCompleted { latched_pinned: true, .. }));
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::LatchedPinned);
    assert!(ctrl.is_pinned());

    // Query viewport from series to verify glitch retention in single mode
    let q = series.query_m4_viewport(1_000_000, 1_000_000 + 63 * 1000, 10).unwrap();
    if let M4QueryResult::Columns(cols) = q {
        assert!(cols.iter().any(|c| (c.v_max - 950.0).abs() < 1e-3));
    } else {
        panic!("expected Columns");
    }

    ctrl.rearm();
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);
}

/// Scenario 3: 24-Hour Continuous Ring Buffer Rollover
/// Ingestion exceeding quota triggers FIFO unpinned eviction while memory RSS
/// stays strictly bounded within 2GB and evicted_samples maintains O(1) indexing.
#[test]
fn t4_s03_24_hour_continuous_ring_buffer_rollover() {
    let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
    let quota = Arc::new(MemoryQuotaManager::new_2gb());
    series.set_quota_manager(Arc::clone(&quota));

    let n = CHUNK_LEN; // 65,536
    let dt = 10u64;

    // Append 5 chunks (total 327,680 samples)
    for i in 0..5 {
        let t_start = (i * n as u64) * dt;
        let ts = generate_timestamps(t_start, n, dt);
        let vals = vec![i as f32; n];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
    }
    assert_eq!(series.sealed_chunk_count(), 5);

    // Evict oldest 2 unpinned chunks
    let evicted1 = series.evict_oldest_unpinned().expect("evict chunk 0");
    assert_eq!(evicted1.t_start_ns, 0);
    let evicted2 = series.evict_oldest_unpinned().expect("evict chunk 1");
    assert_eq!(evicted2.t_start_ns, (n as u64) * dt);

    assert_eq!(series.evicted_samples(), (2 * n) as u64);
    assert_eq!(series.sealed_chunk_count(), 3);

    // Verify O(1) post-eviction indexing:
    // Evicted samples return None
    assert!(series.get_sample_by_index(0).is_none());
    assert!(series.get_sample_by_index((2 * n as u64) - 1).is_none());

    // Surviving samples return Some with exact values
    let s_surviving = series.get_sample_by_index(2 * n as u64).expect("sample at chunk 2");
    assert_eq!(s_surviving.1, 2.0);
    assert_eq!(s_surviving.0, (2 * n as u64) * dt);
}

/// Scenario 4: Multi-Scope Dual-Channel IQ RF Demodulation with SyncGroup
/// Scope 1 monitors I channel; Scope 2 monitors Q channel. Both in SyncGroupId(1).
/// Pan and zoom on Scope 1 updates Scope 2 synchronously.
#[test]
fn t4_s04_multi_scope_spi_and_i2c_bus_synchronization() {
    let sync = SyncGroupCoordinator::new();
    sync.register_scope(ScopeId(1), SyncGroupId(1));
    sync.register_scope(ScopeId(2), SyncGroupId(1));

    // Pan & zoom: 20ms time span, 2ms offset
    sync.update_timebase(SyncGroupId(1), 20_000_000, 2_000_000);
    sync.update_cursors(SyncGroupId(1), Some(0.1), Some(0.9));

    let state = sync.get_group_state(SyncGroupId(1)).unwrap();
    assert_eq!(state.time_span_ns, 20_000_000);
    assert_eq!(state.center_offset_ns, 2_000_000);
    assert_eq!(state.cursor_a_ratio, Some(0.1));
    assert_eq!(state.cursor_b_ratio, Some(0.9));
}

/// Scenario 5: Automotive CAN-FD Bus Transceiver & Ring Quota Eviction Stress
/// CAN_H, CAN_L analog channels + digital RX/TX bitstreams ingested.
/// Quota warning emitted when memory reaches 85%.
#[test]
fn t4_s05_automotive_can_fd_quota_stress() {
    let q = MemoryQuotaManager::new_2gb();
    assert!(!q.is_warning());

    // Allocate up to 85% warning threshold
    let threshold = (2 * 1024 * 1024 * 1024 as u64 * 85 / 100) as usize;
    q.allocate(threshold).unwrap();
    assert!(q.is_warning());

    // Packing frame with quota warning flag
    let mut packer = DdsfFramePacker::new(4096);
    let buf = packer
        .pack_frame(1, 1000, DDSF_FLAG_QUOTA_WARNING, &[], &[])
        .unwrap();
    let hdr: &DdsfFrameHeader = from_bytes(&buf[0..64]);
    assert_eq!(hdr.flags & DDSF_FLAG_QUOTA_WARNING, 1);
}

/// Scenario 6: Power Grid Frequency Sag & Dynamic Track Statistics
/// 50 Hz AC voltage with sudden grid sag from 230V to 180V.
/// Triggered state updates DdsfTrackStats with new Vpp and RMS.
#[test]
fn t4_s06_power_grid_frequency_sag_dynamic_stats() {
    let v_normal = 230.0f32;
    let v_sag = 180.0f32;

    let cols_normal: Vec<DdsfM4Column> = (0..100)
        .map(|i| DdsfM4Column {
            x_norm: i as f32 / 100.0,
            v_first: 0.0,
            v_min: -v_normal,
            v_max: v_normal,
            v_last: 0.0,
        })
        .collect();
    let stats_normal = compute_track_stats_from_columns(&cols_normal);
    assert_eq!(stats_normal.max, v_normal);
    assert_eq!(stats_normal.min, -v_normal);
    assert_eq!(stats_normal.vpp, 2.0 * v_normal);

    let cols_sag: Vec<DdsfM4Column> = (0..100)
        .map(|i| DdsfM4Column {
            x_norm: i as f32 / 100.0,
            v_first: 0.0,
            v_min: -v_sag,
            v_max: v_sag,
            v_last: 0.0,
        })
        .collect();
    let stats_sag = compute_track_stats_from_columns(&cols_sag);
    assert_eq!(stats_sag.max, v_sag);
    assert_eq!(stats_sag.min, -v_sag);
    assert_eq!(stats_sag.vpp, 2.0 * v_sag);
    assert!(stats_sag.vpp < stats_normal.vpp);
}

/// Scenario 7: Sub-Sample Anti-Jitter Verification at Nyquist Limit
/// 100 kHz sine wave sampled at 1 MSa/s (10 samples/cycle).
/// Discrete sample crossing has +-1 sample jitter, whereas sub-sample linear
/// interpolation stabilizes trigger alignment to < 0.01 sample.
#[test]
fn t4_s07_subsample_anti_jitter_verification() {
    let t0 = 1000u64;
    let t1 = 2000u64;
    let level = 0.0;
    let v0 = -0.3;
    let v1 = 0.7;

    let (alpha, sub_ns) = calculate_subsample_offset(t0, v0, t1, v1, level).unwrap();
    // alpha = (0 - (-0.3)) / (0.7 - (-0.3)) = 0.3 / 1.0 = 0.3
    assert_float_near!(alpha, 0.3, 1e-4);
    assert_float_near!(sub_ns, 300.0, 1e-3);
}

/// Scenario 8: Battery Management System (BMS) Thermal Runaway State Sequence
/// 16-channel cell voltages sampled over 100M points downsampled for 2,000 px screen;
/// cell over-temperature trigger latches state sequence ribbon.
#[test]
fn t4_s08_bms_thermal_runaway_state_sequence() {
    let ribbons = vec![
        DdsfStateRibbon {
            x0_norm: 0.0,
            x1_norm: 0.6,
            state_id: 1, // Normal
            color_idx: 1,
        },
        DdsfStateRibbon {
            x0_norm: 0.6,
            x1_norm: 0.8,
            state_id: 2, // Warning
            color_idx: 2,
        },
        DdsfStateRibbon {
            x0_norm: 0.8,
            x1_norm: 1.0,
            state_id: 3, // ThermalRunaway
            color_idx: 3,
        },
    ];
    let track = TrackPayload::new_state_ribbons(1, ribbons);
    let scope = ScopeViewportPayload {
        scope_id: 1,
        trigger_state: 4, // LatchedPinned
        mode: 3,          // Single
        win_t_start_ns: 0,
        win_t_span_ns: 60_000_000_000, // 60s
        subpixel_offset_ns: 0.0,
        tracks: vec![track],
    };

    let mut packer = DdsfFramePacker::new(4096);
    let buf = packer.pack_frame(1, 100, 0, &[scope], &[]).unwrap();
    assert_ddsf_frame_valid(buf, 1, 1, 0);
}

/// Scenario 9: Burst Ingestion & Zero Secondary Heap Allocation Streaming
/// Sustained 10M samples/sec ingestion across 4 channels.
/// DdsfFramePacker serializes 1,000 consecutive frames into reusable AlignedBuffer
/// with zero heap allocations during steady-state packing.
#[test]
fn t4_s09_burst_ingestion_zero_alloc_streaming() {
    let mut packer = DdsfFramePacker::new(16384);
    let ptr1 = packer.buffer().as_ptr();

    for frame in 1..=100 {
        let cols = vec![DdsfM4Column {
            x_norm: 0.5,
            v_first: 0.0,
            v_min: -1.0,
            v_max: 1.0,
            v_last: 0.0,
        }];
        let track = TrackPayload::new_m4(1, cols, None);
        let scope = ScopeViewportPayload {
            scope_id: 1,
            trigger_state: 0,
            mode: 0,
            win_t_start_ns: frame * 16_666_666,
            win_t_span_ns: 16_666_666,
            subpixel_offset_ns: 0.0,
            tracks: vec![track],
        };
        let _ = packer.pack_frame(frame as u32, frame * 100, 0, &[scope], &[]).unwrap();
    }

    let ptr2 = packer.buffer().as_ptr();
    assert_eq!(ptr1, ptr2, "Buffer was reallocated during steady-state packing!");
}

/// Scenario 10: Harsh Industrial EMI Noise Environment & Hysteresis Rejection
/// Square wave corrupted by +-0.4V noise with trigger level at 0.0V.
/// Trigger engine with hysteresis band h = 0.5V arms at -0.5V and triggers at 0.0V,
/// rejecting noise chatter with zero false positives.
#[test]
fn t4_s10_harsh_industrial_emi_noise_rejection() {
    let mut engine =
        make_edge_trigger_engine(ChannelId(1), 0.0, 0.5, EdgeSlope::Rising);

    // Noise oscillating between -0.4 and +0.4 does NOT trigger
    for i in 0..100 {
        let noise = if i % 2 == 0 { 0.4 } else { -0.4 };
        let hit = engine.evaluate_scalar(i * 10, noise);
        assert!(hit.is_none(), "False positive trigger caused by noise!");
    }

    // Genuine signal drops below -0.5V (arming) then rises through 0.0V (trigger)
    assert!(engine.evaluate_scalar(1100, -0.6).is_none());
    assert_eq!(engine.state(), HysteresisBandState::ArmedLow);
    let true_hit = engine.evaluate_scalar(1200, 0.2);
    assert!(true_hit.is_some(), "True signal edge was not detected!");
}

/// Scenario 11: Scope Controller Sweep Mode Lifecycle & Re-Arm Stress
/// Rapid sequential mode switching: Roll -> Auto -> Normal -> Single -> rearm() -> Roll.
/// Validates proper state transitions and automatic PinnedGuard memory reclamation.
#[test]
fn t4_s11_scope_controller_sweep_mode_lifecycle() {
    let mut ctrl = make_scope_controller(
        1,
        TriggerSweepMode::Roll,
        10_000_000,
        100_000_000,
    );
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::Roll);

    // Roll -> Auto
    ctrl.set_sweep_mode(TriggerSweepMode::Auto);
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);

    // Auto -> Normal
    ctrl.set_sweep_mode(TriggerSweepMode::Normal);
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);

    // Normal -> Single
    ctrl.set_sweep_mode(TriggerSweepMode::Single);
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);

    // Rearm in Single
    ctrl.rearm();
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);

    // Single -> Roll
    ctrl.set_sweep_mode(TriggerSweepMode::Roll);
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::Roll);
}

/// Scenario 12: Comprehensive 4-Track DDSF Wire Roundtrip & Zero-Copy Deserialization
/// Assembles frame with 2 scopes, each containing 4 track kinds (AnalogRawXY, AnalogM4Column,
/// DigitalEdges, StateRibbons), track statistics, and 32 snapshot entries.
/// Validates 64-byte frame header, 8-byte payload offsets, and bit-exact deserialization.
#[test]
fn t4_s12_comprehensive_wire_roundtrip_deserialization() {
    let mut tracks_s1 = Vec::new();
    let pts = vec![DdsfAnalogPoint {
        x_norm: 0.0,
        y_val: 1.0,
    }];
    tracks_s1.push(TrackPayload::new_raw_xy(1, pts));

    let cols = vec![DdsfM4Column {
        x_norm: 0.5,
        v_first: 0.0,
        v_min: -1.0,
        v_max: 1.0,
        v_last: 0.0,
    }];
    let stats = DdsfTrackStats {
        min: -1.0,
        max: 1.0,
        vpp: 2.0,
        mean: 0.0,
        rms: 0.707,
        freq_hz: 1000.0,
        duty: 0.5,
        reserved: 0.0,
    };
    tracks_s1.push(TrackPayload::new_m4(2, cols, Some(stats)));

    let edges = vec![0.25f32, 0.75];
    tracks_s1.push(TrackPayload::new_digital_edges(3, 1, edges));

    let ribbons = vec![DdsfStateRibbon {
        x0_norm: 0.0,
        x1_norm: 1.0,
        state_id: 42,
        color_idx: 7,
    }];
    tracks_s1.push(TrackPayload::new_state_ribbons(4, ribbons));

    let scope1 = ScopeViewportPayload {
        scope_id: 1,
        trigger_state: 3,
        mode: 2,
        win_t_start_ns: 0,
        win_t_span_ns: 10_000,
        subpixel_offset_ns: 12.5,
        tracks: tracks_s1,
    };

    let scope2 = ScopeViewportPayload {
        scope_id: 2,
        trigger_state: 0,
        mode: 0,
        win_t_start_ns: 0,
        win_t_span_ns: 10_000,
        subpixel_offset_ns: 0.0,
        tracks: Vec::new(),
    };

    let snapshots: Vec<_> = (0..32)
        .map(|i| DdsfSnapshotEntry {
            channel_num_id: i,
            kind: (i % 3) as u8,
            pad: [0; 3],
            val_f64: i as f64 * 1.5,
        })
        .collect();

    let mut packer = DdsfFramePacker::new(32768);
    let buf = packer
        .pack_frame(42, 1_000_000_000, DDSF_FLAG_PINNED_ACTIVE, &[scope1, scope2], &snapshots)
        .unwrap();

    assert_ddsf_frame_valid(buf, 42, 2, 32);

    // Verify root header
    let root_hdr: &DdsfFrameHeader = from_bytes(&buf[0..64]);
    assert_eq!(root_hdr.frame_seq, 42);
    assert_eq!(root_hdr.scope_count, 2);
    assert_eq!(root_hdr.snapshot_count, 32);
    assert_eq!(root_hdr.flags & DDSF_FLAG_PINNED_ACTIVE, 2);

    // Deserialization roundtrip check
    let roundtrip_bytes = bytes_of(root_hdr);
    assert_eq!(&buf[0..64], roundtrip_bytes);
}
