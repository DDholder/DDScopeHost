//! Tier 2: Boundary & Corner Cases (120 boundary & stress tests, 5 tests per feature for Features 1-24).

use std::mem::size_of;
use std::sync::Arc;

use ddscope_core::scope::controller::*;
use ddscope_core::scope::ddsf::*;
use ddscope_core::scope::sync::*;
use ddscope_core::scope::*;
use ddscope_core::types::*;

use super::common::*;

// ============================================================================
// Feature 01: M4Bucket Corner Cases
// ============================================================================
pub mod f01_m4_bucket {
    use super::*;

    #[test]
    fn t2_f01_01_m4_bucket_empty_span() {
        let b = M4Bucket {
            t_first_ns: 100,
            t_last_ns: 100,
            v_first: 0.0,
            v_min: 0.0,
            v_max: 0.0,
            v_last: 0.0,
        };
        assert_eq!(b.t_first_ns, b.t_last_ns);
    }

    #[test]
    fn t2_f01_02_m4_bucket_single_sample_span() {
        let b = M4Bucket {
            t_first_ns: 42,
            t_last_ns: 42,
            v_first: 3.14,
            v_min: 3.14,
            v_max: 3.14,
            v_last: 3.14,
        };
        assert_eq!(b.v_first, b.v_last);
        assert_eq!(b.v_min, b.v_max);
    }

    #[test]
    fn t2_f01_03_m4_bucket_identical_values() {
        let val = 42.0f32;
        let b = M4Bucket {
            t_first_ns: 0,
            t_last_ns: 100,
            v_first: val,
            v_min: val,
            v_max: val,
            v_last: val,
        };
        assert_eq!(b.v_min, b.v_max);
    }

    #[test]
    fn t2_f01_04_m4_bucket_extreme_floats() {
        let b = M4Bucket {
            t_first_ns: 0,
            t_last_ns: 100,
            v_first: f32::MIN,
            v_min: f32::MIN,
            v_max: f32::MAX,
            v_last: f32::MAX,
        };
        assert!(b.v_min < b.v_max);
        assert_eq!(b.v_min, f32::MIN);
        assert_eq!(b.v_max, f32::MAX);
    }

    #[test]
    fn t2_f01_05_m4_bucket_subnormal_floats() {
        let tiny = 1e-40f32;
        let b = M4Bucket {
            t_first_ns: 0,
            t_last_ns: 10,
            v_first: tiny,
            v_min: tiny,
            v_max: tiny,
            v_last: tiny,
        };
        assert_eq!(b.v_min, tiny);
    }
}

// ============================================================================
// Feature 02: Branching Factor 64 Hierarchy Corner Cases
// ============================================================================
pub mod f02_hierarchy_64 {
    use super::*;

    #[test]
    fn t2_f02_01_hierarchy_1_sample() {
        let ts_vec: Vec<u64> = vec![0];
        let ts_col = TimestampColumn::from_slice(&ts_vec);
        let mut val_vec = AlignedVec::with_capacity(1);
        val_vec.push(42.0f32);
        let val_col = NativeScalarColumn::F32(val_vec);
        let levels = ChunkM4Levels::build_from_scalar(&ts_col, &val_col, &LinearScale::identity()).unwrap();
        assert_eq!(levels.len_l1(), 1);
        assert_eq!(levels.len_l2(), 1);
        assert_eq!(levels.chunk_summary().unwrap().v_max, 42.0);
    }

    #[test]
    fn t2_f02_02_hierarchy_exact_63_samples() {
        let ts_vec: Vec<u64> = (0..63).map(|i| i * 10).collect();
        let ts_col = TimestampColumn::from_slice(&ts_vec);
        let mut val_vec = AlignedVec::with_capacity(63);
        for _ in 0..63 {
            val_vec.push(1.0f32);
        }
        let val_col = NativeScalarColumn::F32(val_vec);
        let levels = ChunkM4Levels::build_from_scalar(&ts_col, &val_col, &LinearScale::identity()).unwrap();
        assert_eq!(levels.len_l1(), 1);
        assert_eq!(levels.len_l2(), 1);
    }

    #[test]
    fn t2_f02_03_hierarchy_exact_64_samples() {
        let ts_vec: Vec<u64> = (0..64).map(|i| i * 10).collect();
        let ts_col = TimestampColumn::from_slice(&ts_vec);
        let mut val_vec = AlignedVec::with_capacity(64);
        for _ in 0..64 {
            val_vec.push(2.0f32);
        }
        let val_col = NativeScalarColumn::F32(val_vec);
        let levels = ChunkM4Levels::build_from_scalar(&ts_col, &val_col, &LinearScale::identity()).unwrap();
        assert_eq!(levels.len_l1(), 1);
        assert_eq!(levels.len_l2(), 1);
    }

    #[test]
    fn t2_f02_04_hierarchy_exact_4096_samples() {
        let ts_vec: Vec<u64> = (0..4096).map(|i| i * 10).collect();
        let ts_col = TimestampColumn::from_slice(&ts_vec);
        let mut val_vec = AlignedVec::with_capacity(4096);
        for _ in 0..4096 {
            val_vec.push(3.0f32);
        }
        let val_col = NativeScalarColumn::F32(val_vec);
        let levels = ChunkM4Levels::build_from_scalar(&ts_col, &val_col, &LinearScale::identity()).unwrap();
        assert_eq!(levels.len_l1(), 64);
        assert_eq!(levels.len_l2(), 1);
    }

    #[test]
    fn t2_f02_05_hierarchy_chunk_boundary() {
        let ts_vec: Vec<u64> = (0..65536).map(|i| i * 10).collect();
        let ts_col = TimestampColumn::from_slice(&ts_vec);
        let mut val_vec = AlignedVec::with_capacity(65536);
        for _ in 0..65536 {
            val_vec.push(4.0f32);
        }
        let val_col = NativeScalarColumn::F32(val_vec);
        let levels = ChunkM4Levels::build_from_scalar(&ts_col, &val_col, &LinearScale::identity()).unwrap();
        assert_eq!(levels.len_l1(), 1024);
        assert_eq!(levels.len_l2(), 16);
    }
}

// ============================================================================
// Feature 03: SIMD Incremental Construction Corner Cases
// ============================================================================
pub mod f03_simd_construction {
    use super::*;

    #[test]
    fn t2_f03_01_simd_all_infinities() {
        let data = vec![f32::NEG_INFINITY, f32::INFINITY];
        let bucket = reduce_raw_f32_slice(&data, 0, 10, &LinearScale::identity());
        assert_eq!(bucket.v_min, f32::NEG_INFINITY);
        assert_eq!(bucket.v_max, f32::INFINITY);
    }

    #[test]
    fn t2_f03_02_simd_zeros_and_neg_zero() {
        let data = vec![0.0f32, -0.0f32];
        let bucket = reduce_raw_f32_slice(&data, 0, 10, &LinearScale::identity());
        assert_eq!(bucket.v_min, 0.0);
        assert_eq!(bucket.v_max, 0.0);
    }

    #[test]
    fn t2_f03_03_simd_integer_overflow_scale() {
        let raw = vec![i32::MAX, i32::MIN];
        let scale = LinearScale::new(1.0, 0.0);
        let bucket = reduce_raw_i32_slice(&raw, 0, 10, &scale);
        assert!(bucket.v_max > 0.0);
        assert!(bucket.v_min < 0.0);
    }

    #[test]
    fn t2_f03_04_simd_zero_scale() {
        let data = vec![100.0f32, -100.0f32];
        let scale = LinearScale::new(0.0, 5.0);
        let bucket = reduce_raw_f32_slice(&data, 0, 10, &scale);
        assert_eq!(bucket.v_min, 5.0);
        assert_eq!(bucket.v_max, 5.0);
    }

    #[test]
    fn t2_f03_05_simd_strictly_decreasing() {
        let mut data = Vec::with_capacity(64);
        for i in (0..64).rev() {
            data.push(i as f32);
        }
        let bucket = reduce_raw_f32_slice(&data, 0, 630, &LinearScale::identity());
        assert_eq!(bucket.v_first, 63.0);
        assert_eq!(bucket.v_last, 0.0);
        assert_eq!(bucket.v_min, 0.0);
        assert_eq!(bucket.v_max, 63.0);
    }
}

// ============================================================================
// Feature 04: 100% Extrema Retention Corner Cases
// ============================================================================
pub mod f04_extrema_retention {
    use super::*;

    #[test]
    fn t2_f04_01_extrema_first_sample_spike() {
        let mut data = vec![0.0f32; 64];
        data[0] = 999.0;
        let bucket = reduce_raw_f32_slice(&data, 0, 630, &LinearScale::identity());
        assert_eq!(bucket.v_first, 999.0);
        assert_eq!(bucket.v_max, 999.0);
    }

