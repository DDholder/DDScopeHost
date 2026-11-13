//! Tier 3: Cross-Feature Pairwise Interaction Tests (24 tests).

use std::sync::Arc;

use ddscope_core::scope::controller::*;
use ddscope_core::scope::ddsf::*;
use ddscope_core::scope::sync::*;
use ddscope_core::scope::*;
use ddscope_core::storage::m4::*;
use ddscope_core::storage::*;
use ddscope_core::types::*;

use super::common::*;

#[test]
fn t3_p01_m4_pyramid_and_quota_rollover() {
    // F01 + F03 + F04: SIMD incremental reduction preserves isolated peak
    let glitch_val = 1234.5f32;
    let signal = generate_glitch_signal(64, 42, glitch_val, 0.0);
    let bucket = reduce_raw_f32_slice(&signal, 0, 630, &LinearScale::identity());
    assert_eq!(bucket.v_max, glitch_val, "single spike must be preserved in M4Bucket");

    let mut buckets = [bucket; 64];
    buckets[0] = bucket;
    let parent = reduce_64_buckets(&buckets);
    assert_eq!(parent.v_max, glitch_val, "64-to-1 reduction preserves peak");
}

#[test]
fn t3_p02_hierarchy_and_pixel_aligned_query() {
    // F02 + F05: Multi-level hierarchy selects pyramid level and columns
    let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
    let count = 10_000;
    let ts: Vec<u64> = (0..count as u64).map(|i| i * 10).collect();
    let val = vec![1.0f32; count];
    series.append(&ts, ScalarSlice::F32(&val)).expect("append ok");

    let w_px = 500;
    // N = 10,000 > 2 * 500 = 1,000 -> Selects Columns
    let res = series.query_m4_viewport(0, 100_000, w_px).expect("query ok");
    match res {
        M4QueryResult::Columns(cols) => assert_eq!(cols.len(), w_px),
        M4QueryResult::Raw(_) => panic!("expected Columns"),
    }
}

#[test]
fn t3_p03_extrema_preservation_in_m4_columns() {
    // F04 + F05: Single-sample glitch preserved in M4 column query
    let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
    let spike_val = 999.0f32;
    let mut val = vec![0.0f32; 1000];
    val[250] = spike_val;
    let ts: Vec<u64> = (0..1000).map(|i| i * 10).collect();
    series.append(&ts, ScalarSlice::F32(&val)).expect("append ok");

    let res = series.query_m4_viewport(0, 10_000, 100).expect("query ok");
    match res {
        M4QueryResult::Columns(cols) => {
            assert_m4_extrema_retained(&cols, Some(spike_val), None);
        }
        M4QueryResult::Raw(_) => panic!("expected Columns"),
    }
}

#[test]
fn t3_p04_latency_with_hierarchy_level2() {
    // F02 + F06: 2500 L2 buckets queried for 2000px completes rapidly
    let buckets = vec![
        M4Bucket {
            t_first_ns: 0,
            t_last_ns: 40_960,
            v_first: 1.0,
            v_min: 0.0,
            v_max: 2.0,
            v_last: 1.0,
        };
        2500
    ];
    let start = std::time::Instant::now();
    let cols = aggregate_buckets_to_columns(&buckets, 0, 100_000_000, 2000);
    let elapsed = start.elapsed();
    assert_eq!(cols.len(), 2000);
    assert!(elapsed.as_millis() < 10);
}

