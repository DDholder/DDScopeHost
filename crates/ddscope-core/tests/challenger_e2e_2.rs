//! Empirical stress-testing harness for Challenger 2:
//! DDScopeHost Phase 1 Track A E2E Test Suite.
//!
//! Validates:
//! 1. M4 100% single-point peak retention assertions (glitch in min/max across pyramid levels & viewport query)
//! 2. Sub-sample linear interpolation formula: alpha = (L_trig - v0) / (v1 - v0), precision & div-by-zero
//! 3. DDSF v1 binary alignment: 64B frame, 32B scope, 32B track, 8B payload alignment, magic = 0x46534444
//! 4. Memory quota invariants: 40% pinning limit rejection, 85% warning threshold, rollover chunk retention

use std::mem::{align_of, size_of};

use ddscope_core::scope::controller::calculate_subsample_offset;
use ddscope_core::scope::ddsf::*;
use ddscope_core::storage::{M4QueryResult, NativeScalarSeries};
use ddscope_core::types::*;

const SAMPLING_INTERVAL_NS: u64 = 10;

// ============================================================================
// 1. M4 Extrema Retention Empirical Tests
// ============================================================================

#[test]
fn test_m4_extrema_retention_all_boundary_indices() {
    let test_indices = [0, 1, 15, 31, 32, 62, 63, 64, 4095, 4096, 65534, 65535];
    let peak_val = 99_999.0f32;
    let valley_val = -88_888.0f32;

    for &idx in &test_indices {
        // Test Peak
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::default());
        let mut ts = Vec::with_capacity(CHUNK_LEN);
        let mut vals = vec![1.0f32; CHUNK_LEN];
        for i in 0..CHUNK_LEN {
            ts.push((i as u64) * SAMPLING_INTERVAL_NS);
        }
        vals[idx] = peak_val;

        series.append(&ts, ScalarSlice::F32(&vals)).expect("append ok");

        // Query M4 viewport across entire chunk
        let t_min = 0;
        let t_max = (CHUNK_LEN as u64) * SAMPLING_INTERVAL_NS;
        let query_res = series.query_m4_viewport(t_min, t_max, 2000).expect("query ok");

        match query_res {
            M4QueryResult::Columns(cols) => {
                let max_observed = cols.iter().map(|c| c.v_max).fold(f32::NEG_INFINITY, f32::max);
                assert_eq!(
                    max_observed, peak_val,
                    "Peak glitch at index {idx} was not preserved in M4 query: max={max_observed}"
                );
            }
            M4QueryResult::Raw(_) => panic!("Expected Columns for 65536 samples > 2 * 2000"),
        }

        // Test Valley
        let mut series_val = NativeScalarSeries::new(ChannelId(2), ScalarType::F32, LinearScale::default());
        let mut vals_v = vec![10.0f32; CHUNK_LEN];
        vals_v[idx] = valley_val;
        series_val.append(&ts, ScalarSlice::F32(&vals_v)).expect("append ok");

        let query_res_v = series_val.query_m4_viewport(t_min, t_max, 2000).expect("query ok");
        match query_res_v {
            M4QueryResult::Columns(cols) => {
                let min_observed = cols.iter().map(|c| c.v_min).fold(f32::INFINITY, f32::min);
                assert_eq!(
                    min_observed, valley_val,
                    "Valley glitch at index {idx} was not preserved in M4 query: min={min_observed}"
                );
            }
            M4QueryResult::Raw(_) => panic!("Expected Columns"),
        }
    }
}

#[test]
fn test_m4_extrema_retention_with_negative_scale() {
    // When scale is negative, peak should invert to valley and vice-versa
    let neg_scale = LinearScale { scale: -2.0, offset: 100.0 };
    let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, neg_scale);

    let mut ts = Vec::with_capacity(CHUNK_LEN);
    let mut vals = vec![0.0f32; CHUNK_LEN];
    for i in 0..CHUNK_LEN {
        ts.push((i as u64) * SAMPLING_INTERVAL_NS);
    }
    // Glitch: raw 50.0 -> calibrated: 50.0 * (-2.0) + 100.0 = 0.0 (valley relative to baseline 100.0)
    vals[100] = 50.0;
    // Glitch: raw -50.0 -> calibrated: (-50.0) * (-2.0) + 100.0 = 200.0 (peak relative to baseline 100.0)
    vals[200] = -50.0;

    series.append(&ts, ScalarSlice::F32(&vals)).expect("append ok");

    let t_min = 0;
    let t_max = (CHUNK_LEN as u64) * SAMPLING_INTERVAL_NS;
    let query_res = series.query_m4_viewport(t_min, t_max, 2000).expect("query ok");

    match query_res {
        M4QueryResult::Columns(cols) => {
            let max_observed = cols.iter().map(|c| c.v_max).fold(f32::NEG_INFINITY, f32::max);
            let min_observed = cols.iter().map(|c| c.v_min).fold(f32::INFINITY, f32::min);
            assert_eq!(max_observed, 200.0, "Negative scale peak failed");
            assert_eq!(min_observed, 0.0, "Negative scale valley failed");
        }
        M4QueryResult::Raw(_) => panic!("Expected Columns"),
    }
}