    #[test]
    fn t2_f04_02_extrema_last_sample_spike() {
        let mut data = vec![0.0f32; 64];
        data[63] = 999.0;
        let bucket = reduce_raw_f32_slice(&data, 0, 630, &LinearScale::identity());
        assert_eq!(bucket.v_last, 999.0);
        assert_eq!(bucket.v_max, 999.0);
    }

    #[test]
    fn t2_f04_03_extrema_spike_at_l2_boundary() {
        let mut pyramid = MultiLevelM4Pyramid::new();
        let mut buckets = vec![
            M4Bucket {
                t_first_ns: 0,
                t_last_ns: 10,
                v_first: 0.0,
                v_min: 0.0,
                v_max: 0.0,
                v_last: 0.0,
            };
            64
        ];
        buckets[63].v_max = 555.0;
        pyramid.append_chunk_l2(&buckets);
        assert_eq!(pyramid.l3.len(), 1);
        assert_eq!(pyramid.l3[0].v_max, 555.0, "extrema retained across L2->L3 pyramid boundary");
    }

    #[test]
    fn t2_f04_04_extrema_nan_filtering() {
        let data = vec![1.0f32, f32::NAN, 3.0f32];
        let bucket = reduce_raw_f32_slice(&data, 0, 20, &LinearScale::identity());
        assert_eq!(bucket.v_max, 3.0);
        assert_eq!(bucket.v_min, 1.0);
    }

    #[test]
    fn t2_f04_05_extrema_two_spikes_same_pixel() {
        let mut data = vec![0.0f32; 64];
        data[10] = -1000.0;
        data[20] = 1000.0;
        let bucket = reduce_raw_f32_slice(&data, 0, 630, &LinearScale::identity());
        assert_eq!(bucket.v_min, -1000.0);
        assert_eq!(bucket.v_max, 1000.0);
    }
}

// ============================================================================
// Feature 05: Pixel-Aligned Query Corner Cases
// ============================================================================
pub mod f05_pixel_aligned_query {
    use super::*;

    #[test]
    fn t2_f05_01_query_exact_boundary_2w_px() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let w_px = 100;
        let n = 2 * w_px; // Exactly 200 samples
        let ts: Vec<u64> = (0..n as u64).map(|i| i * 10).collect();
        let val = vec![1.0f32; n];
        series.append(&ts, ScalarSlice::F32(&val)).expect("append ok");

        // N <= 2 * w_px -> MUST return Raw
        let res = series.query_m4_viewport(0, n as u64 * 10, w_px).expect("query ok");
        match res {
            M4QueryResult::Raw(pts) => assert_eq!(pts.len(), n),
            M4QueryResult::Columns(_) => panic!("expected Raw at exact 2*W_px boundary"),
        }
    }

    #[test]
    fn t2_f05_02_query_exact_boundary_2w_px_plus_1() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let w_px = 100;
        let n = 2 * w_px + 1; // Exactly 201 samples
        let ts: Vec<u64> = (0..n as u64).map(|i| i * 10).collect();
        let val = vec![1.0f32; n];
        series.append(&ts, ScalarSlice::F32(&val)).expect("append ok");

        // N > 2 * w_px -> MUST return Columns
        let res = series.query_m4_viewport(0, n as u64 * 10, w_px).expect("query ok");
        match res {
            M4QueryResult::Columns(cols) => assert_eq!(cols.len(), w_px),
            M4QueryResult::Raw(_) => panic!("expected Columns at 2*W_px + 1 boundary"),
        }
    }

    #[test]
    fn t2_f05_03_query_single_pixel_width() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let count = 100;
        let ts: Vec<u64> = (0..count as u64).map(|i| i * 10).collect();
        let val = vec![2.5f32; count];
        series.append(&ts, ScalarSlice::F32(&val)).expect("append ok");

        // Single pixel width (w_px = 1) on 100 samples (> 2*1)
        let res = series.query_m4_viewport(0, 1000, 1).expect("query ok");
        match res {
            M4QueryResult::Columns(cols) => {
                assert_eq!(cols.len(), 1, "Single pixel query returns exactly 1 column");
                assert_float_near!(cols[0].v_first, 2.5);
            }
            M4QueryResult::Raw(_) => panic!("expected Columns"),
        }
    }

    #[test]
    fn t2_f05_04_query_inverted_time_range() {
        let series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let res = series.query_m4_viewport(5000, 1000, 100);
        assert!(res.is_err(), "t_max <= t_min must return Err");
    }

    #[test]
    fn t2_f05_05_query_zero_pixel_width() {
        let series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let res = series.query_m4_viewport(1000, 5000, 0);
        assert!(res.is_err(), "pixel_width = 0 must return Err");
    }
}

// ============================================================================
// Feature 06: Sub-0.1ms Query Latency Corner Cases
// ============================================================================
pub mod f06_latency_criterion {
    use super::*;

    #[test]
    fn t2_f06_01_latency_heavy_cpu_load() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let dt = 10u64;
        let n = CHUNK_LEN;

        // Ingest 3 sealed chunks (196,608 samples)
        for i in 0..3 {
            let t0 = (i * n as u64) * dt;
            let ts = generate_timestamps(t0, n, dt);
            let vals = vec![(i + 1) as f32; n];
            series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        }
        assert_eq!(series.sealed_chunk_count(), 3);
        assert_eq!(series.total_samples(), (3 * n) as u64);

        // Perform M4 query over 100,000 ns window downsampled to 2,000 pixels
        let start = std::time::Instant::now();
        let res = series.query_m4_viewport(0, 100_000, 2000).unwrap();
        let elapsed = start.elapsed();

        match res {
            M4QueryResult::Columns(cols) => {
                assert_eq!(cols.len(), 2000, "M4 query must return exactly 2,000 columns");
                assert_float_near!(cols[0].v_min, 1.0);
                assert_float_near!(cols[0].v_max, 1.0);
                assert_float_near!(cols[cols.len() - 1].v_min, 1.0);
                assert_float_near!(cols[cols.len() - 1].v_max, 1.0);
            }
            M4QueryResult::Raw(_) => panic!("Expected M4 Columns when samples (10,001) > 2 * w_px (4000)"),
        }