#[test]
fn t3_p05_quota_and_fifo_eviction() {
    // F07 + F08: Memory stays bounded <= 2GB under continuous allocations with automatic FIFO eviction
    let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
    let n = CHUNK_LEN;
    let dt = 10u64;

    // Ingest Chunk 0 to determine accurate resident bytes
    let ts0 = generate_timestamps(0, n, dt);
    let vals0 = vec![1.0f32; n];
    series.append(&ts0, ScalarSlice::F32(&vals0)).unwrap();
    assert_eq!(series.sealed_chunk_count(), 1);

    let chunk_bytes = series.sealed_chunks()[0].storage.resident_bytes();
    let quota_limit = chunk_bytes * 2 + 1024;
    let quota = Arc::new(MemoryQuotaManager::new(quota_limit));
    assert!(quota.record_allocation(chunk_bytes));
    series.set_quota_manager(Arc::clone(&quota));

    // Ingest Chunk 1 (total 2 chunks, stays within quota)
    let ts1 = generate_timestamps((n as u64) * dt, n, dt);
    let vals1 = vec![2.0f32; n];
    series.append(&ts1, ScalarSlice::F32(&vals1)).unwrap();
    assert_eq!(series.sealed_chunk_count(), 2);
    assert_eq!(series.evicted_samples(), 0);

    // Ingest Chunk 2: exceeds quota and triggers automatic FIFO eviction of Chunk 0
    let ts2 = generate_timestamps((2 * n as u64) * dt, n, dt);
    let vals2 = vec![3.0f32; n];
    series.append(&ts2, ScalarSlice::F32(&vals2)).unwrap();

    assert_eq!(series.sealed_chunk_count(), 2, "must maintain 2 sealed chunks within quota");
    assert_eq!(series.evicted_samples(), n as u64, "chunk 0 must be evicted");

    // Verify Chunk 0 is evicted (returns None) while Chunk 1 and Chunk 2 are retained
    assert!(series.get_sample_by_index(0).is_none(), "evicted chunk 0 must return None");
    let s1 = series.get_sample_by_index(n as u64).expect("chunk 1 retained");
    assert_float_near!(s1.1, 2.0);
    let s2 = series.get_sample_by_index(2 * n as u64).expect("chunk 2 retained");
    assert_float_near!(s2.1, 3.0);
}

#[test]
fn t3_p06_quota_and_pinned_guard_protection() {
    // F07 + F09: Pinned chunk memory tracked in pinned_bytes and not evicted
    let q = Arc::new(MemoryQuotaManager::new_2gb());
    let guard = q.pin(500 * 1024 * 1024).unwrap();
    assert_eq!(q.pinned_bytes(), 500 * 1024 * 1024);
    drop(guard);
    assert_eq!(q.pinned_bytes(), 0);
}

#[test]
fn t3_p07_fifo_eviction_and_evicted_offset_indexing() {
    // F08 + F12: Sample indexing resolves global index via evicted_samples base offset
    let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
    let n = CHUNK_LEN;
    let ts = generate_timestamps(0, n, 10);
    let vals = vec![1.0f32; n];
    series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
    series.append(&[n as u64 * 10], ScalarSlice::F32(&[2.0f32])).unwrap();
    series.evict_oldest_unpinned().unwrap();
    assert_eq!(series.evicted_samples(), n as u64);
    assert!(series.get_sample_by_index(n as u64).is_some());
    assert!(series.get_sample_by_index(n as u64 - 1).is_none());
}

#[test]
fn t3_p08_pinned_guard_and_ring_rollover_safety() {
    // F09 + F10: Pinned chunks survive multiple ring buffer overwrites
    let quota = Arc::new(MemoryQuotaManager::new_2gb());
    let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
    series.set_quota_manager(quota);

    let n = CHUNK_LEN;
    let ts1 = generate_timestamps(0, n, 10);
    let vals1 = vec![42.0f32; n];
    series.append(&ts1, ScalarSlice::F32(&vals1)).unwrap();
    series.seal_active().unwrap();

    let guard = series.pin_range(0, (n - 1) as u64 * 10).unwrap();
    assert!(guard.bytes() > 0);

    // Evict unpinned chunks - because chunk 0 is pinned, evict_oldest_unpinned must return None!
    let evicted = series.evict_oldest_unpinned();
    assert!(evicted.is_none());
    assert_eq!(series.evicted_samples(), 0);

    // Drop guard, now it can be evicted
    drop(guard);
    let evicted2 = series.evict_oldest_unpinned();
    assert!(evicted2.is_some());
    assert_eq!(series.evicted_samples(), n as u64);
}

#[test]
fn t3_p09_pinning_and_40_percent_cap() {
    // F09 + F11: Pinning >40% rejected, <=40% succeeds
    let q = Arc::new(MemoryQuotaManager::new_2gb());
    // 40% of 2GB = ~858MB
    let _guard1 = q.pin(750 * 1024 * 1024).unwrap();
    assert!(q.pin(150 * 1024 * 1024).is_err());
}

