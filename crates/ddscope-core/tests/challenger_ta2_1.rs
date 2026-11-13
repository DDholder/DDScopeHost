//! Adversarial challenge test suite for Milestone TA2:
//! 64-ary M4 Envelope Pyramid & Viewport Query in `ddscope-core`.
//!
//! Author: challenger_ta2_1

use ddscope_core::storage::{M4QueryResult, NativeScalarSeries};
use ddscope_core::types::{ChannelId, LinearScale, ScalarSlice, ScalarType, CHUNK_LEN};

const SAMPLING_INTERVAL_NS: u64 = 10; // 10 ns per sample

/// Helper to build a series with baseline sine wave and specific impulse spikes/valleys.
fn create_series_with_spikes_and_base(
    base_time_ns: u64,
    num_chunks: usize,
    active_tail_len: usize,
    spikes: &[(usize, f32)], // (global_sample_index, spike_value)
    scale: LinearScale,
) -> NativeScalarSeries {
    let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, scale);

    let total_samples = num_chunks * CHUNK_LEN + active_tail_len;
    let mut ts_buf = Vec::with_capacity(CHUNK_LEN);
    let mut val_buf = Vec::with_capacity(CHUNK_LEN);

    let mut current_sample = 0;

    // Sealed chunks
    for chunk_idx in 0..num_chunks {
        ts_buf.clear();
        val_buf.clear();
        let base_t = base_time_ns + (chunk_idx * CHUNK_LEN) as u64 * SAMPLING_INTERVAL_NS;

        for i in 0..CHUNK_LEN {
            let t = base_t + (i as u64) * SAMPLING_INTERVAL_NS;
            let val = ((chunk_idx * CHUNK_LEN + i) as f32 * 0.01).sin();
            ts_buf.push(t);
            val_buf.push(val);
        }

        // Apply spikes in this chunk
        for &(spike_idx, spike_val) in spikes {
            if spike_idx >= chunk_idx * CHUNK_LEN && spike_idx < (chunk_idx + 1) * CHUNK_LEN {
                val_buf[spike_idx - chunk_idx * CHUNK_LEN] = spike_val;
            }
        }

        series
            .append(&ts_buf, ScalarSlice::F32(&val_buf))
            .expect("append sealed chunk ok");
        current_sample += CHUNK_LEN;
    }

    // Active unsealed tail
    if active_tail_len > 0 {
        ts_buf.clear();
        val_buf.clear();
        let base_t = base_time_ns + current_sample as u64 * SAMPLING_INTERVAL_NS;

        for i in 0..active_tail_len {
            let t = base_t + (i as u64) * SAMPLING_INTERVAL_NS;
            let val = ((current_sample + i) as f32 * 0.01).sin();
            ts_buf.push(t);
            val_buf.push(val);
        }

        for &(spike_idx, spike_val) in spikes {
            if spike_idx >= current_sample && spike_idx < current_sample + active_tail_len {
                val_buf[spike_idx - current_sample] = spike_val;
            }
        }

        series
            .append(&ts_buf, ScalarSlice::F32(&val_buf))
            .expect("append active tail ok");
    }

    assert_eq!(series.total_samples(), total_samples as u64);
    series
}

fn create_series_with_spikes(
    num_chunks: usize,
    active_tail_len: usize,
    spikes: &[(usize, f32)],
    scale: LinearScale,
) -> NativeScalarSeries {
    create_series_with_spikes_and_base(0, num_chunks, active_tail_len, spikes, scale)
}