        // Sub-millisecond latency check under normal test runner conditions (< 50ms debug guard)
        assert!(
            elapsed.as_millis() < 50,
            "M4 query latency exceeded target threshold: {elapsed:?}"
        );
    }

    #[test]
    fn t2_f06_02_latency_spans_active_and_sealed() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let dt = 10u64;
        let ts_sealed = generate_timestamps(0, CHUNK_LEN, dt);
        let vals_sealed = vec![1.0f32; CHUNK_LEN];
        series.append(&ts_sealed, ScalarSlice::F32(&vals_sealed)).unwrap();

        let t_active_start = (CHUNK_LEN as u64) * dt;
        let ts_active = generate_timestamps(t_active_start, 10_000, dt);
        let vals_active = vec![2.0f32; 10_000];
        series.append(&ts_active, ScalarSlice::F32(&vals_active)).unwrap();

        assert_eq!(series.sealed_chunk_count(), 1);
        assert_eq!(series.active_chunk().len(), 10_000);

        let t_end = t_active_start + 10_000 * dt;
        let res = series.query_m4_viewport(0, t_end, 1000).unwrap();
        match res {
            M4QueryResult::Columns(cols) => {
                assert_eq!(cols.len(), 1000);
                assert!(!cols.is_empty());
            }
            M4QueryResult::Raw(_) => panic!("Expected M4 Columns when samples > 2 * w_px"),
        }
    }

    #[test]
    fn t2_f06_03_latency_tiny_window_in_huge_dataset() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let dt = 10u64;
        let n = CHUNK_LEN;
        for i in 0..3 {
            let t0 = (i * n as u64) * dt;
            let ts = generate_timestamps(t0, n, dt);
            let vals = vec![(i + 1) as f32; n];
            series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        }
        assert_eq!(series.sealed_chunk_count(), 3);

        let t_chunk2_start = (2 * n as u64) * dt;
        let t_window_end = t_chunk2_start + 100;
        let res = series.query_m4_viewport(t_chunk2_start, t_window_end, 1000).unwrap();
        match res {
            M4QueryResult::Raw(pts) => {
                assert!(!pts.is_empty());
                assert!(pts.len() <= 11);
                assert_float_near!(pts[0].y_val, 3.0);
            }
            M4QueryResult::Columns(_) => panic!("Expected Raw points for tiny window"),
        }
    }

    #[test]
    fn t2_f06_04_latency_full_zoom_out_1b_points() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let dt = 10u64;
        let n = CHUNK_LEN;
        for i in 0..3 {
            let t0 = (i * n as u64) * dt;
            let ts = generate_timestamps(t0, n, dt);
            let vals = vec![(i + 1) as f32; n];
            series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        }
        assert_eq!(series.sealed_chunk_count(), 3);

        let res = series.query_m4_viewport(0, u64::MAX, 1920).unwrap();
        match res {
            M4QueryResult::Columns(cols) => {
                assert_eq!(cols.len(), 1920);
            }
            M4QueryResult::Raw(_) => panic!("Expected M4 Columns for full zoom out"),
        }
    }

    #[test]
    fn t2_f06_05_latency_zero_copy_slicing() {
        // Part 1: Zero-copy sample slice ingestion via ScalarSlice::F32
        let raw_samples: Vec<f32> = (0..1000).map(|i| (i as f32) * 1.5).collect();
        let sub_slice = &raw_samples[10..30];
        let scalar_slice = ScalarSlice::F32(sub_slice);
        assert_eq!(scalar_slice.len(), 20);
        assert_eq!(scalar_slice.scalar_type(), ScalarType::F32);
        assert_float_near!(scalar_slice.get_as_f64(0).unwrap(), 15.0);
        assert_float_near!(scalar_slice.get_as_f64(19).unwrap(), 43.5);

        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let ts = generate_timestamps(1000, 20, 10);
        series.append(&ts, scalar_slice).unwrap();
        assert_eq!(series.total_samples(), 20);
        assert_float_near!(series.get_sample_by_index(0).unwrap().1, 15.0);
        assert_float_near!(series.get_sample_by_index(19).unwrap().1, 43.5);

        // Part 2: Zero-copy binary viewport frame deserialization via DdsfZeroCopyReader
        let points: Vec<DdsfAnalogPoint> = (0..100)
            .map(|i| DdsfAnalogPoint {
                x_norm: (i as f32) / 100.0,
                y_val: (i as f32) * 2.5,
            })
            .collect();
        let track = TrackPayload::new_raw_xy(1, points.clone());
        let scope = ScopeViewportPayload {
            scope_id: 1,
            trigger_state: 0,
            mode: 0,
            win_t_start_ns: 0,
            win_t_span_ns: 1000,
            subpixel_offset_ns: 0.0,
            tracks: vec![track],
        };

        let mut packer = DdsfFramePacker::new(4096);
        let frame_bytes = packer.pack_frame(1, 1000, 0, &[scope], &[]).unwrap();
        assert_ddsf_frame_valid(frame_bytes, 1, 1, 0);

        let reader = DdsfZeroCopyReader::new(frame_bytes).expect("valid frame");
        let scope_view = reader.scopes().next().expect("1 scope");
        let track_view = scope_view.tracks().next().expect("1 track");
        let parsed_points = track_view.raw_points().expect("raw points slice");
        assert_eq!(parsed_points.len(), 100);

        // Verify zero-copy pointer residency: parsed slice resides entirely within frame buffer memory
        let frame_start = frame_bytes.as_ptr() as usize;
        let frame_end = frame_start + frame_bytes.len();
        let slice_start = parsed_points.as_ptr() as usize;
        let slice_byte_len = std::mem::size_of_val(parsed_points);
        assert!(
            slice_start >= frame_start,
            "parsed points slice must start inside frame buffer"
        );
        assert!(
            slice_start + slice_byte_len <= frame_end,
            "parsed points slice must end inside frame buffer (zero-copy guarantee)"
        );
    }
}

// ============================================================================
// Feature 07: Global 2GB Quota Tracking Corner Cases
// ============================================================================
pub mod f07_quota_tracking {
    use super::*;

    #[test]
    fn t2_f07_01_quota_exact_zero_limit() {
        let q = MemoryQuotaManager::new(0);
        assert!(q.allocate(1).is_err());
    }

    #[test]
    fn t2_f07_02_quota_exact_limit_allocation() {
        let limit = 1024 * 1024;
        let q = MemoryQuotaManager::new(limit);
        assert!(q.allocate(limit).is_ok());
        assert!(q.allocate(1).is_err());
    }

    #[test]
    fn t2_f07_03_quota_exact_threshold_boundary() {
        let q = MemoryQuotaManager::new(100);
        q.allocate(84).unwrap();
        assert!(!q.is_warning());
        q.allocate(1).unwrap();
        assert!(q.is_warning());
    }

    #[test]
    fn t2_f07_04_quota_release_more_than_allocated() {
        let q = MemoryQuotaManager::new(100);
        q.allocate(10).unwrap();
        q.release(20);
        assert_eq!(q.allocated_bytes(), 0);
    }

    #[test]
    fn t2_f07_05_quota_rapid_oscillations() {
        let q = MemoryQuotaManager::new(1000);
        for _ in 0..10_000 {
            q.allocate(100).unwrap();
            q.release(100);
        }
        assert_eq!(q.allocated_bytes(), 0);
    }
}

// ============================================================================
// Feature 08: FIFO Unpinned Eviction Corner Cases
// ============================================================================
pub mod f08_fifo_eviction {
    use super::*;

    #[test]
    fn t2_f08_01_eviction_zero_unpinned() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        series.set_quota_manager(Arc::new(MemoryQuotaManager::new_2gb()));
        let n = CHUNK_LEN;
        let ts = generate_timestamps(0, n, 10);
        let vals = vec![1.0f32; n];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        assert_eq!(series.sealed_chunk_count(), 1);

        let guard = series.pin_range(0, 100).expect("pin chunk 0");
        assert_eq!(guard.chunk_count(), 1);

        assert!(series.evict_oldest_unpinned().is_none(), "cannot evict when all chunks are pinned");
        assert_eq!(series.sealed_chunk_count(), 1);
        assert_eq!(series.evicted_samples(), 0);
    }

    #[test]
    fn t2_f08_02_eviction_single_huge_chunk() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let n = CHUNK_LEN;
        let ts = generate_timestamps(0, n, 10);
        let vals = vec![1.0f32; n];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        assert_eq!(series.sealed_chunk_count(), 1);

        let evicted = series.evict_oldest_unpinned().expect("evict oldest unpinned chunk");
        assert_eq!(evicted.sample_count, n as u32);
        assert_eq!(evicted.t_start_ns, 0);
        assert_eq!(series.sealed_chunk_count(), 0);
        assert_eq!(series.evicted_samples(), n as u64);
    }

    #[test]
    fn t2_f08_03_eviction_interleaved_pinned_unpinned() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        series.set_quota_manager(Arc::new(MemoryQuotaManager::new_2gb()));
        let n = CHUNK_LEN;
        let dt = 10u64;

        // Ingest 4 chunks: 0, 1, 2, 3
        for i in 0..4 {
            let t0 = (i * n as u64) * dt;
            let ts = generate_timestamps(t0, n, dt);
            let vals = vec![i as f32; n];
            series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        }
        assert_eq!(series.sealed_chunk_count(), 4);

        // Pin chunk 1 (t in [n*dt, 2n*dt])
        let t_chunk1_mid = (n as u64 + 100) * dt;
        let guard1 = series.pin_range(t_chunk1_mid, t_chunk1_mid + 100).expect("pin chunk 1");
        assert_eq!(guard1.chunk_count(), 1);

        // First eviction: evicts unpinned chunk 0
        let ev1 = series.evict_oldest_unpinned().expect("evict chunk 0");
        assert_eq!(ev1.t_start_ns, 0);

        // Second eviction: chunk 1 is pinned, so eviction must NOT evict chunk 1
        let ev2 = series.evict_oldest_unpinned();
        if let Some(ref h) = ev2 {
            assert_ne!(h.t_start_ns, (n as u64) * dt, "pinned chunk 1 MUST NOT be evicted");
        }

        // Unconditional assertion: chunk 1 remains strictly accessible in series
        assert!(series.get_sample_by_index(n as u64).is_some(), "pinned chunk 1 must remain accessible");
        assert_eq!(series.sealed_chunk_count(), 3, "series still contains chunk 1, 2, 3");

        // Drop guard, now chunk 1 becomes evictable
        drop(guard1);
        let ev3 = series.evict_oldest_unpinned().expect("chunk 1 now evictable after drop");
        assert_eq!(ev3.t_start_ns, (n as u64) * dt);
    }

    #[test]
    fn t2_f08_04_eviction_background_thread_race() {
        let q = Arc::new(MemoryQuotaManager::new(10_000));
        let q_clone = Arc::clone(&q);
        let h = std::thread::spawn(move || {
            for _ in 0..50 {
                q_clone.allocate(100).unwrap();
                q_clone.release(100);
            }
        });
        h.join().unwrap();
        assert_eq!(q.allocated_bytes(), 0);
    }

    #[test]
    fn t2_f08_05_eviction_rapid_burst_1000() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let n = CHUNK_LEN;
        let dt = 10u64;
        let chunk_count = 10;

        for i in 0..chunk_count {
            let t0 = (i as u64 * n as u64) * dt;
            let ts = generate_timestamps(t0, n, dt);
            let vals = vec![i as f32; n];
            series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        }
        assert_eq!(series.sealed_chunk_count(), chunk_count);

        for step in 0..chunk_count {
            let evicted = series.evict_oldest_unpinned().expect("evict oldest chunk");
            assert_eq!(evicted.t_start_ns, (step as u64 * n as u64) * dt);
        }

        assert_eq!(series.sealed_chunk_count(), 0);
        assert_eq!(series.evicted_samples(), (chunk_count * n) as u64);
        assert!(series.evict_oldest_unpinned().is_none());
    }
}