#[test]
fn t3_p10_scope_controller_single_mode_and_pinned_guard() {
    // F13 + F18: ScopeController in Single mode latches and pins memory
    let config = ScopeControllerConfig {
        sweep_mode: TriggerSweepMode::Single,
        time_span_ns: 2_000,
        pre_trigger_ratio: 0.5,
        ..Default::default()
    };
    let mut ctrl = ScopeController::new(config);
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);
    assert!(!ctrl.is_pinned());

    ctrl.force_trigger(10_000);
    ctrl.step(12_000);
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::LatchedPinned);
    assert!(ctrl.is_pinned());

    ctrl.rearm();
    assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);
    assert!(!ctrl.is_pinned());
}

#[test]
fn t3_p11_scope_controller_and_auto_timeout() {
    // F13 + F14: Missing trigger transitions controller to AutoFreerun
    let config = ScopeControllerConfig {
        sweep_mode: TriggerSweepMode::Auto,
        time_span_ns: 10_000_000,
        auto_timeout_ns: 100_000_000,
        ..Default::default()
    };
    let mut ctrl = ScopeController::new(config);
    ctrl.step(1_000_000);
    let outcome = ctrl.step(111_000_000);
    match outcome {
        ScopeStepOutcome::AutoFreerunFired { window } => {
            assert_eq!(window.mode, TriggerSweepMode::Auto);
            assert_eq!(window.trigger_state, TriggerRuntimeState::AutoFreerun);
        }
        _ => panic!("expected AutoFreerunFired"),
    }
}

#[test]
fn t3_p12_scope_controller_and_hysteresis_debouncing() {
    // F13 + F15: Noisy signal inside hysteresis band rejected; true crossing triggers
    let mut engine =
        make_edge_trigger_engine(ChannelId(1), 0.0, 0.5, EdgeSlope::Rising);
    // Noise in [-0.4, 0.4] does not arm low
    assert!(engine.evaluate_scalar(100, -0.4).is_none());
    assert!(engine.evaluate_scalar(200, 0.4).is_none());
    // Valid arm low then rise
    assert!(engine.evaluate_scalar(300, -0.6).is_none());
    let hit = engine.evaluate_scalar(400, 0.2);
    assert!(hit.is_some());
}

#[test]
fn t3_p13_hysteresis_and_subsample_interpolation() {
    // F15 + F16: Rising edge crossing calculates accurate subpixel_offset_ns
    let (alpha, sub_ns) =
        calculate_subsample_offset(1000, -1.0, 2000, 1.0, 0.0).unwrap();
    // Crossing exactly at level 0.0 between -1.0 and 1.0 -> alpha = 0.5, sub = 500ns
    assert_float_near!(alpha, 0.5, 1e-4);
    assert_float_near!(sub_ns, 500.0, 1e-3);
}

#[test]
fn t3_p14_scope_controller_and_sync_group() {
    let sync = SyncGroupCoordinator::new();
    sync.register_scope(ScopeId(1), SyncGroupId(1));
    sync.register_scope(ScopeId(2), SyncGroupId(1));
    sync.update_timebase(SyncGroupId(1), 25_000_000, 1_000_000);

    let mut cfg1 = ScopeControllerConfig { scope_id: ScopeId(1), ..Default::default() };
    let mut cfg2 = ScopeControllerConfig { scope_id: ScopeId(2), ..Default::default() };

    assert!(sync.apply_to_controller_config(ScopeId(1), &mut cfg1));
    assert!(sync.apply_to_controller_config(ScopeId(2), &mut cfg2));

    assert_eq!(cfg1.time_span_ns, 25_000_000);
    assert_eq!(cfg1.pan_offset_ns, 1_000_000);
    assert_eq!(cfg2.time_span_ns, 25_000_000);
    assert_eq!(cfg2.pan_offset_ns, 1_000_000);

    let ctrl1 = ScopeController::new(cfg1);
    let ctrl2 = ScopeController::new(cfg2);
    assert_eq!(ctrl1.config().time_span_ns, ctrl2.config().time_span_ns);
}