// ===========================================================================
// Test Group 1: Single-point spikes at exact 64-sample chunk boundaries
// ===========================================================================
#[test]
fn test_isolated_spikes_at_64_sample_boundaries() {
    let scale = LinearScale::identity();
    // Test each boundary spike in its own isolated scenario to avoid co-pixel eclipsing
    let boundary_cases = [
        (63, 10_000.0, "End of first 64-sample L1 block (index 63)"),
        (64, -10_000.0, "Start of second 64-sample L1 block (index 64)"),
        (127, 20_000.0, "End of second L1 block (index 127)"),
        (128, -20_000.0, "Start of third L1 block (index 128)"),
        (4095, 30_000.0, "End of first L2 block (index 4095)"),
        (4096, -30_000.0, "Start of second L2 block (index 4096)"),
    ];

    let widths = [2000, 1000, 500, 100];

    for &(idx, val, desc) in &boundary_cases {
        let series = create_series_with_spikes(2, 0, &[(idx, val)], scale);
        let (t_start, t_end) = series.time_range_ns().unwrap();

        for &w_px in &widths {
            let res = series
                .query_m4_viewport(t_start, t_end, w_px)
                .expect("query ok");

            match res {
                M4QueryResult::Columns(cols) => {
                    assert_eq!(cols.len(), w_px);
                    let t_spike = idx as u64 * SAMPLING_INTERVAL_NS;
                    let p_expected = (((t_spike - t_start) as f64 / (t_end - t_start) as f64)
                        * w_px as f64) as usize;
                    let p_expected = p_expected.min(w_px - 1);

                    let matching_col_idx = cols.iter().position(|c| {
                        if val > 0.0 {
                            (c.v_max - val).abs() < 1e-2
                        } else {
                            (c.v_min - val).abs() < 1e-2
                        }
                    });

                    assert!(
                        matching_col_idx.is_some(),
                        "Boundary spike at {desc} (val {val}) NOT retained globally at w_px={w_px}"
                    );

                    let p_actual = matching_col_idx.unwrap();
                    let pixel_diff = (p_actual as isize - p_expected as isize).abs();

                    println!(
                        "Boundary [{desc}]: w_px={w_px}: p_expected={p_expected}, p_actual={p_actual}, diff={pixel_diff}"
                    );

                    assert!(
                        pixel_diff <= 1,
                        "Boundary spike at {desc} shifted by {pixel_diff} pixels (> 1) at w_px={w_px}"
                    );
                }
                M4QueryResult::Raw(_) => panic!("Expected Columns for 2 chunks"),
            }
        }
    }
}

// ===========================================================================
// Test Group 2: Single-point spikes at chunk start (index 0) and chunk end (index 65,535)
// ===========================================================================
#[test]
fn test_spikes_at_chunk_start_and_end() {
    let scale = LinearScale::identity();
    let edge_cases = [
        (0, 88_888.0, "Chunk 0 index 0"),
        (65_535, -77_777.0, "Chunk 0 index 65535"),
        (65_536, 99_999.0, "Chunk 1 index 0"),
        (2 * CHUNK_LEN - 1, -66_666.0, "Chunk 1 index 65535"),
    ];

    let widths = [2000, 1000, 500, 100];

    for &(idx, val, desc) in &edge_cases {
        let series = create_series_with_spikes(2, 0, &[(idx, val)], scale);
        let (t_start, t_end) = series.time_range_ns().unwrap();

        for &w_px in &widths {
            let res = series
                .query_m4_viewport(t_start, t_end, w_px)
                .expect("query ok");

            match res {
                M4QueryResult::Columns(cols) => {
                    assert_eq!(cols.len(), w_px);
                    let t_spike = idx as u64 * SAMPLING_INTERVAL_NS;
                    let p_expected = (((t_spike - t_start) as f64 / (t_end - t_start) as f64)
                        * w_px as f64) as usize;
                    let p_expected = p_expected.min(w_px - 1);

                    let matching_col_idx = cols.iter().position(|c| {
                        if val > 0.0 {
                            (c.v_max - val).abs() < 1e-2
                        } else {
                            (c.v_min - val).abs() < 1e-2
                        }
                    });

                    assert!(
                        matching_col_idx.is_some(),
                        "Edge spike at {desc} (val {val}) NOT retained globally at w_px={w_px}"
                    );

                    let p_actual = matching_col_idx.unwrap();
                    let pixel_diff = (p_actual as isize - p_expected as isize).abs();

                    println!(
                        "Edge [{desc}]: w_px={w_px}: p_expected={p_expected}, p_actual={p_actual}, diff={pixel_diff}"
                    );

                    assert!(
                        pixel_diff <= 1,
                        "Edge spike at {desc} shifted by {pixel_diff} pixels (> 1) at w_px={w_px}"
                    );
                }
                M4QueryResult::Raw(_) => panic!("Expected Columns for 2 chunks"),
            }
        }
    }
}