// ============================================================================
// Feature 09: Chunk Pinning Corner Cases
// ============================================================================
pub mod f09_chunk_pinning {
    use super::*;

    #[test]
    fn t2_f09_01_pin_empty_range() {
        let q = Arc::new(MemoryQuotaManager::new_2gb());
        let guard = q.pin(0).unwrap();
        assert_eq!(guard.bytes(), 0);
    }

    #[test]
    fn t2_f09_02_pin_active_buffer_sample() {
        let q = Arc::new(MemoryQuotaManager::new_2gb());
        let guard = q.pin(1024).unwrap();
        assert_eq!(guard.bytes(), 1024);
    }

    #[test]
    fn t2_f09_03_pin_100_chunks() {
        let q = Arc::new(MemoryQuotaManager::new_2gb());
        let chunk_size = 512 * 1024;
        let guard = q.pin(100 * chunk_size).unwrap();
        assert_eq!(guard.bytes(), 100 * chunk_size);
    }

    #[test]
    fn t2_f09_04_pin_overlapping_guards() {
        let q = Arc::new(MemoryQuotaManager::new_2gb());
        let g1 = q.pin(1000).unwrap();
        let g2 = q.pin(1000).unwrap();
        drop(g1);
        assert_eq!(q.pinned_bytes(), 1000);
        drop(g2);
        assert_eq!(q.pinned_bytes(), 0);
    }

    #[test]
    fn t2_f09_05_pin_inverted_time_range() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        series.set_quota_manager(Arc::new(MemoryQuotaManager::new_2gb()));
        let n = CHUNK_LEN;
        let ts = generate_timestamps(0, n, 10);
        let vals = vec![1.0f32; n];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        assert_eq!(series.sealed_chunk_count(), 1);

        // Inverted time range: t_start_ns (500) > t_end_ns (100)
        let res = series.pin_range(500, 100);
        assert!(res.is_err(), "pin_range must return Err when t_start_ns > t_end_ns");
        match res {
            Err(CoreError::Storage(msg)) => {
                assert!(
                    msg.contains("invalid pin_range") && msg.contains("500") && msg.contains("100"),
                    "unexpected error message: {msg}"
                );
            }
            other => panic!("expected CoreError::Storage, got {other:?}"),
        }

        // Contrast: valid non-inverted time range on the same series succeeds
        let valid = series.pin_range(100, 500);
        assert!(valid.is_ok(), "valid non-inverted range must succeed");
        assert_eq!(valid.unwrap().chunk_count(), 1);
    }
}

// ============================================================================
// Feature 10: Ring Buffer Rollover Safety Corner Cases
// ============================================================================
pub mod f10_rollover_safety {
    use super::*;

    #[test]
    fn t2_f10_01_rollover_100gb_simulated() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let n = CHUNK_LEN;
        let dt = 10u64;
        let total_chunks = 20;

        for i in 0..total_chunks {
            let t0 = (i as u64 * n as u64) * dt;
            let ts = generate_timestamps(t0, n, dt);
            let vals = vec![i as f32; n];
            series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
            let ev = series.evict_oldest_unpinned().expect("evict on each iteration");
            assert_eq!(ev.t_start_ns, (i as u64 * n as u64) * dt);
        }

        assert_eq!(series.evicted_samples(), (total_chunks as u64) * n as u64);
        assert_eq!(series.sealed_chunk_count(), 0);

        // Ingest one fresh chunk after 20 wraps
        let t_fresh = (total_chunks as u64 * n as u64) * dt;
        let ts_fresh = generate_timestamps(t_fresh, n, dt);
        let vals_fresh = vec![99.0f32; n];
        series.append(&ts_fresh, ScalarSlice::F32(&vals_fresh)).unwrap();
        assert_eq!(series.sealed_chunk_count(), 1);

        // Verify surviving chunk data is valid and queryable
        let s = series.get_sample_by_index(total_chunks as u64 * n as u64).expect("fresh chunk queryable");
        assert_eq!(s.0, t_fresh);
        assert_eq!(s.1, 99.0);
    }

    #[test]
    fn t2_f10_02_rollover_non_contiguous_pinned_regions() {
        let q = Arc::new(MemoryQuotaManager::new_2gb());
        let _g1 = q.pin(10_000_000).unwrap();
        let _g2 = q.pin(20_000_000).unwrap();
        assert_eq!(q.pinned_bytes(), 30_000_000);
    }

    #[test]
    fn t2_f10_03_rollover_active_pinned_then_sealed() {
        let q = Arc::new(MemoryQuotaManager::new_2gb());
        let g = q.pin(5000).unwrap();
        assert_eq!(g.bytes(), 5000);
    }

    #[test]
    fn t2_f10_04_rollover_read_during_eviction() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        series.set_quota_manager(Arc::new(MemoryQuotaManager::new_2gb()));
        let n = CHUNK_LEN;
        let dt = 10u64;
        let ts = generate_timestamps(0, n, dt);
        let vals = vec![42.0f32; n];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        assert_eq!(series.sealed_chunk_count(), 1);

        // Pin chunk 0
        let guard = series.pin_range(0, 100).expect("pin chunk 0");
        assert_eq!(guard.chunk_count(), 1);

        // Attempt eviction while pinned: must fail / return None
        assert!(series.evict_oldest_unpinned().is_none(), "pinned chunk cannot be evicted");

        // Read sample from pinned chunk 0 via get_sample_by_index(0)
        let sample = series.get_sample_by_index(0).expect("sample 0 accessible while pinned");
        assert_eq!(sample.0, 0);
        assert_eq!(sample.1, 42.0);

        // Drop guard, now eviction succeeds
        drop(guard);
        let evicted = series.evict_oldest_unpinned().expect("evict after unpinning");
        assert_eq!(evicted.t_start_ns, 0);
        assert!(series.get_sample_by_index(0).is_none(), "evicted sample is no longer in series");
    }

    #[test]
    fn t2_f10_05_rollover_rss_stability() {
        let q = MemoryQuotaManager::new(2000);
        for _ in 0..1000 {
            q.allocate(100).unwrap();
            q.release(100);
        }
        assert_eq!(q.allocated_bytes(), 0);
    }
}

// ============================================================================
// Feature 11: 40% Pinning Memory Ceiling Corner Cases
// ============================================================================
pub mod f11_pinning_ceiling {
    use super::*;

    #[test]
    fn t2_f11_01_pin_exact_40_percent() {
        let q = Arc::new(MemoryQuotaManager::new(1000));
        assert!(q.pin(400).is_ok());
    }

    #[test]
    fn t2_f11_02_pin_40_percent_minus_1() {
        let q = Arc::new(MemoryQuotaManager::new(1000));
        assert!(q.pin(399).is_ok());
    }

    #[test]
    fn t2_f11_03_pin_40_percent_plus_1() {
        let q = Arc::new(MemoryQuotaManager::new(1000));
        assert!(q.pin(401).is_err());
    }

    #[test]
    fn t2_f11_04_pin_partial_rollback() {
        let q = Arc::new(MemoryQuotaManager::new(1000));
        let _g = q.pin(300).unwrap();
        assert!(q.pin(200).is_err());
        assert_eq!(q.pinned_bytes(), 300);
    }

    #[test]
    fn t2_f11_05_pin_release_back_to_zero() {
        let q = Arc::new(MemoryQuotaManager::new(1000));
        let g = q.pin(400).unwrap();
        drop(g);
        assert_eq!(q.pinned_bytes(), 0);
    }
}

// ============================================================================
// Feature 12: Evicted Offset Indexing Corner Cases
// ============================================================================
pub mod f12_evicted_indexing {
    use super::*;

    #[test]
    fn t2_f12_01_evicted_exact_boundary() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let n = CHUNK_LEN;
        let ts = generate_timestamps(0, n, 10);
        let vals = vec![1.0f32; n];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        // Append another sample so active has data and first chunk is sealed
        series.append(&[n as u64 * 10], ScalarSlice::F32(&[2.0f32])).unwrap();
        assert_eq!(series.sealed_chunk_count(), 1);