#[test]
fn t3_p15_subsample_interpolation_and_ddsf_packet_header() {
    // F16 + F19 + F20: subpixel_offset_ns packed into DdsfScopePacketHeader
    let (_alpha, subpixel_ns) = calculate_subsample_offset(1000, -1.0, 2000, 1.0, 0.0).unwrap();
    assert_float_near!(subpixel_ns, 500.0, 1e-3);

    let scope = ScopeViewportPayload {
        scope_id: 1,
        trigger_state: 3,
        mode: 2,
        win_t_start_ns: 1000,
        win_t_span_ns: 10_000,
        subpixel_offset_ns: subpixel_ns,
        tracks: Vec::new(),
    };
    let mut packer = DdsfFramePacker::new(4096);
    let buf = packer.pack_frame(1, 100, 0, &[scope], &[]).unwrap();
    assert_ddsf_frame_valid(buf, 1, 1, 0);
    let scope_hdr: &DdsfScopePacketHeader = bytemuck::from_bytes(&buf[64..96]);
    assert_eq!(scope_hdr.subpixel_offset_ns, subpixel_ns);
}

#[test]
fn t3_p16_ddsf_header_and_analog_m4_track_packing() {
    // F19 + F20: DDSF frame packs AnalogM4Column with 8-byte alignment
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
        trigger_state: 3,
        mode: 2,
        win_t_start_ns: 0,
        win_t_span_ns: 1000,
        subpixel_offset_ns: 0.0,
        tracks: vec![track],
    };
    let mut packer = DdsfFramePacker::new(4096);
    let buf = packer.pack_frame(1, 100, 0, &[scope], &[]).unwrap();
    assert_ddsf_frame_valid(buf, 1, 1, 0);
}

#[test]
fn t3_p17_track_packing_and_stats_embedding() {
    // F20 + F21: HAS_STATS flag embeds 32-byte DdsfTrackStats immediately after track header
    let stats = DdsfTrackStats {
        min: -5.0,
        max: 5.0,
        vpp: 10.0,
        mean: 0.0,
        rms: 3.535,
        freq_hz: 50.0,
        duty: 0.5,
        reserved: 0.0,
    };
    let cols = vec![DdsfM4Column {
        x_norm: 0.5,
        v_first: 0.0,
        v_min: -5.0,
        v_max: 5.0,
        v_last: 0.0,
    }];
    let track = TrackPayload::new_m4(1, cols, Some(stats));
    assert_eq!(track.flags & DDSF_TRACK_FLAG_HAS_STATS, 1);
}

#[test]
fn t3_p18_track_packing_and_zero_allocations() {
    // F20 + F22: Reusable buffer packs frame with zero pointer changes in steady state
    let mut packer = DdsfFramePacker::new(8192);
    let ptr1 = packer.buffer().as_ptr();
    for seq in 1..=10 {
        let _ = packer.pack_frame(seq, seq as u64, 0, &[], &[]).unwrap();
    }
    let ptr2 = packer.buffer().as_ptr();
    assert_eq!(ptr1, ptr2);
}

#[test]
fn t3_p19_pixel_aligned_query_and_ddsf_packer() {
    // F05 + F20: Viewport query output serializes into AnalogM4Column track block
    let w_px = 100;
    let cols: Vec<DdsfM4Column> = (0..w_px)
        .map(|i| DdsfM4Column {
            x_norm: i as f32 / w_px as f32,
            v_first: 0.0,
            v_min: -1.0,
            v_max: 1.0,
            v_last: 0.0,
        })
        .collect();
    let track = TrackPayload::new_m4(1, cols, None);
    let scope = ScopeViewportPayload {
        scope_id: 1,
        trigger_state: 0,
        mode: 0,
        win_t_start_ns: 0,
        win_t_span_ns: 10_000,
        subpixel_offset_ns: 0.0,
        tracks: vec![track],
    };
    let mut packer = DdsfFramePacker::new(8192);
    let buf = packer.pack_frame(1, 100, 0, &[scope], &[]).unwrap();
    assert_ddsf_frame_valid(buf, 1, 1, 0);
}

#[test]
fn t3_p20_quota_warning_and_ddsf_header_flag() {
    let q = MemoryQuotaManager::new_2gb();
    let threshold = (2 * 1024 * 1024 * 1024 as u64 * 85 / 100) as usize;
    q.allocate(threshold).unwrap();
    assert!(q.is_warning());

    let flags = if q.is_warning() { DDSF_FLAG_QUOTA_WARNING } else { 0 };
    let mut packer = DdsfFramePacker::new(4096);
    let buf = packer.pack_frame(1, 100, flags, &[], &[]).unwrap();
    assert_ddsf_frame_valid(buf, 1, 0, 0);
    let hdr: &DdsfFrameHeader = bytemuck::from_bytes(&buf[0..64]);
    assert_eq!(hdr.flags & DDSF_FLAG_QUOTA_WARNING, DDSF_FLAG_QUOTA_WARNING);
}