// ===========================================================================
// Test Group 3: Single-point spikes in active unsealed chunk tail (< 64 samples)
// ===========================================================================
#[test]
fn test_spikes_in_active_unsealed_tail() {
    let scale = LinearScale::identity();
    // 1 sealed chunk + active tail of 45 samples (< 64 samples)
    let active_tail_len = 45;
    let spikes = vec![
        (CHUNK_LEN + 10, 55_555.0),  // Isolated peak in active tail
        (CHUNK_LEN + 30, -44_444.0), // Isolated valley in active tail
    ];

    let series = create_series_with_spikes(1, active_tail_len, &spikes, scale);
    let (t_start, t_end) = series.time_range_ns().unwrap();

    let widths = [2000, 1000, 500, 100];
    for &w_px in &widths {
        let res = series
            .query_m4_viewport(t_start, t_end, w_px)
            .expect("query ok");

        match res {
            M4QueryResult::Columns(cols) => {
                assert_eq!(cols.len(), w_px);
                for &(idx, expected_val) in &spikes {
                    let global_found = if expected_val > 0.0 {
                        cols.iter().any(|c| (c.v_max - expected_val).abs() < 1e-2)
                    } else {
                        cols.iter().any(|c| (c.v_min - expected_val).abs() < 1e-2)
                    };

                    assert!(
                        global_found,
                        "Active tail spike at sample {idx} (val {expected_val}) NOT retained globally at w_px={w_px}"
                    );
                }
            }
            M4QueryResult::Raw(_) => panic!("Expected Columns for 1 chunk + tail"),
        }
    }
}

// ===========================================================================
// Test Group 4: Float Extremes (f32::MAX, f32::MIN, negative scale inverted spikes)
// ===========================================================================
#[test]
fn test_float_extremes_and_negative_scale() {
    // Test A: f32::MAX and f32::MIN with identity scale
    {
        let scale = LinearScale::identity();
        let spikes = vec![
            (100, f32::MAX),
            (200, f32::MIN),
        ];

        let series = create_series_with_spikes(2, 0, &spikes, scale);
        let (t_start, t_end) = series.time_range_ns().unwrap();

        let res = series.query_m4_viewport(t_start, t_end, 2000).expect("query ok");
        if let M4QueryResult::Columns(cols) = res {
            let max_found = cols.iter().any(|c| c.v_max == f32::MAX);
            let min_found = cols.iter().any(|c| c.v_min == f32::MIN);

            assert!(max_found, "f32::MAX spike must be retained exactly in v_max");
            assert!(min_found, "f32::MIN valley must be retained exactly in v_min");
        } else {
            panic!("Expected Columns");
        }
    }

    // Test B: Negative scale inversion
    // Physical = raw * (-3.0) + 100.0
    // A positive raw spike of +500.0 becomes 500 * (-3.0) + 100 = -1400.0 (a valley!)
    // A negative raw dip of -500.0 becomes (-500) * (-3.0) + 100 = +1600.0 (a peak!)
    {
        let inverted_scale = LinearScale::new(-3.0, 100.0);
        let spikes = vec![
            (500, 500.0),   // Raw peak -> Scaled valley: -1400.0
            (1000, -500.0), // Raw valley -> Scaled peak: +1600.0
        ];

        let series = create_series_with_spikes(2, 0, &spikes, inverted_scale);
        let (t_start, t_end) = series.time_range_ns().unwrap();

        let res = series.query_m4_viewport(t_start, t_end, 2000).expect("query ok");
        if let M4QueryResult::Columns(cols) = res {
            let peak_found = cols.iter().any(|c| (c.v_max - 1600.0).abs() < 1e-2);
            let valley_found = cols.iter().any(|c| (c.v_min - (-1400.0)).abs() < 1e-2);

            assert!(peak_found, "Inverted peak (+1600.0) must be retained in v_max");
            assert!(valley_found, "Inverted valley (-1400.0) must be retained in v_min");
        } else {
            panic!("Expected Columns");
        }
    }
}