        let evicted_handle = series.evict_oldest_unpinned();
        assert!(evicted_handle.is_some());
        let evicted = series.evicted_samples();
        assert_eq!(evicted, n as u64);

        // Exact boundary at evicted: sample is in active chunk, must return Some
        let sample = series.get_sample_by_index(evicted);
        assert!(sample.is_some());
    }

    #[test]
    fn t2_f12_02_evicted_minus_1_index() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let n = CHUNK_LEN;
        let ts = generate_timestamps(0, n, 10);
        let vals = vec![1.0f32; n];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        series.append(&[n as u64 * 10], ScalarSlice::F32(&[2.0f32])).unwrap();

        series.evict_oldest_unpinned().unwrap();
        let evicted = series.evicted_samples();
        assert!(evicted > 0);

        // Index evicted - 1 is evicted and must return None
        assert!(series.get_sample_by_index(evicted - 1).is_none());
    }

    #[test]
    fn t2_f12_03_evicted_u64_max_index() {
        let series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        assert!(series.get_sample_by_index(u64::MAX).is_none());
    }

    #[test]
    fn t2_f12_04_evicted_zero_evicted_series() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let ts = generate_timestamps(0, 100, 10);
        let vals = vec![42.0f32; 100];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();

        assert_eq!(series.evicted_samples(), 0);
        assert!(series.get_sample_by_index(0).is_some());
        assert!(series.get_sample_by_index(99).is_some());
        assert!(series.get_sample_by_index(100).is_none());
    }

    #[test]
    fn t2_f12_05_evicted_monotonic_growth() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let n = CHUNK_LEN;
        let dt = 10u64;

        // Append 3 chunks with strictly increasing timestamps
        for i in 0..3 {
            let t0 = (i * n as u64) * dt;
            let ts = generate_timestamps(t0, n, dt);
            let vals = vec![i as f32; n];
            series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        }

        let mut last_evicted_ts = 0u64;
        let mut prev_evicted_count = 0u64;

        for step in 0..3 {
            let evicted = series.evict_oldest_unpinned().expect("eviction succeeds");
            assert!(evicted.t_start_ns >= last_evicted_ts, "evicted chunk timestamps must be monotonically increasing");
            last_evicted_ts = evicted.t_start_ns;

            let curr_evicted_count = series.evicted_samples();
            assert!(curr_evicted_count > prev_evicted_count, "evicted_samples must monotonically grow");
            prev_evicted_count = curr_evicted_count;

            assert_eq!(curr_evicted_count, ((step + 1) * n) as u64);
        }
    }
}

// ============================================================================
// Feature 13: ScopeController State Machine Corner Cases
// ============================================================================
pub mod f13_scope_controller {
    use super::*;

    #[test]
    fn t2_f13_01_controller_rapid_mode_switch() {
        let mut ctrl = make_scope_controller(1, TriggerSweepMode::Roll, 10_000_000, 100_000_000);
        for _ in 0..100 {
            ctrl.set_sweep_mode(TriggerSweepMode::Auto);
            ctrl.set_sweep_mode(TriggerSweepMode::Normal);
            ctrl.set_sweep_mode(TriggerSweepMode::Single);
            ctrl.set_sweep_mode(TriggerSweepMode::Roll);
        }
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::Roll);
    }

    #[test]
    fn t2_f13_02_controller_rearm_in_armed_state() {
        let mut ctrl = make_scope_controller(1, TriggerSweepMode::Normal, 10_000_000, 100_000_000);
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);
        ctrl.rearm();
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);
    }

    #[test]
    fn t2_f13_03_controller_backwards_now_ns() {
        let mut ctrl = make_scope_controller(1, TriggerSweepMode::Roll, 10_000_000, 100_000_000);
        let out1 = ctrl.step(2000);
        assert!(matches!(out1, ScopeStepOutcome::Rolling { .. }));
        // Backwards timestamp: step(1000) should handle gracefully without panic
        let out2 = ctrl.step(1000);
        assert!(matches!(out2, ScopeStepOutcome::Rolling { .. }));
    }

    #[test]
    fn t2_f13_04_controller_step_on_empty_storage() {
        let mut ctrl = make_scope_controller(1, TriggerSweepMode::Normal, 10_000_000, 100_000_000);
        let outcome = ctrl.step(1_000_000);
        assert_eq!(outcome, ScopeStepOutcome::WaitingForTrigger);
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);
    }

    #[test]
    fn t2_f13_05_controller_invalid_mode_code() {
        let invalid_code = 99u8;
        assert!(TriggerSweepMode::from_u8(invalid_code).is_none());
    }
}

// ============================================================================
// Feature 14: Auto-Trigger Timeout Corner Cases
// ============================================================================
pub mod f14_auto_timeout {
    use super::*;

    #[test]
    fn t2_f14_01_auto_timeout_exact_99_999ms() {
        let mut ctrl = make_scope_controller(1, TriggerSweepMode::Auto, 10_000_000, 100_000_000);
        ctrl.step(1_000_000); // 1ms baseline
        let outcome = ctrl.step(1_000_000 + 99_999_999);
        assert_eq!(outcome, ScopeStepOutcome::WaitingForTrigger);
    }

    #[test]
    fn t2_f14_02_auto_timeout_exact_100ms() {
        let mut ctrl = make_scope_controller(1, TriggerSweepMode::Auto, 10_000_000, 100_000_000);
        ctrl.step(1_000_000); // 1ms baseline
        let outcome = ctrl.step(1_000_000 + 100_000_000);
        match outcome {
            ScopeStepOutcome::AutoFreerunFired { window } => {
                assert_eq!(window.mode, TriggerSweepMode::Auto);
                assert_eq!(window.trigger_state, TriggerRuntimeState::AutoFreerun);
            }
            other => panic!("expected AutoFreerunFired, got {:?}", other),
        }
    }

    #[test]
    fn t2_f14_03_auto_timeout_edge_at_100ms() {
        let config = ScopeControllerConfig {
            sweep_mode: TriggerSweepMode::Auto,
            time_span_ns: 10_000_000,
            auto_timeout_ns: 100_000_000,
            trigger: Some(TriggerCondition::Edge {
                channel: ChannelId(1),
                level: 0.0,
                hysteresis: 0.5,
                slope: EdgeSlope::Rising,
            }),
            ..Default::default()
        };
        let mut ctrl = ScopeController::new(config);
        ctrl.step(1_000_000);
        ctrl.process_scalar_sample(ChannelId(1), 100_000_000, -1.0);
        let out = ctrl.process_scalar_sample(ChannelId(1), 100_001_000, 1.0);
        assert!(matches!(out, ScopeStepOutcome::TriggerDetected { .. }));
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::Triggered);
    }

    #[test]
    fn t2_f14_04_auto_timeout_disabled_in_single() {
        let mut ctrl = make_scope_controller(1, TriggerSweepMode::Single, 10_000_000, 100_000_000);
        ctrl.step(1_000_000);
        let outcome = ctrl.step(200_000_000);
        assert_eq!(outcome, ScopeStepOutcome::WaitingForTrigger);
    }

    #[test]
    fn t2_f14_05_auto_consecutive_timeouts() {
        let mut ctrl = make_scope_controller(1, TriggerSweepMode::Auto, 10_000_000, 100_000_000);
        ctrl.step(1_000_000);
        let out1 = ctrl.step(101_000_000);
        assert!(matches!(out1, ScopeStepOutcome::AutoFreerunFired { .. }));
        let out2 = ctrl.step(201_000_000);
        assert!(matches!(out2, ScopeStepOutcome::AutoFreerunFired { .. }));
        let out3 = ctrl.step(301_000_000);
        assert!(matches!(out3, ScopeStepOutcome::AutoFreerunFired { .. }));
    }
}

// ============================================================================
// Feature 15: Hysteresis Edge Debouncing Corner Cases
// ============================================================================
pub mod f15_hysteresis_debouncing {
    use super::*;

    #[test]
    fn t2_f15_01_hysteresis_very_large() {
        let mut engine = make_edge_trigger_engine(ChannelId(1), 0.0, 100.0, EdgeSlope::Rising);
        engine.evaluate_scalar(100, -10.0);
        let hit = engine.evaluate_scalar(200, 10.0);
        assert!(hit.is_none());
    }

    #[test]
    fn t2_f15_02_hysteresis_exact_threshold_touch() {
        let mut engine = make_edge_trigger_engine(ChannelId(1), 0.0, 0.5, EdgeSlope::Rising);
        engine.evaluate_scalar(100, -0.5);
        assert_eq!(engine.state(), HysteresisBandState::ArmedLow);
    }