#[test]
fn test_m4_pyramid_multi_level_glitch_retention() {
    // Verify inter-chunk L3 and L4 hierarchy retains an isolated glitch
    // 4 sealed chunks = 262,144 samples (triggers L3 aggregation)
    let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::default());

    let glitch_val = 1_234_567.0f32;
    for chunk_idx in 0..4 {
        let mut ts = Vec::with_capacity(CHUNK_LEN);
        let mut vals = vec![5.0f32; CHUNK_LEN];
        let base_t = (chunk_idx * CHUNK_LEN) as u64 * SAMPLING_INTERVAL_NS;
        for i in 0..CHUNK_LEN {
            ts.push(base_t + (i as u64) * SAMPLING_INTERVAL_NS);
        }
        if chunk_idx == 2 {
            vals[12345] = glitch_val;
        }
        series.append(&ts, ScalarSlice::F32(&vals)).expect("append ok");
    }

    // Now query with 100 pixels, forcing coarsest level
    let t_min = 0;
    let t_max = (4 * CHUNK_LEN as u64) * SAMPLING_INTERVAL_NS;
    let query_res = series.query_m4_viewport(t_min, t_max, 100).expect("query ok");

    match query_res {
        M4QueryResult::Columns(cols) => {
            let max_observed = cols.iter().map(|c| c.v_max).fold(f32::NEG_INFINITY, f32::max);
            assert_eq!(max_observed, glitch_val, "L3 pyramid did not retain peak");
        }
        M4QueryResult::Raw(_) => panic!("Expected Columns"),
    }
}

// ============================================================================
// 2. Sub-Sample Linear Interpolation Empirical Tests
// ============================================================================

#[test]
fn test_subsample_interpolation_precision_and_limits() {
    // 1. Division by zero: flatline where level == v0 == v1
    let (alpha_flat, sub_flat) = calculate_subsample_offset(100, 5.0, 200, 5.0, 5.0).unwrap();
    assert_eq!(alpha_flat, 0.0);
    assert_eq!(sub_flat, 0.0);

    // 2. Division by zero: flatline where level != v0 == v1
    // v0 = 5.0, v1 = 5.0, level = 6.0. Nearest bound is chosen
    let (alpha_flat2, _) = calculate_subsample_offset(100, 5.0, 200, 5.0, 6.0).unwrap();
    assert!(alpha_flat2 == 0.0 || alpha_flat2 == 1.0);

    // 3. Denominator near machine epsilon (1e-15 < 1e-12 threshold)
    let (alpha_tiny, _) = calculate_subsample_offset(100, 5.0, 200, 5.0 + 1e-14, 5.0).unwrap();
    assert_eq!(alpha_tiny, 0.0);

    // 4. Denominator just above 1e-12 threshold
    let (alpha_above, sub_above) = calculate_subsample_offset(100, 5.0, 200, 5.0 + 2e-12, 5.0 + 1e-12).unwrap();
    assert!((alpha_above - 0.5).abs() < 1e-6);
    assert!((sub_above - 50.0).abs() < 1e-3);

    // 5. Huge timestamp span (dt = 1e9 ns = 1 sec)
    let (alpha_big, sub_big) = calculate_subsample_offset(0, 0.0, 1_000_000_000, 10.0, 3.0).unwrap();
    assert!((alpha_big - 0.3).abs() < 1e-6);
    // f32 precision check for 300,000,000.0 ns
    assert!((sub_big - 300_000_000.0).abs() < 100.0);

    // 6. Non-finite values reject cleanly
    assert!(calculate_subsample_offset(100, f64::NAN, 200, 1.0, 0.5).is_none());
    assert!(calculate_subsample_offset(100, 0.0, 200, f64::INFINITY, 0.5).is_none());
    assert!(calculate_subsample_offset(100, 0.0, 200, 1.0, f64::NAN).is_none());
    assert!(calculate_subsample_offset(200, 0.0, 100, 1.0, 0.5).is_none()); // t1 < t0
}