// ===========================================================================
// Test Group 5: Viewport Queries with Various W_px over Large Multi-Chunk Series
// ===========================================================================
#[test]
fn test_multi_level_pyramid_queries_large_series() {
    let scale = LinearScale::identity();
    // 300 sealed chunks (~19.6M samples), which activates L3 and L4 pyramid levels
    let spikes = vec![
        (10_000, 77_777.0),          // Within chunk 0
        (300_000, -88_888.0),        // In L3 range (chunk 4)
        (17_000_000, 99_999.0),      // In L4 range (chunk 259)
    ];

    let series = create_series_with_spikes(300, 50, &spikes, scale);
    let (t_start, t_end) = series.time_range_ns().unwrap();

    let widths = [2000, 1000, 500, 100];
    for &w_px in &widths {
        let res = series
            .query_m4_viewport(t_start, t_end, w_px)
            .expect("query ok");

        if let M4QueryResult::Columns(cols) = res {
            assert_eq!(cols.len(), w_px);

            for &(idx, expected_val) in &spikes {
                let global_found = if expected_val > 0.0 {
                    cols.iter().any(|c| (c.v_max - expected_val).abs() < 1e-2)
                } else {
                    cols.iter().any(|c| (c.v_min - expected_val).abs() < 1e-2)
                };

                assert!(
                    global_found,
                    "Large series spike at sample {idx} (val {expected_val}) NOT retained at w_px={w_px}"
                );
            }
        } else {
            panic!("Expected Columns for 300 chunks");
        }
    }
}

// ===========================================================================
// Test Group 6: Boundary & Malformed Inputs
// ===========================================================================
#[test]
fn test_boundary_and_malformed_inputs() {
    let scale = LinearScale::identity();
    // Start at base_time_ns = 500_000 ns so that timestamps < 500_000 exist as valid pre-window values
    let series = create_series_with_spikes_and_base(500_000, 2, 0, &[], scale);
    let (t_start, t_end) = series.time_range_ns().unwrap();
    assert_eq!(t_start, 500_000);

    // 1. Time window completely outside data bounds: t_max < t_first (e.g. 100_000 to 200_000)
    {
        let res = series.query_m4_viewport(100_000, 200_000, 1000);
        assert!(res.is_ok(), "Query before data bounds should not error");
        match res.unwrap() {
            M4QueryResult::Raw(pts) => assert!(pts.is_empty(), "Expected empty raw points"),
            M4QueryResult::Columns(cols) => assert!(cols.is_empty(), "Expected empty columns"),
        }
    }

    // 2. Time window completely outside data bounds: t_min > t_last
    {
        let res = series.query_m4_viewport(t_end + 100, t_end + 1000, 1000);
        assert!(res.is_ok(), "Query after data bounds should not error");
        match res.unwrap() {
            M4QueryResult::Raw(pts) => assert!(pts.is_empty(), "Expected empty raw points"),
            M4QueryResult::Columns(cols) => assert!(cols.is_empty(), "Expected empty columns"),
        }
    }

    // 3. Inverted time window: t_min > t_max
    {
        let res = series.query_m4_viewport(t_end, t_start, 1000);
        assert!(res.is_err(), "Inverted time window must return Err");
    }

    // 4. Equal time window: t_min == t_max
    {
        let res = series.query_m4_viewport(t_start, t_start, 1000);
        assert!(res.is_err(), "Equal time window must return Err");
    }

    // 5. W_px = 0
    {
        let res = series.query_m4_viewport(t_start, t_end, 0);
        assert!(res.is_err(), "W_px = 0 must return Err");
    }

    // 6. Empty series
    {
        let empty_series = NativeScalarSeries::new(ChannelId(2), ScalarType::F32, scale);
        let res = empty_series.query_m4_viewport(1000, 2000, 1000);
        assert!(res.is_ok(), "Empty series query should not error");
        match res.unwrap() {
            M4QueryResult::Raw(pts) => assert!(pts.is_empty()),
            M4QueryResult::Columns(cols) => assert!(cols.is_empty()),
        }
    }
}