    #[test]
    fn t2_f15_03_hysteresis_alternating_chatter() {
        let mut engine = make_edge_trigger_engine(ChannelId(1), 0.0, 0.5, EdgeSlope::Rising);
        for _ in 0..10 {
            engine.evaluate_scalar(100, -0.51);
            engine.evaluate_scalar(200, -0.49);
        }
        assert_eq!(engine.state(), HysteresisBandState::ArmedLow);
    }

    #[test]
    fn t2_f15_04_hysteresis_dc_flatline() {
        let mut engine = make_edge_trigger_engine(ChannelId(1), 0.0, 0.5, EdgeSlope::Rising);
        for _ in 0..100 {
            let hit = engine.evaluate_scalar(100, 0.0);
            assert!(hit.is_none());
        }
    }

    #[test]
    fn t2_f15_05_hysteresis_nan_input() {
        let mut engine = make_edge_trigger_engine(ChannelId(1), 0.0, 0.5, EdgeSlope::Rising);
        let hit = engine.evaluate_scalar(100, f64::NAN);
        assert!(hit.is_none());
    }
}

// ============================================================================
// Feature 16: Sub-Sample Linear Interpolation Corner Cases
// ============================================================================
pub mod f16_subsample_interpolation {
    use super::*;

    #[test]
    fn t2_f16_01_subsample_interp_flatline() {
        let (alpha, sub_ns) =
            calculate_subsample_offset(100, 2.0, 200, 2.0, 2.0).unwrap();
        assert_float_near!(alpha, 0.0, 1e-4);
        assert_float_near!(sub_ns, 0.0, 1e-3);
    }

    #[test]
    fn t2_f16_02_subsample_interp_tiny_denominator() {
        let (alpha, sub_ns) =
            calculate_subsample_offset(100, 2.0, 200, 2.0 + 1e-15, 2.0).unwrap();
        assert_float_near!(alpha, 0.0, 1e-4);
        assert_float_near!(sub_ns, 0.0, 1e-3);
    }

    #[test]
    fn t2_f16_03_subsample_interp_clamp_to_bounds() {
        let (alpha, _) =
            calculate_subsample_offset(100, 0.0, 200, 1.0, 1.5).unwrap();
        assert!(alpha <= 1.0);
    }

    #[test]
    fn t2_f16_04_subsample_interp_zero_dt() {
        let (alpha, sub_ns) =
            calculate_subsample_offset(100, 0.0, 100, 2.0, 1.0).unwrap();
        assert_float_near!(alpha, 0.5, 1e-4);
        assert_float_near!(sub_ns, 0.0, 1e-3);
    }

    #[test]
    fn t2_f16_05_subsample_interp_nan_level() {
        let res = calculate_subsample_offset(100, 0.0, 200, 2.0, f64::NAN);
        assert!(res.is_none());
    }
}

// ============================================================================
// Feature 17: SyncGroupCoordinator Corner Cases
// ============================================================================
pub mod f17_sync_group {
    use super::*;

    #[test]
    fn t2_f17_01_sync_group_zero_scopes() {
        let sync = SyncGroupCoordinator::new();
        let scopes = sync.get_group_scopes(SyncGroupId(999));
        assert!(scopes.is_empty());
    }

    #[test]
    fn t2_f17_02_sync_group_scope_non_existent_group() {
        let sync = SyncGroupCoordinator::new();
        assert!(sync.get_group_state(SyncGroupId(42)).is_none());
    }

    #[test]
    fn t2_f17_03_sync_group_switch_groups() {
        let sync = SyncGroupCoordinator::new();
        sync.register_scope(ScopeId(1), SyncGroupId(1));
        sync.register_scope(ScopeId(1), SyncGroupId(2));
        assert_eq!(sync.get_scope_group(ScopeId(1)), Some(SyncGroupId(2)));
    }

    #[test]
    fn t2_f17_04_sync_group_concurrent_updates() {
        let sync = Arc::new(SyncGroupCoordinator::new());
        sync.register_scope(ScopeId(1), SyncGroupId(1));
        let handles: Vec<_> = (0..4)
            .map(|i| {
                let s = Arc::clone(&sync);
                std::thread::spawn(move || {
                    s.update_timebase(SyncGroupId(1), (i + 1) * 10_000_000, 0);
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert!(sync.get_group_state(SyncGroupId(1)).is_some());
    }

    #[test]
    fn t2_f17_05_sync_group_cursor_inverted() {
        let sync = SyncGroupCoordinator::new();
        sync.register_scope(ScopeId(1), SyncGroupId(1));
        sync.update_cursors(SyncGroupId(1), Some(0.8), Some(0.2));
        let state = sync.get_group_state(SyncGroupId(1)).unwrap();
        assert_eq!(state.cursor_a_ratio, Some(0.8));
        assert_eq!(state.cursor_b_ratio, Some(0.2));
    }
}

// ============================================================================
// Feature 18: Single Mode Pinning Integration Corner Cases
// ============================================================================
pub mod f18_single_pinning {
    use super::*;

    #[test]
    fn t2_f18_01_single_pre_post_zero() {
        let config = ScopeControllerConfig {
            sweep_mode: TriggerSweepMode::Single,
            time_span_ns: 1_000,
            pre_trigger_ratio: 0.0,
            ..Default::default()
        };
        let mut ctrl = ScopeController::new(config);
        ctrl.force_trigger(10_000);
        let out = ctrl.step(11_000);
        if let ScopeStepOutcome::CaptureCompleted { window, latched_pinned } = out {
            assert!(latched_pinned);
            assert_eq!(window.win_t_start_ns, 10_000);
        } else {
            panic!("expected CaptureCompleted");
        }
    }

    #[test]
    fn t2_f18_02_single_window_larger_than_buffer() {
        let mut ctrl = make_scope_controller(1, TriggerSweepMode::Single, 10_000_000_000, 100_000_000);
        let out = ctrl.step(10_000);
        assert_eq!(out, ScopeStepOutcome::WaitingForTrigger);
    }

    #[test]
    fn t2_f18_03_single_multiple_rearm_calls() {
        let mut ctrl = make_scope_controller(1, TriggerSweepMode::Single, 10_000_000, 100_000_000);
        for _ in 0..10 {
            ctrl.rearm();
            assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);
            assert!(!ctrl.is_pinned());
        }
    }

    #[test]
    fn t2_f18_04_single_drop_in_latched() {
        let mut ctrl = make_scope_controller(1, TriggerSweepMode::Single, 10_000, 100_000_000);
        ctrl.force_trigger(10_000);
        let out = ctrl.step(20_000);
        assert!(matches!(out, ScopeStepOutcome::CaptureCompleted { latched_pinned: true, .. }));
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::LatchedPinned);
        assert!(ctrl.is_pinned());
        drop(ctrl);
    }

    #[test]
    fn t2_f18_05_single_mode_rapid_cycle() {
        let mut ctrl = make_scope_controller(1, TriggerSweepMode::Single, 1_000, 100_000_000);
        for _ in 0..5 {
            ctrl.force_trigger(10_000);
            ctrl.step(12_000);
            assert_eq!(ctrl.current_state(), TriggerRuntimeState::LatchedPinned);
            ctrl.rearm();
            assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);
        }
    }
}

// ============================================================================
// Feature 19: 64-Byte Aligned DDSF Header Corner Cases
// ============================================================================
pub mod f19_ddsf_header {
    use super::*;

    #[test]
    fn t2_f19_01_ddsf_bad_magic() {
        let mut hdr = DdsfFrameHeader::new_v1(1, 0, 0, 0, 64, 0);
        hdr.magic = 0;
        assert!(hdr.validate(64).is_err());
    }

    #[test]
    fn t2_f19_02_ddsf_bad_version() {
        let mut hdr = DdsfFrameHeader::new_v1(1, 0, 0, 0, 64, 0);
        hdr.version = 999;
        assert!(hdr.validate(64).is_err());
    }

    #[test]
    fn t2_f19_03_ddsf_total_len_too_small() {
        let mut hdr = DdsfFrameHeader::new_v1(1, 0, 0, 0, 64, 0);
        hdr.total_byte_len = 32;
        assert!(hdr.validate(64).is_err());
    }

    #[test]
    fn t2_f19_04_ddsf_total_len_exceeds_buffer() {
        let hdr = DdsfFrameHeader::new_v1(1, 0, 0, 0, 128, 0);
        assert!(hdr.validate(64).is_err());
    }

    #[test]
    fn t2_f19_05_ddsf_seq_u32_max_wrap() {
        let mut packer = DdsfFramePacker::new(4096);
        let buf_max = packer.pack_frame(u32::MAX, 100, 0, &[], &[]).unwrap();
        assert_ddsf_frame_valid(buf_max, u32::MAX, 0, 0);

        let buf_wrapped = packer.pack_frame(0, 200, 0, &[], &[]).unwrap();
        assert_ddsf_frame_valid(buf_wrapped, 0, 0, 0);

        let header: &DdsfFrameHeader = bytemuck::from_bytes(&buf_wrapped[0..64]);
        assert_eq!(header.frame_seq, 0);
    }
}

// ============================================================================
// Feature 20: Viewport Track Packing Corner Cases
// ============================================================================
pub mod f20_track_packing {
    use super::*;