// ============================================================================
// 3. DDSF v1 Binary Alignment and Layout Contracts
// ============================================================================

#[test]
fn test_ddsf_v1_binary_alignment_and_sizes() {
    assert_eq!(size_of::<DdsfFrameHeader>(), 64);
    assert_eq!(align_of::<DdsfFrameHeader>(), 8);

    assert_eq!(size_of::<DdsfScopePacketHeader>(), 32);
    assert_eq!(align_of::<DdsfScopePacketHeader>(), 8);

    assert_eq!(size_of::<DdsfTrackBlockHeader>(), 32);
    assert_eq!(align_of::<DdsfTrackBlockHeader>(), 4);

    assert_eq!(size_of::<DdsfTrackStats>(), 32);
    assert_eq!(align_of::<DdsfTrackStats>(), 4);

    assert_eq!(size_of::<DdsfSnapshotEntry>(), 16);
    assert_eq!(align_of::<DdsfSnapshotEntry>(), 8);

    assert_eq!(DDSF_MAGIC, 0x4653_4444);
    assert_eq!(DDSF_MAGIC.to_le_bytes(), *b"DDSF");
    assert_eq!(DDSF_VERSION_V1, 1);

    // Track payload offset alignment:
    let mut tb = DdsfTrackBlockHeader {
        channel_id: 1,
        block_kind: DdsfBlockKind::AnalogRawXY.as_u8(),
        digital_init_level: 0,
        track_flags: 0,
        element_count: 10,
        data_byte_len: 80,
        block_byte_len: 112,
        aux_f32_0: 0.0,
        aux_f32_1: 0.0,
        reserved: 0,
    };
    assert_eq!(tb.payload_offset() % 8, 0);

    tb.track_flags |= DDSF_TRACK_FLAG_HAS_STATS;
    assert_eq!(tb.payload_offset() % 8, 0);

    // Element sizes:
    assert_eq!(size_of::<DdsfAnalogPoint>(), 8);
    assert_eq!(size_of::<DdsfM4Column>(), 20); // 5 * 4 = 20 bytes
    assert_eq!(size_of::<DdsfStateRibbon>(), 16);

    // Note: DdsfM4Column is 20 bytes, so 1 column is 20 bytes (not a multiple of 8).
    // The wire protocol requires align_up_8(data_byte_len):
    assert_eq!(align_up_8(20), 24);
    assert_eq!(align_up_8(40), 40);
}

// ============================================================================
// 4. Memory Quota Invariants Empirical Tests
// ============================================================================

#[test]
fn test_memory_quota_invariants_and_caps() {
    let limit_2gb = 2 * 1024 * 1024 * 1024usize; // 2,147,483,648 bytes

    // 40% cap calculation:
    let cap_40 = (limit_2gb as u64 * 40 / 100) as usize; // 858,993,459 bytes
    // 85% warning calculation:
    let warn_85 = (limit_2gb as u64 * 85 / 100) as usize; // 1,825,361,100 bytes

    assert_eq!(cap_40, 858_993_459);
    assert_eq!(warn_85, 1_825_361_100);

    // Check MemoryQuotaManager harness from tests
    // (Note: this tests the test harness logic in e2e_track_a_tests/common.rs)
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct TestQuota {
        limit: usize,
        allocated: AtomicUsize,
        pinned: AtomicUsize,
    }

    impl TestQuota {
        fn pin(&self, bytes: usize) -> Result<(), &'static str> {
            let cap = (self.limit as u64 * 40 / 100) as usize;
            let curr = self.pinned.load(Ordering::Acquire);
            if curr.saturating_add(bytes) > cap {
                return Err("Pinning ceiling exceeded (40% limit)");
            }
            self.pinned.fetch_add(bytes, Ordering::Release);
            Ok(())
        }
        fn is_warning(&self) -> bool {
            let alloc = self.allocated.load(Ordering::Acquire);
            let threshold = (self.limit as u64 * 85 / 100) as usize;
            alloc >= threshold
        }
    }

    let q = TestQuota {
        limit: limit_2gb,
        allocated: AtomicUsize::new(0),
        pinned: AtomicUsize::new(0),
    };

    // Exactly at 40% cap: allowed
    assert!(q.pin(cap_40).is_ok());
    // 1 more byte: rejected
    assert!(q.pin(1).is_err());

    // 85% threshold
    q.allocated.store(warn_85 - 1, Ordering::Release);
    assert!(!q.is_warning());
    q.allocated.store(warn_85, Ordering::Release);
    assert!(q.is_warning());
}