// ===========================================================================
// Test Group 7: Adversarial Sub-Chunk / Sparse Decimation (N > 2 W_px, but N / W_px < 64)
// ===========================================================================
#[test]
fn test_sub_chunk_sparse_decimation_peak_location() {
    let scale = LinearScale::identity();
    // 3,000 samples total, active unsealed chunk (0 sealed chunks).
    // Query with w_px = 1,000.
    // Here N = 3,000 > 2 * 1,000 = 2,000, so query_m4_viewport enters Columns mode!
    // But samples_per_pixel = 3,000 / 1,000 = 3 samples per pixel.
    // L1 bucket size is 64 samples, which is 64 / 3 ≈ 21.3 pixels wide!
    let spikes = vec![
        (5, 77_777.0),    // Sample 5: idealized pixel is 5 / 3 = 1
        (100, -88_888.0), // Sample 100: idealized pixel is 100 / 3 = 33
    ];

    let series = create_series_with_spikes(0, 3000, &spikes, scale);
    let (t_start, t_end) = series.time_range_ns().unwrap();

    let res = series
        .query_m4_viewport(t_start, t_end, 1000)
        .expect("query ok");

    match res {
        M4QueryResult::Columns(cols) => {
            assert_eq!(cols.len(), 1000);

            let max_global = cols.iter().any(|c| (c.v_max - 77_777.0).abs() < 1e-2);
            let min_global = cols.iter().any(|c| (c.v_min - (-88_888.0)).abs() < 1e-2);

            println!("Sub-chunk sparse decimation: max_global={max_global}, min_global={min_global}");

            let p_actual_max = cols.iter().position(|c| (c.v_max - 77_777.0).abs() < 1e-2);
            let p_actual_min = cols.iter().position(|c| (c.v_min - (-88_888.0)).abs() < 1e-2);

            println!(
                "p_actual_max={:?} (expected ~1), p_actual_min={:?} (expected ~33)",
                p_actual_max, p_actual_min
            );

            assert!(max_global, "Peak 77,777.0 must be retained globally in v_max");
            assert!(min_global, "Valley -88,888.0 must be retained globally in v_min");
        }
        M4QueryResult::Raw(_) => {
            println!("Returned Raw points!");
        }
    }
}


// ===========================================================================
// Test Group 8: Zooming from 100M points down to Deep Zoom (Raw points mode)
// ===========================================================================
#[test]
fn test_adaptive_switching_raw_vs_columns_and_deep_zoom() {
    let scale = LinearScale::identity();
    let spikes = vec![
        (100, 12_345.0),
        (105, -54_321.0),
    ];

    let series = create_series_with_spikes(2, 0, &spikes, scale);
    let (t_start, _t_end) = series.time_range_ns().unwrap();

    // Query 1: Full view (131,072 samples > 2 * 2,000 = 4,000) -> Columns mode
    let res_full = series.query_m4_viewport(t_start, t_start + 131_071 * SAMPLING_INTERVAL_NS, 2000).unwrap();
    assert!(matches!(res_full, M4QueryResult::Columns(_)));

    // Query 2: Deep zoom window around the spikes (samples 90 to 120 = 31 samples <= 2 * 2000) -> Raw mode
    let t_zoom_start = t_start + 90 * SAMPLING_INTERVAL_NS;
    let t_zoom_end = t_start + 120 * SAMPLING_INTERVAL_NS;
    let res_zoom = series.query_m4_viewport(t_zoom_start, t_zoom_end, 2000).unwrap();

    match res_zoom {
        M4QueryResult::Raw(pts) => {
            assert_eq!(pts.len(), 31, "Raw mode must return exactly 31 raw points");
            let peak_pt = pts.iter().find(|p| (p.y_val - 12_345.0).abs() < 1e-3);
            let valley_pt = pts.iter().find(|p| (p.y_val - (-54_321.0)).abs() < 1e-3);

            assert!(peak_pt.is_some(), "Peak must be preserved exactly in Raw mode");
            assert!(valley_pt.is_some(), "Valley must be preserved exactly in Raw mode");

            // Verify x_norm is strictly normalized within [0, 1]
            for pt in &pts {
                assert!(pt.x_norm >= 0.0 && pt.x_norm <= 1.0, "x_norm must be in [0, 1]");
            }
        }
        M4QueryResult::Columns(_) => panic!("Expected Raw mode for 31 samples with w_px=2000"),
    }
}