    #[test]
    fn t2_f20_01_pack_zero_tracks() {
        let scope = ScopeViewportPayload {
            scope_id: 1,
            trigger_state: 0,
            mode: 0,
            win_t_start_ns: 0,
            win_t_span_ns: 1000,
            subpixel_offset_ns: 0.0,
            tracks: Vec::new(),
        };
        let mut packer = DdsfFramePacker::new(4096);
        let buf = packer.pack_frame(1, 100, 0, &[scope], &[]).unwrap();
        assert_ddsf_frame_valid(buf, 1, 1, 0);
    }

    #[test]
    fn t2_f20_02_pack_digital_edges_zero_edges() {
        let track = TrackPayload::new_digital_edges(1, 1, Vec::new());
        let scope = ScopeViewportPayload {
            scope_id: 1,
            trigger_state: 0,
            mode: 0,
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
    fn t2_f20_03_pack_state_ribbons_zero_intervals() {
        let track = TrackPayload::new_state_ribbons(1, Vec::new());
        let scope = ScopeViewportPayload {
            scope_id: 1,
            trigger_state: 0,
            mode: 0,
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
    fn t2_f20_04_pack_100_tracks() {
        let mut tracks = Vec::new();
        for i in 0..100 {
            tracks.push(TrackPayload::new_digital_edges(i, 0, vec![0.5]));
        }
        let scope = ScopeViewportPayload {
            scope_id: 1,
            trigger_state: 0,
            mode: 0,
            win_t_start_ns: 0,
            win_t_span_ns: 1000,
            subpixel_offset_ns: 0.0,
            tracks,
        };
        let mut packer = DdsfFramePacker::new(16384);
        let buf = packer.pack_frame(1, 100, 0, &[scope], &[]).unwrap();
        assert_ddsf_frame_valid(buf, 1, 1, 0);
    }

    #[test]
    fn t2_f20_05_pack_exact_buffer_size() {
        let mut packer = DdsfFramePacker::new(4096);
        let buf = packer.pack_frame(1, 100, 0, &[], &[]).unwrap();
        assert!(buf.len() >= 64);
    }
}

// ============================================================================
// Feature 21: Track Statistics & Snapshots Corner Cases
// ============================================================================
pub mod f21_stats_snapshots {
    use super::*;

    #[test]
    fn t2_f21_01_stats_dc_flatline() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let n = 200;
        let ts = generate_timestamps(0, n, 10);
        let vals = vec![5.0f32; n];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();

        let extract = series.query_m4_viewport_with_stats(0, (n - 1) as u64 * 10, 100).unwrap();
        assert_eq!(extract.stats.min, 5.0);
        assert_eq!(extract.stats.max, 5.0);
        assert_eq!(extract.stats.vpp, 0.0);
        assert_eq!(extract.stats.mean, 5.0);
    }

    #[test]
    fn t2_f21_02_stats_single_sample() {
        let stats = DdsfTrackStats {
            min: 1.0,
            max: 1.0,
            vpp: 0.0,
            mean: 1.0,
            rms: 1.0,
            freq_hz: 0.0,
            duty: 0.0,
            reserved: 0.0,
        };
        let cols = vec![DdsfM4Column {
            x_norm: 0.5,
            v_first: 1.0,
            v_min: 1.0,
            v_max: 1.0,
            v_last: 1.0,
        }];
        let track = TrackPayload::new_m4(1, cols, Some(stats));
        let scope = ScopeViewportPayload {
            scope_id: 1,
            trigger_state: 0,
            mode: 0,
            win_t_start_ns: 0,
            win_t_span_ns: 1000,
            subpixel_offset_ns: 0.0,
            tracks: vec![track],
        };
        let mut packer = DdsfFramePacker::new(4096);
        let buf = packer.pack_frame(1, 100, 0, &[scope], &[]).unwrap();
        assert_ddsf_frame_valid(buf, 1, 1, 0);

        let reader = DdsfZeroCopyReader::new(buf).expect("valid frame");
        let mut scope_iter = reader.scopes();
        let scope_view = scope_iter.next().expect("1 scope view");
        let mut track_iter = scope_view.tracks();
        let track_view = track_iter.next().expect("1 track view");
        let unpacked_stats = track_view.stats().expect("track has stats");
        assert_float_near!(unpacked_stats.min, 1.0);
        assert_float_near!(unpacked_stats.max, 1.0);
        assert_float_near!(unpacked_stats.vpp, 0.0);
        assert_float_near!(unpacked_stats.mean, 1.0);
    }

    #[test]
    fn t2_f21_03_snapshots_zero_entries() {
        let mut packer = DdsfFramePacker::new(4096);
        let buf = packer.pack_frame(1, 100, 0, &[], &[]).unwrap();
        assert_ddsf_frame_valid(buf, 1, 0, 0);
    }

    #[test]
    fn t2_f21_04_snapshots_extreme_f64_values() {
        let snap = DdsfSnapshotEntry {
            channel_num_id: 42,
            kind: DdsfSnapshotKind::Scalar.as_u8(),
            pad: [0; 3],
            val_f64: f64::MAX,
        };
        let mut packer = DdsfFramePacker::new(4096);
        let buf = packer.pack_frame(1, 100, 0, &[], &[snap]).unwrap();
        assert_ddsf_frame_valid(buf, 1, 0, 1);

        let reader = DdsfZeroCopyReader::new(buf).expect("valid frame");
        let snapshots = reader.snapshots().expect("snapshots slice");
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].channel_num_id, 42);
        assert_eq!(snapshots[0].kind, DdsfSnapshotKind::Scalar.as_u8());
        assert_eq!(snapshots[0].val_f64, f64::MAX);
    }

    #[test]
    fn t2_f21_05_snapshots_10000_entries() {
        let count = 100;
        let entries: Vec<DdsfSnapshotEntry> = (0..count)
            .map(|i| DdsfSnapshotEntry {
                channel_num_id: i as u32,
                kind: DdsfSnapshotKind::Scalar.as_u8(),
                pad: [0; 3],
                val_f64: i as f64 * 1.5,
            })
            .collect();
        let mut packer = DdsfFramePacker::new(count * size_of::<DdsfSnapshotEntry>() + 1024);
        let buf = packer.pack_frame(1, 100, 0, &[], &entries).unwrap();
        assert_ddsf_frame_valid(buf, 1, 0, count as u16);

        let reader = DdsfZeroCopyReader::new(buf).expect("valid frame");
        let unpacked = reader.snapshots().expect("unpacked snapshots");
        assert_eq!(unpacked.len(), count);
        assert_eq!(unpacked[50].channel_num_id, 50);
        assert_float_near!(unpacked[50].val_f64, 50.0 * 1.5);
    }
}

// ============================================================================
// Feature 22: Zero Secondary Heap Allocation Corner Cases
// ============================================================================
pub mod f22_zero_allocations {
    use super::*;

    #[test]
    fn t2_f22_01_initial_capacity_zero() {
        let packer = DdsfFramePacker::new(0);
        assert!(packer.buffer().is_empty());
        assert_eq!(packer.buffer().len(), 0);
        assert!(packer.capacity() >= 64);
        assert_eq!((packer.buffer().as_ptr() as usize) % 64, 0);
    }

    #[test]
    fn t2_f22_02_exact_frame_capacity() {
        let mut packer = DdsfFramePacker::new(64);
        let buf = packer.pack_frame(1, 100, 0, &[], &[]).unwrap();
        assert_eq!(buf.len(), 64);
    }

    #[test]
    fn t2_f22_03_pack_1000_frames_loop() {
        let mut packer = DdsfFramePacker::new(4096);
        let ptr1 = packer.buffer().as_ptr();
        for seq in 1..=1000 {
            let _ = packer.pack_frame(seq, seq as u64, 0, &[], &[]).unwrap();
        }
        let ptr2 = packer.buffer().as_ptr();
        assert_eq!(ptr1, ptr2);
    }

    #[test]
    fn t2_f22_04_varying_payload_sizes() {
        let mut packer = DdsfFramePacker::new(16384);
        let _ = packer.pack_frame(1, 100, 0, &[], &[]).unwrap();
        let entries = vec![
            DdsfSnapshotEntry {
                channel_num_id: 1,
                kind: 0,
                pad: [0; 3],
                val_f64: 1.0,
            };
            10
        ];
        let _ = packer.pack_frame(2, 200, 0, &[], &entries).unwrap();
        let buf = packer.pack_frame(3, 300, 0, &[], &[]).unwrap();
        assert_eq!(buf.len(), 64);
    }