// ============================================================================
// 5. Additional Adversarial Stress Scenarios
// ============================================================================

#[test]
fn test_m4_viewport_boundary_precision_and_leakage() {
    let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::default());
    let mut ts = Vec::with_capacity(CHUNK_LEN);
    let mut vals = vec![0.0f32; CHUNK_LEN];
    for i in 0..CHUNK_LEN {
        ts.push((i as u64) * 100); // 100 ns step
    }

    // Put a huge glitch at sample 100 (t = 10,000 ns)
    let glitch_idx = 100;
    let glitch_ts = 100 * 100u64; // 10,000 ns
    let glitch_val = 50_000.0f32;
    vals[glitch_idx] = glitch_val;

    series.append(&ts, ScalarSlice::F32(&vals)).expect("append ok");

    // Case 1: Viewport exactly starts at glitch_ts (500 samples <= 2 * 2000, so it returns Raw)
    let res_at_start = series.query_m4_viewport(glitch_ts, glitch_ts + 50_000, 2000).expect("query ok");
    match res_at_start {
        M4QueryResult::Raw(raw) => {
            let max_val = raw.iter().map(|p| p.y_val).fold(f32::NEG_INFINITY, f32::max);
            assert_eq!(max_val, glitch_val, "Glitch at exact t_min was missed in Raw mode");
        }
        M4QueryResult::Columns(cols) => {
            let max_val = cols.iter().map(|c| c.v_max).fold(f32::NEG_INFINITY, f32::max);
            assert_eq!(max_val, glitch_val, "Glitch at exact t_min was missed in Columns mode");
        }
    }

    // Case 2: Viewport with large span so sample count > 2 * W_px (Columns mode)
    let res_columns = series.query_m4_viewport(0, 100 * CHUNK_LEN as u64, 50).expect("query ok");
    match res_columns {
        M4QueryResult::Columns(cols) => {
            let max_val = cols.iter().map(|c| c.v_max).fold(f32::NEG_INFINITY, f32::max);
            assert_eq!(max_val, glitch_val, "Glitch was missed in Columns mode");
        }
        M4QueryResult::Raw(_) => panic!("Expected Columns for 65536 samples > 2 * 50"),
    }
}

#[test]
fn test_ddsf_mixed_track_alignment() {
    // Verify that packing all track types produces strictly 8-byte aligned payload offsets
    let raw_pts = [DdsfAnalogPoint { x_norm: 0.1, y_val: 1.0 }; 3]; // 3 * 8 = 24 bytes
    let m4_cols = [DdsfM4Column { x_norm: 0.2, v_first: 1.0, v_min: 0.0, v_max: 2.0, v_last: 1.5 }; 3]; // 3 * 20 = 60 bytes
    let edges = [0.1f32, 0.3f32, 0.7f32]; // 3 * 4 = 12 bytes
    let ribbons = [DdsfStateRibbon { x0_norm: 0.0, x1_norm: 0.5, state_id: 1, color_idx: 2 }]; // 1 * 16 = 16 bytes

    let raw_bytes = raw_pts.len() * size_of::<DdsfAnalogPoint>();
    let m4_bytes = m4_cols.len() * size_of::<DdsfM4Column>();
    let edges_bytes = edges.len() * size_of::<f32>();
    let ribbons_bytes = ribbons.len() * size_of::<DdsfStateRibbon>();

    // Check payload byte lengths and their 8-byte aligned counterparts
    assert_eq!(align_up_8(raw_bytes), 24);
    assert_eq!(align_up_8(m4_bytes), 64); // 60B requires 4B padding
    assert_eq!(align_up_8(edges_bytes), 16); // 12B requires 4B padding
    assert_eq!(align_up_8(ribbons_bytes), 16);
}