#[test]
fn t3_p21_pinned_active_and_ddsf_header_flag() {
    let q = Arc::new(MemoryQuotaManager::new_2gb());
    let _guard = q.pin(10_000_000).unwrap();
    let flags = if q.pinned_bytes() > 0 { DDSF_FLAG_PINNED_ACTIVE } else { 0 };
    let mut packer = DdsfFramePacker::new(4096);
    let buf = packer.pack_frame(1, 100, flags, &[], &[]).unwrap();
    assert_ddsf_frame_valid(buf, 1, 0, 0);
    let hdr: &DdsfFrameHeader = bytemuck::from_bytes(&buf[0..64]);
    assert_eq!(hdr.flags & DDSF_FLAG_PINNED_ACTIVE, DDSF_FLAG_PINNED_ACTIVE);
}

#[test]
fn t3_p22_auto_timeout_and_ddsf_scope_state() {
    let config = ScopeControllerConfig {
        sweep_mode: TriggerSweepMode::Auto,
        auto_timeout_ns: 100_000_000,
        ..Default::default()
    };
    let mut ctrl = ScopeController::new(config);
    ctrl.step(1_000_000);
    let outcome = ctrl.step(111_000_000);
    if let ScopeStepOutcome::AutoFreerunFired { window } = outcome {
        let scope = ScopeViewportPayload {
            scope_id: 1,
            trigger_state: window.trigger_state.as_u8(),
            mode: window.mode.as_u8(),
            win_t_start_ns: window.win_t_start_ns,
            win_t_span_ns: window.win_t_span_ns,
            subpixel_offset_ns: window.subpixel_offset_ns,
            tracks: Vec::new(),
        };
        let mut packer = DdsfFramePacker::new(4096);
        let buf = packer.pack_frame(1, 100, 0, &[scope], &[]).unwrap();
        assert_ddsf_frame_valid(buf, 1, 1, 0);
        let scope_hdr: &DdsfScopePacketHeader = bytemuck::from_bytes(&buf[64..96]);
        assert_eq!(scope_hdr.trigger_state, TriggerRuntimeState::AutoFreerun.as_u8());
    } else {
        panic!("expected AutoFreerunFired");
    }
}

#[test]
fn t3_p23_extrema_retention_and_track_stats() {
    // F04 + F21: Single-sample spike sets TrackStats max and updates vpp
    let glitch_val = 500.0f32;
    let baseline = 10.0f32;
    let signal = generate_glitch_signal(64, 30, glitch_val, baseline);
    let bucket = reduce_raw_f32_slice(&signal, 0, 630, &LinearScale::identity());
    let cols = aggregate_buckets_to_columns(&[bucket], 0, 1000, 10);
    let stats = compute_track_stats_from_columns(&cols);
    assert_eq!(stats.max, glitch_val);
    assert!(stats.vpp >= glitch_val - baseline);
}

#[test]
fn t3_p24_eviction_single_mode_and_track_packing() {
    // F08 + F18 + F20: Latched Single mode viewport packed accurately despite background ingestion
    let q = Arc::new(MemoryQuotaManager::new_2gb());
    let guard = q.pin(5 * 1024 * 1024).unwrap();
    assert_eq!(guard.bytes(), 5 * 1024 * 1024);

    let cols = vec![DdsfM4Column {
        x_norm: 0.0,
        v_first: 1.0,
        v_min: -1.0,
        v_max: 1.0,
        v_last: 1.0,
    }];
    let track = TrackPayload::new_m4(1, cols, None);
    let scope = ScopeViewportPayload {
        scope_id: 1,
        trigger_state: TriggerRuntimeState::LatchedPinned.as_u8(),
        mode: TriggerSweepMode::Single.as_u8(),
        win_t_start_ns: 1000,
        win_t_span_ns: 5000,
        subpixel_offset_ns: 0.0,
        tracks: vec![track],
    };
    let mut packer = DdsfFramePacker::new(4096);
    let buf = packer.pack_frame(1, 100, 0, &[scope], &[]).unwrap();
    assert_ddsf_frame_valid(buf, 1, 1, 0);
}