// ===========================================================================
// Test Group 9: Floating Point NaN & Inf Resilience
// ===========================================================================
#[test]
fn test_nan_and_inf_resilience_in_series() {
    let scale = LinearScale::identity();
    let spikes = vec![
        (50, f32::NAN),
        (51, 999.0),
        (52, -888.0),
        (100, f32::INFINITY),
        (200, f32::NEG_INFINITY),
    ];

    let series = create_series_with_spikes(2, 0, &spikes, scale);
    let (t_start, t_end) = series.time_range_ns().unwrap();

    let res = series.query_m4_viewport(t_start, t_end, 1000).expect("query ok");

    match res {
        M4QueryResult::Columns(cols) => {
            assert_eq!(cols.len(), 1000);
            for c in &cols {
                assert!(!c.v_min.is_nan(), "Column v_min must not be NaN");
                assert!(!c.v_max.is_nan(), "Column v_max must not be NaN");
                assert!(!c.v_first.is_nan(), "Column v_first must not be NaN");
                assert!(!c.v_last.is_nan(), "Column v_last must not be NaN");
            }
        }
        M4QueryResult::Raw(_) => panic!("Expected Columns"),
    }
}

// ===========================================================================
// Test Group 10: High-Density Alternating Impulse Train
// ===========================================================================
#[test]
fn test_high_density_alternating_impulse_train() {
    let scale = LinearScale::identity();
    // Insert alternating impulse spikes every 64 samples across 4 chunks (262,144 samples)
    let num_chunks = 4;
    let mut spikes = Vec::new();

    for block in 0..(num_chunks * CHUNK_LEN / 64) {
        let idx = block * 64 + 32;
        let val = if block % 2 == 0 { 5000.0 } else { -5000.0 };
        spikes.push((idx, val));
    }

    let series = create_series_with_spikes(num_chunks, 0, &spikes, scale);
    let (t_start, t_end) = series.time_range_ns().unwrap();

    let widths = [2000, 1000, 500, 100];
    for &w_px in &widths {
        let res = series.query_m4_viewport(t_start, t_end, w_px).expect("query ok");
        match res {
            M4QueryResult::Columns(cols) => {
                assert_eq!(cols.len(), w_px);
                // At any downsampled width, the global waveform envelope must retain +5000 and -5000
                let max_all = cols.iter().map(|c| c.v_max).fold(f32::NEG_INFINITY, f32::max);
                let min_all = cols.iter().map(|c| c.v_min).fold(f32::INFINITY, f32::min);

                assert!(
                    (max_all - 5000.0).abs() < 1e-2,
                    "High density max +5000 must be retained at w_px={w_px}, got {max_all}"
                );
                assert!(
                    (min_all - (-5000.0)).abs() < 1e-2,
                    "High density min -5000 must be retained at w_px={w_px}, got {min_all}"
                );
            }
            M4QueryResult::Raw(_) => panic!("Expected Columns"),
        }
    }
}