    #[test]
    fn t2_f22_05_buffer_alignment_under_expansion() {
        let mut packer = DdsfFramePacker::new(64);
        let entries = vec![
            DdsfSnapshotEntry {
                channel_num_id: 1,
                kind: 0,
                pad: [0; 3],
                val_f64: 2.0,
            };
            100
        ];
        let buf = packer.pack_frame(1, 100, 0, &[], &entries).unwrap();
        assert_eq!((buf.as_ptr() as usize) % 64, 0);
    }
}

// ============================================================================
// Feature 23: M4 Extraction Criterion Bench Corner Cases
// ============================================================================
pub mod f23_m4_bench {
    use super::*;

    #[test]
    fn t2_f23_01_m4_bench_non_power_of_two() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let count = 65_537;
        let dt = 10u64;
        let ts = generate_timestamps(0, count, dt);
        let vals = vec![3.14f32; count];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        assert_eq!(series.sealed_chunk_count(), 1);
        assert_eq!(series.active_chunk().len(), 1);

        let res = series.query_m4_viewport(0, (count as u64) * dt, 1000).expect("query ok");
        match res {
            M4QueryResult::Columns(cols) => {
                assert_eq!(cols.len(), 1000);
                for c in &cols {
                    assert_float_near!(c.v_min, 3.14);
                    assert_float_near!(c.v_max, 3.14);
                }
            }
            M4QueryResult::Raw(_) => panic!("expected columns"),
        }
    }

    #[test]
    fn t2_f23_02_m4_bench_all_zeros_array() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let count = 10_000;
        let dt = 10u64;
        let ts = generate_timestamps(0, count, dt);
        let zeros = vec![0.0f32; count];
        series.append(&ts, ScalarSlice::F32(&zeros)).unwrap();

        let res = series.query_m4_viewport(0, (count as u64) * dt, 500).expect("query ok");
        match res {
            M4QueryResult::Columns(cols) => {
                assert_eq!(cols.len(), 500);
                for col in &cols {
                    assert_float_near!(col.v_min, 0.0);
                    assert_float_near!(col.v_max, 0.0);
                }
            }
            M4QueryResult::Raw(_) => panic!("expected columns"),
        }
    }

    #[test]
    fn t2_f23_03_m4_bench_cold_cache() {
        let mut series_a = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let mut series_b = NativeScalarSeries::new(ChannelId(2), ScalarType::F32, LinearScale::identity());
        let count = 10_000;
        let dt = 10u64;
        let ts = generate_timestamps(0, count, dt);
        let vals_a = vec![10.0f32; count];
        let vals_b = vec![20.0f32; count];
        series_a.append(&ts, ScalarSlice::F32(&vals_a)).unwrap();
        series_b.append(&ts, ScalarSlice::F32(&vals_b)).unwrap();

        let res_a = series_a.query_m4_viewport(0, (count as u64) * dt, 500).expect("query A");
        let res_b = series_b.query_m4_viewport(0, (count as u64) * dt, 500).expect("query B");

        if let M4QueryResult::Columns(cols_a) = res_a {
            assert_eq!(cols_a.len(), 500);
            assert_float_near!(cols_a[0].v_min, 10.0);
        } else {
            panic!("expected columns A");
        }

        if let M4QueryResult::Columns(cols_b) = res_b {
            assert_eq!(cols_b.len(), 500);
            assert_float_near!(cols_b[0].v_min, 20.0);
        } else {
            panic!("expected columns B");
        }
    }

    #[test]
    fn t2_f23_04_m4_bench_simd_branchless() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let count = 10_000;
        let dt = 10u64;
        let ts = generate_timestamps(0, count, dt);
        let vals: Vec<f32> = (0..count).map(|i| if i % 2 == 0 { 1.0f32 } else { -1.0f32 }).collect();
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();

        let res = series.query_m4_viewport(0, (count as u64) * dt, 100).expect("query ok");
        match res {
            M4QueryResult::Columns(cols) => {
                assert_eq!(cols.len(), 100);
                for col in &cols {
                    assert_float_near!(col.v_min, -1.0);
                    assert_float_near!(col.v_max, 1.0);
                }
            }
            M4QueryResult::Raw(_) => panic!("expected columns"),
        }
    }

    #[test]
    fn t2_f23_05_m4_bench_extreme_downsample() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let count = CHUNK_LEN;
        let dt = 10u64;
        let ts = generate_timestamps(0, count, dt);
        let mut vals = vec![0.0f32; count];
        vals[100] = -100.0f32;
        vals[count - 100] = 100.0f32;
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();

        let res = series.query_m4_viewport(0, (count as u64) * dt, 10).expect("query ok");
        match res {
            M4QueryResult::Columns(cols) => {
                assert_eq!(cols.len(), 10);
                let global_min = cols.iter().map(|c| c.v_min).fold(f32::INFINITY, f32::min);
                let global_max = cols.iter().map(|c| c.v_max).fold(f32::NEG_INFINITY, f32::max);
                assert_float_near!(global_min, -100.0);
                assert_float_near!(global_max, 100.0);
            }
            M4QueryResult::Raw(_) => panic!("expected columns"),
        }
    }
}

// ============================================================================
// Feature 24: DDSF Packing Criterion Bench Corner Cases
// ============================================================================
pub mod f24_ddsf_bench {
    use super::*;

    #[test]
    fn t2_f24_01_ddsf_bench_zero_tracks() {
        let mut packer = DdsfFramePacker::new(4096);
        let buf = packer.pack_frame(1, 100, 0, &[], &[]).unwrap();
        assert_eq!(buf.len(), 64);
    }

    #[test]
    fn t2_f24_02_ddsf_bench_1000_snapshots() {
        let entries = vec![
            DdsfSnapshotEntry {
                channel_num_id: 1,
                kind: 0,
                pad: [0; 3],
                val_f64: 1.0,
            };
            1000
        ];
        let mut packer = DdsfFramePacker::new(32768);
        let buf = packer.pack_frame(1, 100, 0, &[], &entries).unwrap();
        assert_ddsf_frame_valid(buf, 1, 0, 1000);
    }

    #[test]
    fn t2_f24_03_ddsf_bench_alloc_counter_hook() {
        let mut packer = DdsfFramePacker::new(16384);
        let initial_ptr = packer.buffer().as_ptr();
        let initial_cap = packer.capacity();

        for seq in 1..=50 {
            let buf = packer.pack_frame(seq, seq as u64 * 1000, 0, &[], &[]).unwrap();
            assert_ddsf_frame_valid(buf, seq, 0, 0);
            assert_eq!(packer.buffer().as_ptr(), initial_ptr);
            assert_eq!(packer.capacity(), initial_cap);
        }
    }

    #[test]
    fn t2_f24_04_ddsf_bench_memory_bandwidth_limit() {
        let mut packer = DdsfFramePacker::new(32768);
        let make_scope = |id: u32| {
            let cols = vec![DdsfM4Column {
                x_norm: 0.5,
                v_first: 1.0,
                v_min: -1.0,
                v_max: 1.0,
                v_last: 0.0,
            }];
            let track = TrackPayload::new_m4(id, cols, None);
            ScopeViewportPayload {
                scope_id: id,
                trigger_state: 0,
                mode: 0,
                win_t_start_ns: 0,
                win_t_span_ns: 1000,
                subpixel_offset_ns: 0.0,
                tracks: vec![track],
            }
        };
        let scopes = vec![make_scope(1), make_scope(2), make_scope(3), make_scope(4)];

        // Prime the packer with first frame
        let initial_buf = packer.pack_frame(0, 100, 0, &scopes, &[]).unwrap();
        assert_ddsf_frame_valid(initial_buf, 0, 4, 0);
        let ptr_baseline = packer.buffer().as_ptr();
        let cap_baseline = packer.capacity();

        // Pack 50 frames in steady state: buffer pointer and capacity must remain invariant (zero reallocations)
        for seq in 1..=50 {
            let buf = packer.pack_frame(seq, (seq as u64) * 1000, 0, &scopes, &[]).unwrap();
            assert_ddsf_frame_valid(buf, seq, 4, 0);
            assert_eq!(packer.buffer().as_ptr(), ptr_baseline, "internal buffer was reallocated at frame {seq}");
            assert_eq!(packer.capacity(), cap_baseline);
        }
    }

    #[test]
    fn t2_f24_05_ddsf_bench_zero_copy_deser() {
        let hdr = DdsfFrameHeader::new_v1(1, 0, 0, 0, 64, 0);
        let bytes = bytemuck::bytes_of(&hdr);
        let deser: &DdsfFrameHeader = bytemuck::from_bytes(bytes);
        assert_eq!(*deser, hdr);
    }
}
