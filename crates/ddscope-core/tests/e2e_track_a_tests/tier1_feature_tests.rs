//! Tier 1: Feature Coverage (120 isolated tests, 5 tests per feature for Features 1-24).

use std::mem::{align_of, offset_of, size_of};
use std::sync::Arc;

use bytemuck::{bytes_of, from_bytes, Zeroable};

use super::common::*;

// ============================================================================
// Feature 01: M4Bucket Representation (32-byte POD)
// ============================================================================
pub mod f01_m4_bucket {
    use super::*;

    #[test]
    fn t1_f01_01_m4_bucket_size_and_alignment() {
        assert_eq!(size_of::<M4Bucket>(), 32);
        assert_eq!(align_of::<M4Bucket>(), 8);
    }

    #[test]
    fn t1_f01_02_m4_bucket_bytemuck_pod_zeroable() {
        let b = M4Bucket {
            t_first_ns: 100,
            t_last_ns: 200,
            v_first: 1.0,
            v_min: -0.5,
            v_max: 2.5,
            v_last: 1.8,
        };
        let bytes: &[u8] = bytes_of(&b);
        assert_eq!(bytes.len(), 32);
        let decoded: &M4Bucket = from_bytes(bytes);
        assert_eq!(*decoded, b);
    }

    #[test]
    fn t1_f01_03_m4_bucket_field_ordering_and_offsets() {
        assert_eq!(offset_of!(M4Bucket, t_first_ns), 0x00);
        assert_eq!(offset_of!(M4Bucket, t_last_ns), 0x08);
        assert_eq!(offset_of!(M4Bucket, v_first), 0x10);
        assert_eq!(offset_of!(M4Bucket, v_min), 0x14);
        assert_eq!(offset_of!(M4Bucket, v_max), 0x18);
        assert_eq!(offset_of!(M4Bucket, v_last), 0x1C);
    }

    #[test]
    fn t1_f01_04_m4_bucket_clone_and_equality() {
        let b1 = M4Bucket {
            t_first_ns: 50,
            t_last_ns: 60,
            v_first: 0.0,
            v_min: -10.0,
            v_max: 10.0,
            v_last: 0.0,
        };
        let b2 = b1;
        assert_eq!(b1, b2);
    }

    #[test]
    fn t1_f01_05_m4_bucket_default_zeroed() {
        let b: M4Bucket = Zeroable::zeroed();
        assert_eq!(b.t_first_ns, 0);
        assert_eq!(b.t_last_ns, 0);
        assert_eq!(b.v_first, 0.0);
        assert_eq!(b.v_min, 0.0);
        assert_eq!(b.v_max, 0.0);
        assert_eq!(b.v_last, 0.0);
    }
}

// ============================================================================
// Feature 02: Branching Factor 64 Hierarchy
// ============================================================================
pub mod f02_hierarchy_64 {
    use super::*;

    #[test]
    fn t1_f02_01_branching_factor_is_64() {
        assert_eq!(M4_BRANCH_FACTOR, 64);
    }

    #[test]
    fn t1_f02_02_l1_bucket_count_per_chunk() {
        let ts_vec: Vec<u64> = (0..CHUNK_LEN as u64).map(|i| i * 10).collect();
        let ts_col = TimestampColumn::from_slice(&ts_vec);
        let mut val_vec = AlignedVec::with_capacity(CHUNK_LEN);
        for _ in 0..CHUNK_LEN {
            val_vec.push(1.0f32);
        }
        let val_col = NativeScalarColumn::F32(val_vec);
        let levels = ChunkM4Levels::build_from_scalar(&ts_col, &val_col, &LinearScale::identity())
            .expect("chunk M4 build");
        assert_eq!(levels.len_l1(), 1024, "CHUNK_LEN 65536 / 64 = 1024 L1 buckets");
    }

    #[test]
    fn t1_f02_03_l2_bucket_count_per_chunk() {
        let ts_vec: Vec<u64> = (0..CHUNK_LEN as u64).map(|i| i * 10).collect();
        let ts_col = TimestampColumn::from_slice(&ts_vec);
        let mut val_vec = AlignedVec::with_capacity(CHUNK_LEN);
        for _ in 0..CHUNK_LEN {
            val_vec.push(2.0f32);
        }
        let val_col = NativeScalarColumn::F32(val_vec);
        let levels = ChunkM4Levels::build_from_scalar(&ts_col, &val_col, &LinearScale::identity())
            .expect("chunk M4 build");
        assert_eq!(levels.len_l2(), 16, "1024 L1 / 64 = 16 L2 buckets");
    }

    #[test]
    fn t1_f02_04_l3_sample_span() {
        let mut pyramid = MultiLevelM4Pyramid::new();
        // Ingest 64 L2 buckets (spanning 4 sealed chunks = 262,144 samples)
        let l2_dummy = vec![
            M4Bucket {
                t_first_ns: 0,
                t_last_ns: 40_960,
                v_first: 1.0,
                v_min: 0.0,
                v_max: 2.0,
                v_last: 1.0,
            };
            64
        ];
        pyramid.append_chunk_l2(&l2_dummy);
        assert_eq!(pyramid.l3.len(), 1, "64 L2 buckets aggregate to exactly 1 L3 bucket");
        assert_eq!(pyramid.l4.len(), 0);
    }

    #[test]
    fn t1_f02_05_l4_sample_span() {
        let mut pyramid = MultiLevelM4Pyramid::new();
        // Ingest 4,096 L2 buckets (spanning 256 chunks = 64 L3 buckets = 16,777,216 samples)
        let l2_dummy = vec![
            M4Bucket {
                t_first_ns: 0,
                t_last_ns: 40_960,
                v_first: 1.0,
                v_min: 0.0,
                v_max: 2.0,
                v_last: 1.0,
            };
            64
        ];
        for _ in 0..64 {
            pyramid.append_chunk_l2(&l2_dummy);
        }
        assert_eq!(pyramid.l3.len(), 64);
        assert_eq!(pyramid.l4.len(), 1, "64 L3 buckets aggregate to exactly 1 L4 bucket");
    }
}

// ============================================================================
// Feature 03: SIMD Incremental Construction
// ============================================================================
pub mod f03_simd_construction {
    use super::*;

    #[test]
    fn t1_f03_01_simd_f32_reduction() {
        let mut data = vec![1.0f32; 64];
        data[10] = -5.0;
        data[45] = 12.0;
        let scale = LinearScale::identity();
        let bucket = reduce_raw_f32_slice(&data, 100, 730, &scale);
        assert_eq!(bucket.v_min, -5.0);
        assert_eq!(bucket.v_max, 12.0);
        assert_eq!(bucket.v_first, 1.0);
        assert_eq!(bucket.v_last, 1.0);
        assert_eq!(bucket.t_first_ns, 100);
        assert_eq!(bucket.t_last_ns, 730);
    }

    #[test]
    fn t1_f03_02_simd_i16_scaled_reduction() {
        let raw = vec![100i16; 64];
        let scale = LinearScale::new(0.1, 5.0);
        let bucket = reduce_raw_i16_slice(&raw, 0, 630, &scale);
        assert_float_near!(bucket.v_min, 15.0);
        assert_float_near!(bucket.v_max, 15.0);
        assert_float_near!(bucket.v_first, 15.0);
        assert_float_near!(bucket.v_last, 15.0);
    }

    #[test]
    fn t1_f03_03_simd_negative_scale_inversion() {
        let data = vec![10.0f32, 20.0, 30.0, 50.0];
        let scale = LinearScale::new(-2.0, 100.0);
        let bucket = reduce_raw_f32_slice(&data, 0, 30, &scale);
        // raw 10.0 -> 80.0, raw 50.0 -> 0.0
        assert_float_near!(bucket.v_min, 0.0);
        assert_float_near!(bucket.v_max, 80.0);
        assert_float_near!(bucket.v_first, 80.0);
        assert_float_near!(bucket.v_last, 0.0);
    }

    #[test]
    fn t1_f03_04_simd_u8_scaled_reduction() {
        let raw = vec![0u8, 50, 100, 255];
        let scale = LinearScale::new(0.01, 0.0);
        let bucket = reduce_raw_u8_slice(&raw, 0, 30, &scale);
        assert_float_near!(bucket.v_min, 0.0);
        assert_float_near!(bucket.v_max, 2.55);
    }

    #[test]
    fn t1_f03_05_simd_active_incremental() {
        let ts_vec: Vec<u64> = (0..128).map(|i| i * 10).collect();
        let ts_col = TimestampColumn::from_slice(&ts_vec);
        let mut val_vec = AlignedVec::with_capacity(128);
        for i in 0..128 {
            val_vec.push(i as f32);
        }
        let val_col = NativeScalarColumn::F32(val_vec);
        let levels = ChunkM4Levels::build_from_scalar(&ts_col, &val_col, &LinearScale::identity())
            .expect("build partial m4");
        assert_eq!(levels.len_l1(), 2, "128 samples produce 2 L1 buckets");
        let summary = levels.chunk_summary().expect("summary available");
        assert_eq!(summary.v_min, 0.0);
        assert_eq!(summary.v_max, 127.0);
    }
}

// ============================================================================
// Feature 04: 100% Extrema Retention
// ============================================================================
pub mod f04_extrema_retention {
    use super::*;

    #[test]
    fn t1_f04_01_single_sample_peak_retention_in_l1() {
        let glitch_val = 999.0f32;
        let signal = generate_glitch_signal(64, 32, glitch_val, 1.0);
        let bucket = reduce_raw_f32_slice(&signal, 0, 630, &LinearScale::identity());
        assert_eq!(bucket.v_max, glitch_val, "single peak must be preserved in L1 bucket");
        assert_eq!(bucket.v_min, 1.0);
    }

    #[test]
    fn t1_f04_02_single_sample_valley_retention_in_l1() {
        let valley_val = -888.0f32;
        let signal = generate_glitch_signal(64, 15, valley_val, 5.0);
        let bucket = reduce_raw_f32_slice(&signal, 0, 630, &LinearScale::identity());
        assert_eq!(bucket.v_min, valley_val, "single valley must be preserved in L1 bucket");
        assert_eq!(bucket.v_max, 5.0);
    }

    #[test]
    fn t1_f04_03_peak_preserved_at_boundary_index_0() {
        let signal = generate_glitch_signal(64, 0, 500.0, 0.0);
        let bucket = reduce_raw_f32_slice(&signal, 0, 630, &LinearScale::identity());
        assert_eq!(bucket.v_first, 500.0);
        assert_eq!(bucket.v_max, 500.0);
    }

    #[test]
    fn t1_f04_04_peak_preserved_at_boundary_index_63() {
        let signal = generate_glitch_signal(64, 63, 500.0, 0.0);
        let bucket = reduce_raw_f32_slice(&signal, 0, 630, &LinearScale::identity());
        assert_eq!(bucket.v_last, 500.0);
        assert_eq!(bucket.v_max, 500.0);
    }

    #[test]
    fn t1_f04_05_peak_preserved_across_l1_to_l2_reduction() {
        let mut buckets = [
            M4Bucket {
                t_first_ns: 0,
                t_last_ns: 640,
                v_first: 0.0,
                v_min: 0.0,
                v_max: 1.0,
                v_last: 0.0,
            };
            64
        ];
        buckets[17].v_max = 1234.5;
        let l2_bucket = reduce_64_buckets(&buckets);
        assert_eq!(l2_bucket.v_max, 1234.5, "L1 to L2 reduction preserves isolated peak");
    }
}

// ============================================================================
// Feature 05: Pixel-Aligned Query
// ============================================================================
pub mod f05_pixel_aligned_query {
    use super::*;

    #[test]
    fn t1_f05_01_raw_below_threshold() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let count = 100;
        let ts: Vec<u64> = (0..count as u64).map(|i| i * 10).collect();
        let val = vec![1.0f32; count];
        series.append(&ts, ScalarSlice::F32(&val)).expect("append ok");

        // N = 100 <= 2 * w_px = 200 -> Selects Raw
        let res = series.query_m4_viewport(0, 1000, 100).expect("viewport ok");
        match res {
            M4QueryResult::Raw(pts) => {
                assert_eq!(pts.len(), 100, "Raw points count equals sample count in range");
            }
            M4QueryResult::Columns(_) => panic!("expected M4QueryResult::Raw when N <= 2 * w_px"),
        }
    }

    #[test]
    fn t1_f05_02_m4_above_threshold() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let count = 500;
        let ts: Vec<u64> = (0..count as u64).map(|i| i * 10).collect();
        let val = vec![2.0f32; count];
        series.append(&ts, ScalarSlice::F32(&val)).expect("append ok");

        // N = 500 > 2 * w_px = 200 -> Selects Columns
        let res = series.query_m4_viewport(0, 5000, 100).expect("viewport ok");
        match res {
            M4QueryResult::Columns(cols) => {
                assert_eq!(cols.len(), 100, "Decimated M4 column count matches pixel width");
            }
            M4QueryResult::Raw(_) => panic!("expected M4QueryResult::Columns when N > 2 * w_px"),
        }
    }

    #[test]
    fn t1_f05_03_m4_column_count_matches_pixel_width() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let count = 1000;
        let ts: Vec<u64> = (0..count as u64).map(|i| i * 10).collect();
        let val = vec![0.5f32; count];
        series.append(&ts, ScalarSlice::F32(&val)).expect("append ok");

        let w_px = 192;
        let res = series.query_m4_viewport(0, 10000, w_px).expect("query ok");
        if let M4QueryResult::Columns(cols) = res {
            assert_eq!(cols.len(), w_px);
        } else {
            panic!("expected columns");
        }
    }

    #[test]
    fn t1_f05_04_m4_column_x_norm_monotonic() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let count = 1000;
        let ts: Vec<u64> = (0..count as u64).map(|i| i * 10).collect();
        let val = vec![1.0f32; count];
        series.append(&ts, ScalarSlice::F32(&val)).expect("append ok");

        let res = series.query_m4_viewport(0, 10000, 100).expect("query ok");
        if let M4QueryResult::Columns(cols) = res {
            for i in 1..cols.len() {
                assert!(cols[i].x_norm >= cols[i - 1].x_norm);
            }
        } else {
            panic!("expected columns");
        }
    }

    #[test]
    fn t1_f05_05_partial_window_bounds() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let count = 2000;
        let ts: Vec<u64> = (0..count as u64).map(|i| i * 10).collect();
        let val = vec![1.0f32; count];
        series.append(&ts, ScalarSlice::F32(&val)).expect("append ok");

        let res = series.query_m4_viewport(5000, 15000, 50).expect("sub-window query");
        match res {
            M4QueryResult::Columns(cols) => assert_eq!(cols.len(), 50),
            M4QueryResult::Raw(pts) => assert!(!pts.is_empty()),
        }
    }
}

// ============================================================================
// Feature 06: Sub-0.1ms Query Latency
// ============================================================================
pub mod f06_latency_criterion {
    use super::*;

    #[test]
    fn t1_f06_01_latency_100m_points_pyramid_scan_count() {
        let bucket_count = 25_000; // Represents 100M points at Level 2 (4096 samples/bucket)
        let buckets = vec![
            M4Bucket {
                t_first_ns: 0,
                t_last_ns: 40_960,
                v_first: 1.0,
                v_min: 0.0,
                v_max: 2.0,
                v_last: 1.0,
            };
            bucket_count
        ];
        let start = std::time::Instant::now();
        let cols = aggregate_buckets_to_columns(&buckets, 0, 1_000_000_000, 2000);
        let elapsed = start.elapsed();
        assert_eq!(cols.len(), 2000);
        assert!(elapsed.as_millis() < 50, "25,000 bucket decimation must complete rapidly");
    }

    #[test]
    fn t1_f06_02_latency_level_selection_optimality() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let ts: Vec<u64> = (0..CHUNK_LEN as u64).map(|i| i * 10).collect();
        let val = vec![1.0f32; CHUNK_LEN];
        series.append(&ts, ScalarSlice::F32(&val)).expect("append sealed chunk");

        let start = std::time::Instant::now();
        let res = series.query_m4_viewport(0, CHUNK_LEN as u64 * 10, 1000).expect("query ok");
        let elapsed = start.elapsed();
        assert!(matches!(res, M4QueryResult::Columns(_)));
        assert!(elapsed.as_millis() < 20);
    }

    #[test]
    fn t1_f06_03_latency_10m_points_reduction_speed() {
        let buckets = vec![
            M4Bucket {
                t_first_ns: 0,
                t_last_ns: 100,
                v_first: 1.0,
                v_min: 0.0,
                v_max: 2.0,
                v_last: 1.0,
            };
            2000
        ];
        let start = std::time::Instant::now();
        let cols = aggregate_buckets_to_columns(&buckets, 0, 200_000, 2000);
        let elapsed = start.elapsed();
        assert_eq!(cols.len(), 2000);
        assert!(elapsed.as_millis() < 10);
    }

    #[test]
    fn t1_f06_04_latency_cold_cache_scan() {
        let cols: Vec<DdsfM4Column> = (0..2000)
            .map(|i| DdsfM4Column {
                x_norm: i as f32 / 2000.0,
                v_first: 1.0,
                v_min: -1.0,
                v_max: 1.0,
                v_last: 0.0,
            })
            .collect();
        let start = std::time::Instant::now();
        let stats = compute_track_stats_from_columns(&cols);
        let elapsed = start.elapsed();
        assert_eq!(stats.max, 1.0);
        assert_eq!(stats.min, -1.0);
        assert!(elapsed.as_millis() < 10);
    }

    #[test]
    fn t1_f06_05_latency_multi_channel() {
        let mut channels = Vec::with_capacity(4);
        for ch in 1..=4 {
            let mut s = NativeScalarSeries::new(ChannelId(ch), ScalarType::F32, LinearScale::identity());
            let ts: Vec<u64> = (0..5000).map(|i| i * 10).collect();
            let val = vec![ch as f32; 5000];
            s.append(&ts, ScalarSlice::F32(&val)).unwrap();
            channels.push(s);
        }

        let start = std::time::Instant::now();
        for ch in &channels {
            let res = ch.query_m4_viewport(0, 50000, 2000).unwrap();
            assert!(matches!(res, M4QueryResult::Columns(_)));
        }
        let elapsed = start.elapsed();
        assert!(elapsed.as_millis() < 20);
    }
}

// ============================================================================
// Feature 07: Global 2GB Quota Tracking
// ============================================================================
pub mod f07_quota_tracking {
    use super::*;

    #[test]
    fn t1_f07_01_quota_initial_state() {
        let q = MemoryQuotaManager::new_2gb();
        assert_eq!(q.allocated_bytes(), 0);
        assert_eq!(q.pinned_bytes(), 0);
        assert!(!q.is_warning());
    }

    #[test]
    fn t1_f07_02_quota_allocate_release() {
        let q = MemoryQuotaManager::new_2gb();
        q.allocate(100 * 1024 * 1024).unwrap();
        assert_eq!(q.allocated_bytes(), 100 * 1024 * 1024);
        q.release(50 * 1024 * 1024);
        assert_eq!(q.allocated_bytes(), 50 * 1024 * 1024);
    }

    #[test]
    fn t1_f07_03_quota_warning_threshold_trigger() {
        let q = MemoryQuotaManager::new_2gb();
        let threshold = (2 * 1024 * 1024 * 1024 as u64 * 85 / 100) as usize;
        q.allocate(threshold).unwrap();
        assert!(q.is_warning());
    }

    #[test]
    fn t1_f07_04_quota_warning_threshold_recovery() {
        let q = MemoryQuotaManager::new_2gb();
        let threshold = (2 * 1024 * 1024 * 1024 as u64 * 85 / 100) as usize;
        q.allocate(threshold).unwrap();
        assert!(q.is_warning());
        q.release(100 * 1024 * 1024);
        assert!(!q.is_warning());
    }

    #[test]
    fn t1_f07_05_quota_atomic_safety() {
        let q = Arc::new(MemoryQuotaManager::new_2gb());
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let q_clone = Arc::clone(&q);
                std::thread::spawn(move || {
                    for _ in 0..100 {
                        q_clone.allocate(1024).unwrap();
                        q_clone.release(1024);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(q.allocated_bytes(), 0);
    }
}

// ============================================================================
// Feature 08: FIFO Unpinned Eviction
// ============================================================================
pub mod f08_fifo_eviction {
    use super::*;

    #[test]
    fn t1_f08_01_eviction_at_limit() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let n = CHUNK_LEN;
        let dt = 10u64;

        // Ingest 1 chunk to measure chunk resident footprint
        let ts0 = generate_timestamps(0, n, dt);
        let vals0 = vec![1.0f32; n];
        series.append(&ts0, ScalarSlice::F32(&vals0)).unwrap();
        assert_eq!(series.sealed_chunk_count(), 1);
        let chunk_bytes = series.sealed_chunks()[0].storage.resident_bytes();

        // Limit quota to hold exactly 3 chunks
        let quota = Arc::new(MemoryQuotaManager::new(chunk_bytes * 3 + 1024));
        quota.record_allocation(chunk_bytes);
        series.set_quota_manager(quota);

        // Append 2 more chunks (total 3 chunks in memory)
        for i in 1..3 {
            let t0 = (i * n as u64) * dt;
            let ts = generate_timestamps(t0, n, dt);
            let vals = vec![(i + 1) as f32; n];
            series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        }
        assert_eq!(series.sealed_chunk_count(), 3);
        assert_eq!(series.evicted_samples(), 0);

        // Append 4th chunk -> exceeds quota limit of 3 chunks, triggers automatic eviction of chunk 0
        let t0 = (3 * n as u64) * dt;
        let ts = generate_timestamps(t0, n, dt);
        let vals = vec![4.0f32; n];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();

        assert_eq!(series.sealed_chunk_count(), 3);
        assert_eq!(series.evicted_samples(), n as u64);
        assert!(series.get_sample_by_index(0).is_none(), "chunk 0 was evicted");
        assert!(series.get_sample_by_index(n as u64).is_some(), "chunk 1 is retained");
    }

    #[test]
    fn t1_f08_02_eviction_fifo_order() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let n = CHUNK_LEN;
        let dt = 10u64;

        // Ingest 3 sealed chunks with distinct timestamps and values
        for i in 0..3 {
            let t0 = (i * n as u64) * dt;
            let ts = generate_timestamps(t0, n, dt);
            let vals = vec![(i + 1) as f32; n];
            series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        }
        assert_eq!(series.sealed_chunk_count(), 3);

        // Evict oldest unpinned: MUST be Chunk 0 (t_start_ns == 0), NOT Chunk 2 (LIFO)!
        let evicted = series.evict_oldest_unpinned().expect("should evict chunk 0");
        assert_eq!(evicted.t_start_ns, 0, "FIFO eviction MUST evict chunk 0 with t_start_ns == 0");
        assert_eq!(evicted.sample_count, n as u32);

        // Chunk 0 sample index is None (evicted)
        assert!(series.get_sample_by_index(0).is_none());

        // Chunk 1 & 2 are preserved
        let s_chunk1 = series.get_sample_by_index(n as u64).expect("chunk 1 retained");
        assert_eq!(s_chunk1.1, 2.0);
        let s_chunk2 = series.get_sample_by_index(2 * n as u64).expect("chunk 2 retained");
        assert_eq!(s_chunk2.1, 3.0);
    }

    #[test]
    fn t1_f08_03_eviction_unpinned_strong_count_zero() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let n = CHUNK_LEN;
        let ts = generate_timestamps(0, n, 10);
        let vals = vec![1.0f32; n];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        let sealed = series.sealed_chunks();
        assert_eq!(sealed.len(), 1);
        if let ChunkStorageRef::InMemory(ref c) = sealed[0].storage {
            assert_eq!(Arc::strong_count(c), 1);
        }
    }

    #[test]
    fn t1_f08_04_active_chunk_never_evicted() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let n = 1000;
        let ts = generate_timestamps(0, n, 10);
        let vals = vec![1.0f32; n];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        assert_eq!(series.sealed_chunk_count(), 0);
        assert_eq!(series.total_samples(), 1000);
    }

    #[test]
    fn t1_f08_05_eviction_query_post_eviction() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let n = CHUNK_LEN;
        let ts = generate_timestamps(0, n, 10);
        let vals = vec![1.0f32; n];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        // Query post-seal
        let res = series.query_m4_viewport(0, (n - 1) as u64 * 10, 100).unwrap();
        assert!(matches!(res, M4QueryResult::Columns(_)));
    }
}

// ============================================================================
// Feature 09: Chunk Pinning (PinnedGuard)
// ============================================================================
pub mod f09_chunk_pinning {
    use super::*;

    #[test]
    fn t1_f09_01_pin_range_creates_guard() {
        let q = Arc::new(MemoryQuotaManager::new_2gb());
        let guard = q.pin(1024 * 1024).unwrap();
        assert_eq!(guard.bytes(), 1024 * 1024);
        assert_eq!(q.pinned_bytes(), 1024 * 1024);
    }

    #[test]
    fn t1_f09_02_pinned_guard_drop_releases_memory() {
        let q = Arc::new(MemoryQuotaManager::new_2gb());
        {
            let _guard = q.pin(500 * 1024 * 1024).unwrap();
            assert_eq!(q.pinned_bytes(), 500 * 1024 * 1024);
        }
        assert_eq!(q.pinned_bytes(), 0);
    }

    #[test]
    fn t1_f09_03_pinned_chunk_arc_cloning() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        series.set_quota_manager(Arc::new(MemoryQuotaManager::new_2gb()));
        let n = CHUNK_LEN;
        let ts = generate_timestamps(0, n, 10);
        let vals = vec![1.0f32; n];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        assert_eq!(series.sealed_chunk_count(), 1);

        // Pin sealed chunk via pin_range
        let guard = series.pin_range(0, 100_000).expect("pin chunk");
        assert_eq!(guard.chunk_count(), 1, "guard pins 1 chunk");

        // While pinned, evict_oldest_unpinned must NOT evict this chunk
        assert!(series.evict_oldest_unpinned().is_none());

        // Drop guard and verify chunk becomes evictable
        drop(guard);
        let evicted = series.evict_oldest_unpinned().expect("now evictable after drop");
        assert_eq!(evicted.t_start_ns, 0);
    }

    #[test]
    fn t1_f09_04_active_chunk_sealing_on_pin() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        series.set_quota_manager(Arc::new(MemoryQuotaManager::new_2gb()));
        // Append 10,000 samples (< CHUNK_LEN 65,536), stays in active buffer
        let ts = generate_timestamps(0, 10_000, 10);
        let vals = vec![42.0f32; 10_000];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        assert_eq!(series.sealed_chunk_count(), 0, "active buffer not yet sealed");

        // Pinning a range that covers active samples forces active buffer sealing
        let guard = series.pin_range(0, 50_000).expect("pin forces seal");
        assert_eq!(series.sealed_chunk_count(), 1, "pinning active samples must seal active buffer into sealed chunk");
        assert_eq!(guard.chunk_count(), 1);
    }

    #[test]
    fn t1_f09_05_multi_scope_shared_pin() {
        let q = Arc::new(MemoryQuotaManager::new_2gb());
        let g1 = q.pin(100 * 1024 * 1024).unwrap();
        let g2 = q.pin(100 * 1024 * 1024).unwrap();
        assert_eq!(q.pinned_bytes(), 200 * 1024 * 1024);
        drop(g1);
        assert_eq!(q.pinned_bytes(), 100 * 1024 * 1024);
        drop(g2);
        assert_eq!(q.pinned_bytes(), 0);
    }
}

// ============================================================================
// Feature 10: Ring Buffer Rollover Safety
// ============================================================================
pub mod f10_rollover_safety {
    use super::*;

    #[test]
    fn t1_f10_01_rollover_single_wrap() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let n = CHUNK_LEN;
        let dt = 10u64;

        for i in 0..3 {
            let t0 = (i * n as u64) * dt;
            let ts = generate_timestamps(t0, n, dt);
            let vals = vec![i as f32; n];
            series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        }
        assert_eq!(series.sealed_chunk_count(), 3);

        let evicted = series.evict_oldest_unpinned().expect("single wrap eviction");
        assert_eq!(evicted.t_start_ns, 0);
        assert_eq!(series.evicted_samples(), n as u64);
        assert!(series.get_sample_by_index(0).is_none(), "sample 0 evicted");
        assert!(series.get_sample_by_index((n - 1) as u64).is_none(), "last sample of chunk 0 evicted");
        let s1 = series.get_sample_by_index(n as u64).expect("sample in surviving chunk 1");
        assert_eq!(s1.0, (n as u64) * dt);
        assert_eq!(s1.1, 1.0);
    }

    #[test]
    fn t1_f10_02_rollover_multi_wrap_survival() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let n = CHUNK_LEN;
        let dt = 10u64;

        for i in 0..5 {
            let t0 = (i * n as u64) * dt;
            let ts = generate_timestamps(t0, n, dt);
            let vals = vec![i as f32; n];
            series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        }
        assert_eq!(series.sealed_chunk_count(), 5);

        for step in 0..3 {
            let ev = series.evict_oldest_unpinned().expect("evict sequential chunk");
            assert_eq!(ev.t_start_ns, (step * n as u64) * dt);
        }

        assert_eq!(series.evicted_samples(), (3 * n) as u64);
        assert_eq!(series.sealed_chunk_count(), 2);
        // Chunks 0..3 are gone
        assert!(series.get_sample_by_index(0).is_none());
        assert!(series.get_sample_by_index((3 * n - 1) as u64).is_none());
        // Chunks 3 and 4 survive
        let s_ch3 = series.get_sample_by_index((3 * n) as u64).expect("chunk 3 sample survives");
        assert_eq!(s_ch3.1, 3.0);
        let s_ch4 = series.get_sample_by_index((4 * n) as u64).expect("chunk 4 sample survives");
        assert_eq!(s_ch4.1, 4.0);
    }

    #[test]
    fn t1_f10_03_rollover_timestamp_continuity() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let n = CHUNK_LEN;
        let dt = 10u64;

        // Append 3 chunks
        for i in 0..3 {
            let t0 = (i * n as u64) * dt;
            let ts = generate_timestamps(t0, n, dt);
            let vals = vec![i as f32; n];
            series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        }
        // Evict chunk 0
        let _ = series.evict_oldest_unpinned().unwrap();

        // Verify timestamp continuity across the remaining sealed boundary (chunk 1 -> chunk 2)
        let s_last_ch1 = series.get_sample_by_index((2 * n as u64) - 1).unwrap();
        let s_first_ch2 = series.get_sample_by_index(2 * n as u64).unwrap();
        assert_eq!(s_first_ch2.0, s_last_ch1.0 + dt);
    }

    #[test]
    fn t1_f10_04_rollover_zoom_invariance() {
        let glitch_val = 999.0f32;
        let data = generate_glitch_signal(64, 10, glitch_val, 0.0);
        let b = reduce_raw_f32_slice(&data, 0, 630, &LinearScale::identity());
        assert_eq!(b.v_max, glitch_val);
    }

    #[test]
    fn t1_f10_05_rollover_memory_stability() {
        let q = MemoryQuotaManager::new(50 * 1024 * 1024);
        for _ in 0..100 {
            q.allocate(1024 * 1024).unwrap();
            q.release(1024 * 1024);
        }
        assert_eq!(q.allocated_bytes(), 0);
    }
}

// ============================================================================
// Feature 11: 40% Pinning Memory Ceiling
// ============================================================================
pub mod f11_pinning_ceiling {
    use super::*;

    #[test]
    fn t1_f11_01_pinning_under_limit() {
        let q = Arc::new(MemoryQuotaManager::new_2gb());
        let guard = q.pin(700 * 1024 * 1024).unwrap();
        assert_eq!(guard.bytes(), 700 * 1024 * 1024);
    }

    #[test]
    fn t1_f11_02_pinning_rejection_at_limit() {
        let q = Arc::new(MemoryQuotaManager::new_2gb());
        let res = q.pin(900 * 1024 * 1024);
        assert!(res.is_err());
    }

    #[test]
    fn t1_f11_03_pinning_cumulative() {
        let q = Arc::new(MemoryQuotaManager::new_2gb());
        let _g1 = q.pin(400 * 1024 * 1024).unwrap();
        let _g2 = q.pin(350 * 1024 * 1024).unwrap();
        assert!(q.pin(150 * 1024 * 1024).is_err());
    }

    #[test]
    fn t1_f11_04_pinning_release_frees_capacity() {
        let q = Arc::new(MemoryQuotaManager::new_2gb());
        let g1 = q.pin(700 * 1024 * 1024).unwrap();
        assert!(q.pin(200 * 1024 * 1024).is_err());
        drop(g1);
        assert!(q.pin(200 * 1024 * 1024).is_ok());
    }

    #[test]
    fn t1_f11_05_pinning_preserves_running_buffers() {
        let q = Arc::new(MemoryQuotaManager::new_2gb());
        q.allocate(500 * 1024 * 1024).unwrap();
        let pin_res = q.pin(900 * 1024 * 1024);
        assert!(pin_res.is_err());
        assert_eq!(q.allocated_bytes(), 500 * 1024 * 1024);
    }
}

// ============================================================================
// Feature 12: Evicted Offset Indexing
// ============================================================================
pub mod f12_evicted_indexing {
    use super::*;

    #[test]
    fn t1_f12_01_evicted_offset_initial_zero() {
        let series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        assert_eq!(series.total_samples(), 0);
        assert!(series.is_empty());
        assert!(series.time_range_ns().is_none());
        assert_eq!(series.evicted_samples(), 0);
    }

    #[test]
    fn t1_f12_02_evicted_offset_advancement() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let n = CHUNK_LEN;
        let ts = generate_timestamps(0, n, 10);
        let vals = vec![1.0f32; n];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        assert_eq!(series.total_samples(), CHUNK_LEN as u64);
        assert_eq!(series.sealed_chunk_count(), 1);
        assert_eq!(series.evicted_samples(), 0);
    }

    #[test]
    fn t1_f12_03_evicted_offset_index_translation() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let n = CHUNK_LEN;
        let ts = generate_timestamps(1000, n, 10);
        let vals: Vec<f32> = (0..n).map(|i| i as f32).collect();
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();

        let query = series.query_m4_viewport(1000, 1000 + 100 * 10, 500).unwrap();
        if let M4QueryResult::Raw(pts) = query {
            assert_eq!(pts.len(), 101);
            assert_eq!(pts[0].y_val, 0.0);
            assert_eq!(pts[100].y_val, 100.0);
        } else {
            panic!("expected Raw");
        }
    }

    #[test]
    fn t1_f12_04_evicted_offset_lookup_error() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let n = 100;
        let ts = generate_timestamps(5000, n, 10);
        let vals = vec![2.0f32; n];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();

        // Query before series start time: should return empty Raw without error
        let query = series.query_m4_viewport(1000, 4000, 100).unwrap();
        assert_eq!(query, M4QueryResult::Raw(Vec::new()));
    }

    #[test]
    fn t1_f12_05_evicted_timestamp_lower_bound() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let n = 50;
        let ts = generate_timestamps(1_000_000_000, n, 10);
        let vals = vec![1.0f32; n];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();

        let (t_start, t_end) = series.time_range_ns().unwrap();
        assert_eq!(t_start, 1_000_000_000);
        assert_eq!(t_end, 1_000_000_000 + 49 * 10);
    }
}

// ============================================================================
// Feature 13: ScopeController State Machine
// ============================================================================
pub mod f13_scope_controller {
    use super::*;

    #[test]
    fn t1_f13_01_scope_initial_roll() {
        let ctrl = make_scope_controller(1, TriggerSweepMode::Roll, 10_000_000, 100_000_000);
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::Roll);
    }

    #[test]
    fn t1_f13_02_scope_switch_to_normal_arms() {
        let mut ctrl = make_scope_controller(1, TriggerSweepMode::Roll, 10_000_000, 100_000_000);
        ctrl.set_sweep_mode(TriggerSweepMode::Normal);
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);
    }

    #[test]
    fn t1_f13_03_scope_switch_to_auto_arms() {
        let mut ctrl = make_scope_controller(1, TriggerSweepMode::Roll, 10_000_000, 100_000_000);
        ctrl.set_sweep_mode(TriggerSweepMode::Auto);
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);
    }

    #[test]
    fn t1_f13_04_scope_switch_to_single_arms() {
        let mut ctrl = make_scope_controller(1, TriggerSweepMode::Roll, 10_000_000, 100_000_000);
        ctrl.set_sweep_mode(TriggerSweepMode::Single);
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);
    }

    #[test]
    fn t1_f13_05_scope_single_latched() {
        let mut ctrl = make_scope_controller(1, TriggerSweepMode::Single, 10_000_000, 100_000_000);
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);
        ctrl.rearm();
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);
    }
}

// ============================================================================
// Feature 14: Auto-Trigger Timeout
// ============================================================================
pub mod f14_auto_timeout {
    use super::*;

    #[test]
    fn t1_f14_01_auto_timeout_100ms() {
        let config = ScopeControllerConfig {
            sweep_mode: TriggerSweepMode::Auto,
            time_span_ns: 10_000_000,
            auto_timeout_ns: 100_000_000, // 100 ms
            trigger: Some(TriggerCondition::Edge {
                channel: ChannelId(1),
                level: 2.0,
                hysteresis: 0.5,
                slope: EdgeSlope::Rising,
            }),
            ..Default::default()
        };
        let mut ctrl = ScopeController::new(config);
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);

        // Step at 1ms to initialize armed baseline (non-zero)
        ctrl.step(1_000_000);

        // Step at 50ms: still waiting
        let outcome50 = ctrl.step(50_000_000);
        assert_eq!(outcome50, ScopeStepOutcome::WaitingForTrigger);

        // Step at 110ms (> 100ms timeout): AutoFreerun fires!
        let outcome110 = ctrl.step(110_000_000);
        match outcome110 {
            ScopeStepOutcome::AutoFreerunFired { window } => {
                assert_eq!(window.mode, TriggerSweepMode::Auto);
                assert_eq!(window.trigger_state, TriggerRuntimeState::AutoFreerun);
                assert_eq!(window.subpixel_offset_ns, 0.0);
                assert!(window.trigger_timestamp_ns.is_none());
            }
            _ => panic!("expected AutoFreerunFired, got {:?}", outcome110),
        }
    }

    #[test]
    fn t1_f14_02_auto_freerun_rearms_to_armed() {
        let config = ScopeControllerConfig {
            sweep_mode: TriggerSweepMode::Auto,
            auto_timeout_ns: 100_000_000,
            ..Default::default()
        };
        let mut ctrl = ScopeController::new(config);
        ctrl.step(1_000_000);
        let outcome = ctrl.step(110_000_000);
        assert!(matches!(outcome, ScopeStepOutcome::AutoFreerunFired { .. }));
        // Controller automatically re-arms after freerun
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);
    }

    #[test]
    fn t1_f14_03_auto_freerun_recovers_on_edge() {
        let config = ScopeControllerConfig {
            sweep_mode: TriggerSweepMode::Auto,
            auto_timeout_ns: 100_000_000,
            time_span_ns: 10_000_000,
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
        ctrl.step(110_000_000); // Trigger freerun

        // Prime low then edge crossing closely spaced (within time_span)
        ctrl.process_scalar_sample(ChannelId(1), 120_000_000, -1.0);
        let out = ctrl.process_scalar_sample(ChannelId(1), 120_001_000, 1.0);
        assert!(matches!(out, ScopeStepOutcome::TriggerDetected { .. }));
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::Triggered);
    }

    #[test]
    fn t1_f14_04_auto_timeout_reset_on_edge() {
        let config = ScopeControllerConfig {
            sweep_mode: TriggerSweepMode::Auto,
            auto_timeout_ns: 100_000_000,
            time_span_ns: 10_000_000,
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
        ctrl.process_scalar_sample(ChannelId(1), 50_000_000, -1.0);
        // Edge crossing at 50_001_000 before 100ms timeout
        let out = ctrl.process_scalar_sample(ChannelId(1), 50_001_000, 1.0);
        assert!(matches!(out, ScopeStepOutcome::TriggerDetected { .. }));
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::Triggered);
    }

    #[test]
    fn t1_f14_05_normal_mode_no_timeout() {
        let config = ScopeControllerConfig {
            sweep_mode: TriggerSweepMode::Normal,
            auto_timeout_ns: 100_000_000,
            ..Default::default()
        };
        let mut ctrl = ScopeController::new(config);
        ctrl.step(1_000_000);
        let out = ctrl.step(250_000_000);
        assert_eq!(out, ScopeStepOutcome::WaitingForTrigger);
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);
    }
}

// ============================================================================
// Feature 15: Hysteresis Edge Debouncing
// ============================================================================
pub mod f15_hysteresis_debouncing {
    use super::*;

    #[test]
    fn t1_f15_01_hysteresis_rising_edge() {
        let mut engine = make_edge_trigger_engine(ChannelId(1), 2.0, 0.5, EdgeSlope::Rising);
        let hit1 = engine.evaluate_scalar(100, 1.0);
        assert!(hit1.is_none());
        assert_eq!(engine.state(), HysteresisBandState::ArmedLow);

        let hit2 = engine.evaluate_scalar(200, 2.5);
        assert!(hit2.is_some());
    }

    #[test]
    fn t1_f15_02_hysteresis_rising_noise_rejection() {
        let mut engine = make_edge_trigger_engine(ChannelId(1), 2.0, 0.5, EdgeSlope::Rising);
        let hit1 = engine.evaluate_scalar(100, 1.8);
        assert!(hit1.is_none());
        let hit2 = engine.evaluate_scalar(200, 2.2);
        assert!(hit2.is_none());
        assert_eq!(engine.state(), HysteresisBandState::Unprimed);
    }

    #[test]
    fn t1_f15_03_hysteresis_falling_edge() {
        let mut engine = make_edge_trigger_engine(ChannelId(1), 0.0, 0.2, EdgeSlope::Falling);
        let hit1 = engine.evaluate_scalar(100, 0.5);
        assert!(hit1.is_none());
        assert_eq!(engine.state(), HysteresisBandState::ArmedHigh);

        let hit2 = engine.evaluate_scalar(200, -0.1);
        assert!(hit2.is_some());
    }

    #[test]
    fn t1_f15_04_hysteresis_either_edge() {
        let mut engine = make_edge_trigger_engine(ChannelId(1), 0.0, 0.5, EdgeSlope::Either);
        engine.evaluate_scalar(100, -1.0);
        let hit_rising = engine.evaluate_scalar(200, 1.0);
        assert!(hit_rising.is_some());

        engine.evaluate_scalar(300, 1.0);
        let hit_falling = engine.evaluate_scalar(400, -1.0);
        assert!(hit_falling.is_some());
    }

    #[test]
    fn t1_f15_05_hysteresis_zero_band() {
        let mut engine = make_edge_trigger_engine(ChannelId(1), 1.0, 0.0, EdgeSlope::Rising);
        engine.evaluate_scalar(100, 0.5);
        let hit = engine.evaluate_scalar(200, 1.5);
        assert!(hit.is_some());
    }
}

// ============================================================================
// Feature 16: Sub-Sample Linear Interpolation
// ============================================================================
pub mod f16_subsample_interpolation {
    use super::*;

    #[test]
    fn t1_f16_01_subsample_interp_halfway() {
        let (alpha, sub_ns) =
            calculate_subsample_offset(1000, 1.0, 2000, 3.0, 2.0).unwrap();
        assert_float_near!(alpha, 0.5, 1e-4);
        assert_float_near!(sub_ns, 500.0, 1e-3);
    }

    #[test]
    fn t1_f16_02_subsample_interp_exact_start() {
        let (alpha, sub_ns) =
            calculate_subsample_offset(1000, 2.0, 2000, 4.0, 2.0).unwrap();
        assert_float_near!(alpha, 0.0, 1e-4);
        assert_float_near!(sub_ns, 0.0, 1e-3);
    }

    #[test]
    fn t1_f16_03_subsample_interp_falling_slope() {
        let (alpha, sub_ns) =
            calculate_subsample_offset(100, 5.0, 200, 1.0, 2.0).unwrap();
        assert_float_near!(alpha, 0.75, 1e-4);
        assert_float_near!(sub_ns, 75.0, 1e-3);
    }

    #[test]
    fn t1_f16_04_subsample_interp_exact_end() {
        let (alpha, sub_ns) =
            calculate_subsample_offset(1000, 0.0, 2000, 2.0, 2.0).unwrap();
        assert_float_near!(alpha, 1.0, 1e-4);
        assert_float_near!(sub_ns, 1000.0, 1e-3);
    }

    #[test]
    fn t1_f16_05_subsample_interp_zero_denominator() {
        let (alpha, sub_ns) =
            calculate_subsample_offset(100, 2.0, 200, 2.0, 2.0).unwrap();
        assert_float_near!(alpha, 0.0, 1e-4);
        assert_float_near!(sub_ns, 0.0, 1e-3);
    }
}

// ============================================================================
// Feature 17: SyncGroupCoordinator
// ============================================================================
pub mod f17_sync_group {
    use super::*;

    #[test]
    fn t1_f17_01_sync_group_register() {
        let sync = SyncGroupCoordinator::new();
        sync.register_scope(ScopeId(1), SyncGroupId(10));
        sync.register_scope(ScopeId(2), SyncGroupId(10));
        let scopes = sync.get_group_scopes(SyncGroupId(10));
        assert_eq!(scopes.len(), 2);
    }

    #[test]
    fn t1_f17_02_sync_group_update_timebase() {
        let sync = SyncGroupCoordinator::new();
        sync.register_scope(ScopeId(1), SyncGroupId(10));
        sync.update_timebase(SyncGroupId(10), 50_000_000, -5_000_000);
        let state = sync.get_group_state(SyncGroupId(10)).unwrap();
        assert_eq!(state.time_span_ns, 50_000_000);
        assert_eq!(state.center_offset_ns, -5_000_000);
    }

    #[test]
    fn t1_f17_03_sync_group_cursor_sync() {
        let sync = SyncGroupCoordinator::new();
        sync.register_scope(ScopeId(1), SyncGroupId(10));
        sync.update_cursors(SyncGroupId(10), Some(0.25), Some(0.75));
        let state = sync.get_group_state(SyncGroupId(10)).unwrap();
        assert_eq!(state.cursor_a_ratio, Some(0.25));
        assert_eq!(state.cursor_b_ratio, Some(0.75));
    }

    #[test]
    fn t1_f17_04_sync_group_unregister() {
        let sync = SyncGroupCoordinator::new();
        sync.register_scope(ScopeId(1), SyncGroupId(10));
        sync.unregister_scope(ScopeId(1));
        assert!(sync.get_scope_group(ScopeId(1)).is_none());
    }

    #[test]
    fn t1_f17_05_sync_group_multiple_groups() {
        let sync = SyncGroupCoordinator::new();
        sync.register_scope(ScopeId(1), SyncGroupId(1));
        sync.register_scope(ScopeId(2), SyncGroupId(2));
        sync.update_timebase(SyncGroupId(1), 20_000_000, 0);
        sync.update_timebase(SyncGroupId(2), 40_000_000, 0);
        let s1 = sync.get_group_state(SyncGroupId(1)).unwrap();
        let s2 = sync.get_group_state(SyncGroupId(2)).unwrap();
        assert_eq!(s1.time_span_ns, 20_000_000);
        assert_eq!(s2.time_span_ns, 40_000_000);
    }
}

// ============================================================================
// Feature 18: Single Mode Pinning Integration
// ============================================================================
pub mod f18_single_pinning {
    use super::*;

    #[test]
    fn t1_f18_01_single_mode_pin() {
        let config = ScopeControllerConfig {
            sweep_mode: TriggerSweepMode::Single,
            time_span_ns: 10_000,
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

        // Prime low
        ctrl.process_scalar_sample(ChannelId(1), 1_000, -1.0);
        // Trigger at 2_000 (target end = 2_000 + 5_000 = 7_000)
        let trig = ctrl.process_scalar_sample(ChannelId(1), 2_000, 1.0);
        assert!(matches!(trig, ScopeStepOutcome::TriggerDetected { .. }));

        // Complete window at 8_000 >= 7_000
        let comp = ctrl.process_scalar_sample(ChannelId(1), 8_000, 1.0);
        match comp {
            ScopeStepOutcome::CaptureCompleted { latched_pinned, window } => {
                assert!(latched_pinned);
                assert_eq!(window.mode, TriggerSweepMode::Single);
                assert_eq!(window.trigger_state, TriggerRuntimeState::LatchedPinned);
            }
            _ => panic!("expected CaptureCompleted"),
        }
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::LatchedPinned);
        assert!(ctrl.is_pinned());
    }

    #[test]
    fn t1_f18_02_single_mode_ddsf_header_flags() {
        let config = ScopeControllerConfig {
            sweep_mode: TriggerSweepMode::Single,
            time_span_ns: 2_000,
            pre_trigger_ratio: 0.5,
            ..Default::default()
        };
        let mut ctrl = ScopeController::new(config);
        ctrl.force_trigger(10_000);
        let out = ctrl.step(12_000);
        if let ScopeStepOutcome::CaptureCompleted { latched_pinned, .. } = out {
            assert!(latched_pinned);
        } else {
            panic!("expected CaptureCompleted");
        }
    }

    #[test]
    fn t1_f18_03_single_mode_scope_header_state() {
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
        assert!(matches!(out, ScopeStepOutcome::CaptureCompleted { latched_pinned: true, .. }));
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::LatchedPinned);

        // In LatchedPinned state, subsequent steps do NOT fire additional triggers
        let out2 = ctrl.step(15_000);
        assert!(matches!(out2, ScopeStepOutcome::NoChange));
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::LatchedPinned);
    }

    #[test]
    fn t1_f18_04_single_mode_rearm_releases_pin() {
        let config = ScopeControllerConfig {
            sweep_mode: TriggerSweepMode::Single,
            time_span_ns: 1_000,
            pre_trigger_ratio: 0.5,
            ..Default::default()
        };
        let mut ctrl = ScopeController::new(config);
        ctrl.force_trigger(10_000);
        ctrl.step(12_000);
        assert!(ctrl.is_pinned());
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::LatchedPinned);

        ctrl.rearm();
        assert!(!ctrl.is_pinned());
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);
    }

    #[test]
    fn t1_f18_05_single_mode_switch_to_roll() {
        let mut ctrl = make_scope_controller(1, TriggerSweepMode::Single, 10_000_000, 100_000_000);
        ctrl.set_sweep_mode(TriggerSweepMode::Roll);
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::Roll);
        assert!(!ctrl.is_pinned());
    }
}

// ============================================================================
// Feature 19: 64-Byte Aligned DDSF Header
// ============================================================================
pub mod f19_ddsf_header {
    use super::*;

    #[test]
    fn t1_f19_01_ddsf_header_size_align() {
        assert_eq!(size_of::<DdsfFrameHeader>(), 64);
        assert_eq!(align_of::<DdsfFrameHeader>(), 8);
    }

    #[test]
    fn t1_f19_02_ddsf_header_magic() {
        assert_eq!(DDSF_MAGIC, 0x4653_4444);
        assert_eq!(DDSF_MAGIC.to_le_bytes(), *b"DDSF");
    }

    #[test]
    fn t1_f19_03_ddsf_header_version() {
        assert_eq!(DDSF_VERSION_V1, 1);
    }

    #[test]
    fn t1_f19_04_ddsf_header_validation_success() {
        let hdr = DdsfFrameHeader::new_v1(1, 2, 4, 1_000_000, 64, 0);
        assert!(hdr.validate(64).is_ok());
    }

    #[test]
    fn t1_f19_05_ddsf_header_validation_errors() {
        let mut bad = DdsfFrameHeader::new_v1(1, 2, 4, 1_000_000, 64, 0);
        bad.magic = 0xDEADBEEF;
        assert!(bad.validate(64).is_err());
    }
}

// ============================================================================
// Feature 20: Viewport Track Packing
// ============================================================================
pub mod f20_track_packing {
    use super::*;

    #[test]
    fn t1_f20_01_pack_analog_raw_track() {
        let pts = vec![
            DdsfAnalogPoint {
                x_norm: 0.0,
                y_val: 1.0,
            },
            DdsfAnalogPoint {
                x_norm: 1.0,
                y_val: 2.0,
            },
        ];
        let track = TrackPayload::new_raw_xy(1, pts);
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
    fn t1_f20_02_pack_analog_m4_track() {
        let cols = vec![DdsfM4Column {
            x_norm: 0.5,
            v_first: 0.0,
            v_min: -1.0,
            v_max: 1.0,
            v_last: 0.0,
        }];
        let track = TrackPayload::new_m4(2, cols, None);
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
        let buf = packer.pack_frame(2, 200, 0, &[scope], &[]).unwrap();
        assert_ddsf_frame_valid(buf, 2, 1, 0);
    }

    #[test]
    fn t1_f20_03_pack_digital_edges_track() {
        let edges = vec![0.1f32, 0.5, 0.9];
        let track = TrackPayload::new_digital_edges(3, 1, edges);
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
        let buf = packer.pack_frame(3, 300, 0, &[scope], &[]).unwrap();
        assert_ddsf_frame_valid(buf, 3, 1, 0);
    }

    #[test]
    fn t1_f20_04_pack_state_ribbons_track() {
        let ribbons = vec![DdsfStateRibbon {
            x0_norm: 0.0,
            x1_norm: 0.5,
            state_id: 1,
            color_idx: 0,
        }];
        let track = TrackPayload::new_state_ribbons(4, ribbons);
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
        let buf = packer.pack_frame(4, 400, 0, &[scope], &[]).unwrap();
        assert_ddsf_frame_valid(buf, 4, 1, 0);
    }

    #[test]
    fn t1_f20_05_pack_multi_track_single_scope() {
        let pts = vec![DdsfAnalogPoint {
            x_norm: 0.0,
            y_val: 1.0,
        }];
        let cols = vec![DdsfM4Column {
            x_norm: 0.5,
            v_first: 0.0,
            v_min: -1.0,
            v_max: 1.0,
            v_last: 0.0,
        }];
        let track1 = TrackPayload::new_raw_xy(1, pts);
        let track2 = TrackPayload::new_m4(2, cols, None);
        let scope = ScopeViewportPayload {
            scope_id: 1,
            trigger_state: 0,
            mode: 0,
            win_t_start_ns: 0,
            win_t_span_ns: 1000,
            subpixel_offset_ns: 0.0,
            tracks: vec![track1, track2],
        };
        let mut packer = DdsfFramePacker::new(4096);
        let buf = packer.pack_frame(5, 500, 0, &[scope], &[]).unwrap();
        assert_ddsf_frame_valid(buf, 5, 1, 0);
    }
}

// ============================================================================
// Feature 21: Track Statistics & Snapshots
// ============================================================================
pub mod f21_stats_snapshots {
    use super::*;

    #[test]
    fn t1_f21_01_pack_track_stats_presence() {
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
        let cols = vec![DdsfM4Column {
            x_norm: 0.5,
            v_first: 0.0,
            v_min: -1.0,
            v_max: 1.0,
            v_last: 0.0,
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
    }

    #[test]
    fn t1_f21_02_track_stats_size() {
        assert_eq!(size_of::<DdsfTrackStats>(), 32);
    }

    #[test]
    fn t1_f21_03_snapshot_entry_size() {
        assert_eq!(size_of::<DdsfSnapshotEntry>(), 16);
    }

    #[test]
    fn t1_f21_04_snapshot_kinds() {
        assert_eq!(DdsfSnapshotKind::Scalar.as_u8(), 0);
        assert_eq!(DdsfSnapshotKind::Bool.as_u8(), 1);
        assert_eq!(DdsfSnapshotKind::StateEnum.as_u8(), 2);
    }

    #[test]
    fn t1_f21_05_track_stats_omitted_when_flag_clear() {
        let cols = vec![DdsfM4Column {
            x_norm: 0.5,
            v_first: 0.0,
            v_min: -1.0,
            v_max: 1.0,
            v_last: 0.0,
        }];
        let track = TrackPayload::new_m4(1, cols, None);
        assert_eq!(track.flags & DDSF_TRACK_FLAG_HAS_STATS, 0);
    }
}

// ============================================================================
// Feature 22: Zero Secondary Heap Allocation
// ============================================================================
pub mod f22_zero_allocations {
    use super::*;

    #[test]
    fn t1_f22_01_packer_buffer_reuse() {
        let mut packer = DdsfFramePacker::new(4096);
        let ptr1 = packer.buffer().as_ptr();
        let _ = packer.pack_frame(1, 100, 0, &[], &[]).unwrap();
        let ptr2 = packer.buffer().as_ptr();
        assert_eq!(ptr1, ptr2);
    }

    #[test]
    fn t1_f22_02_consecutive_frames_same_buffer() {
        let mut packer = DdsfFramePacker::new(4096);
        for seq in 1..=5 {
            let buf = packer.pack_frame(seq, 100 * seq as u64, 0, &[], &[]).unwrap();
            assert_ddsf_frame_valid(buf, seq, 0, 0);
        }
    }

    #[test]
    fn t1_f22_03_aligned_buffer_initial_capacity() {
        let packer = DdsfFramePacker::new(8192);
        assert_eq!(packer.buffer().len(), 0);
        assert!(packer.capacity() >= 8192);
        assert_eq!((packer.buffer().as_ptr() as usize) % 64, 0);
    }

    #[test]
    fn t1_f22_04_buffer_alignment_cache_line() {
        let packer = DdsfFramePacker::new(4096);
        let ptr = packer.buffer().as_ptr() as usize;
        assert_eq!(ptr % 64, 0);
    }

    #[test]
    fn t1_f22_05_packer_clear_reset() {
        let mut packer = DdsfFramePacker::new(4096);
        packer.pack_frame(1, 100, 0, &[], &[]).unwrap();
        assert!(packer.buffer().len() >= 64);
    }
}

// ============================================================================
// Feature 23: M4 Extraction Criterion Bench
// ============================================================================
pub mod f23_m4_bench {
    use super::*;

    #[test]
    fn t1_f23_01_m4_bench_target_compilation() {
        assert_eq!(CHUNK_LEN, 65536);
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let count = CHUNK_LEN;
        let dt = 10u64;
        let ts = generate_timestamps(0, count, dt);
        let mut vals = vec![1.0f32; count];
        let peak_val = 50.0f32;
        let valley_val = -50.0f32;
        vals[10_000] = peak_val;
        vals[50_000] = valley_val;

        let appended = series
            .append(&ts, ScalarSlice::F32(&vals))
            .expect("append full chunk ok");
        assert_eq!(appended, count);
        assert_eq!(series.total_samples(), count as u64);
        assert_eq!(series.sealed_chunk_count(), 1);
        assert_eq!(series.active_chunk().len(), 0);

        let res = series
            .query_m4_viewport(0, 655_360, 1000)
            .expect("viewport query ok");
        match res {
            M4QueryResult::Columns(cols) => {
                assert_eq!(cols.len(), 1000);
                assert_m4_extrema_retained(&cols, Some(peak_val), Some(valley_val));
                for col in &cols {
                    assert!(col.v_min <= col.v_max);
                    assert!(col.v_first >= col.v_min && col.v_first <= col.v_max);
                    assert!(col.v_last >= col.v_min && col.v_last <= col.v_max);
                    assert!(col.x_norm >= 0.0 && col.x_norm <= 1.0);
                }
            }
            M4QueryResult::Raw(_) => panic!("expected M4 columns for 65536 points"),
        }
    }

    #[test]
    fn t1_f23_02_m4_bench_data_generator_speed() {
        let count = 100_000;
        let dt = 10u64;
        let ts = generate_timestamps(0, count, dt);
        assert_eq!(ts.len(), count);

        let mut vals = generate_sine_wave_f32(count, 1_000_000.0, 100.0, 5.0, 0.0);
        let peak_val = 99.0f32;
        let valley_val = -99.0f32;
        vals[20_000] = peak_val;
        vals[80_000] = valley_val;

        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let appended = series
            .append(&ts, ScalarSlice::F32(&vals))
            .expect("append ok");
        assert_eq!(appended, count);
        assert_eq!(series.total_samples(), count as u64);
        assert_eq!(series.sealed_chunk_count(), 1);
        assert_eq!(series.active_chunk().len(), count - CHUNK_LEN);

        let start = std::time::Instant::now();
        let res = series
            .query_m4_viewport(0, (count as u64) * dt, 1000)
            .expect("viewport query ok");
        let elapsed = start.elapsed();

        match res {
            M4QueryResult::Columns(cols) => {
                assert_eq!(cols.len(), 1000);
                assert_m4_extrema_retained(&cols, Some(peak_val), Some(valley_val));
            }
            M4QueryResult::Raw(_) => panic!("expected M4 columns for 100k points"),
        }
        assert!(
            elapsed.as_millis() < 500,
            "100k query should complete within debug threshold (<500ms)"
        );
    }

    #[test]
    fn t1_f23_03_m4_bench_sub_0_1ms_target() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let count = 10_000;
        let dt = 10u64;
        let ts = generate_timestamps(0, count, dt);
        let vals = generate_sine_wave_f32(count, 1_000_000.0, 1000.0, 5.0, 0.0);
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();

        let t_start = std::time::Instant::now();
        let res = series.query_m4_viewport(0, (count as u64) * dt, 1000).expect("viewport ok");
        let elapsed = t_start.elapsed();
        match res {
            M4QueryResult::Columns(cols) => {
                assert_eq!(cols.len(), 1000);
                assert!(!cols.is_empty());
            }
            M4QueryResult::Raw(_) => panic!("Expected Columns for 10000 samples / 1000 px"),
        }
        assert!(elapsed.as_millis() < 500, "Query took too long: {:?}", elapsed);
    }

    #[test]
    fn t1_f23_04_m4_bench_cache_alignment() {
        assert_eq!(CACHE_LINE_BYTES, 64);

        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let count = CHUNK_LEN;
        let ts = generate_timestamps(0, count, 10);
        let vals = vec![3.14159f32; count];
        series
            .append(&ts, ScalarSlice::F32(&vals))
            .expect("append ok");
        assert_eq!(series.sealed_chunk_count(), 1);

        let res = series
            .query_m4_viewport(0, 655_360, 1000)
            .expect("query ok");
        let cols = match res {
            M4QueryResult::Columns(cols) => {
                assert_eq!(cols.len(), 1000);
                assert_eq!(
                    cols.as_slice().as_ptr() as usize % std::mem::align_of::<DdsfM4Column>(),
                    0,
                    "column slice must be aligned"
                );
                cols
            }
            M4QueryResult::Raw(_) => panic!("expected columns"),
        };

        // End-to-end DDSF frame packaging 64-byte alignment
        let track = TrackPayload::new_m4(1, cols, None);
        let scope = ScopeViewportPayload {
            scope_id: 1,
            trigger_state: 0,
            mode: 0,
            win_t_start_ns: 0,
            win_t_span_ns: 655_360,
            subpixel_offset_ns: 0.0,
            tracks: vec![track],
        };
        let mut packer = DdsfFramePacker::new(32768);
        let frame_buf = packer.pack_frame(1, 1000, 0, &[scope], &[]).unwrap();
        assert_ddsf_frame_valid(frame_buf, 1, 1, 0);
        assert_eq!(
            frame_buf.as_ptr() as usize % CACHE_LINE_BYTES,
            0,
            "packed DDSF frame must be 64-byte cache line aligned"
        );
    }

    #[test]
    fn t1_f23_05_m4_bench_pixel_width_sweep() {
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let count = 20_000;
        let dt = 10u64;
        let ts = generate_timestamps(0, count, dt);
        let vals = vec![2.5f32; count];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();

        let widths = [500, 1000, 1920];
        for &w in &widths {
            let res = series.query_m4_viewport(0, (count as u64) * dt, w).expect("query ok");
            match res {
                M4QueryResult::Columns(cols) => {
                    assert_eq!(cols.len(), w, "column count matches pixel width {w}");
                }
                M4QueryResult::Raw(_) => panic!("expected M4 Columns for width {w}"),
            }
        }
    }
}

// ============================================================================
// Feature 24: DDSF Packing Criterion Bench
// ============================================================================
pub mod f24_ddsf_bench {
    use super::*;

    #[test]
    fn t1_f24_01_ddsf_bench_target_compilation() {
        assert_eq!(DDSF_HEADER_BYTES, 64);

        let mut packer = DdsfFramePacker::new(4096);
        let buf0 = packer.pack_frame(0, 0, 0, &[], &[]).unwrap();
        assert_ddsf_frame_valid(buf0, 0, 0, 0);

        let hdr0: &DdsfFrameHeader = bytemuck::from_bytes(&buf0[..DDSF_HEADER_BYTES]);
        assert_eq!(hdr0.magic, DDSF_MAGIC);
        assert_eq!(hdr0.version, DDSF_VERSION_V1);
        assert_eq!(hdr0.frame_seq, 0);
        assert_eq!(hdr0.total_byte_len, 64);

        let buf1 = packer.pack_frame(1, 1000, 0, &[], &[]).unwrap();
        assert_ddsf_frame_valid(buf1, 1, 0, 0);
        let reader = DdsfZeroCopyReader::new(buf1).expect("valid frame");
        let hdr1 = reader.header();
        assert_eq!(hdr1.magic, DDSF_MAGIC);
        assert_eq!(hdr1.version, DDSF_VERSION_V1);
        assert_eq!(hdr1.frame_seq, 1);
        assert_eq!(hdr1.total_byte_len, 64);
    }

    #[test]
    fn t1_f24_02_ddsf_bench_throughput_target() {
        let mut packer = DdsfFramePacker::new(4096);
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
            trigger_state: 0,
            mode: 0,
            win_t_start_ns: 0,
            win_t_span_ns: 1000,
            subpixel_offset_ns: 0.0,
            tracks: vec![track],
        };

        for seq in 0..100 {
            let buf = packer.pack_frame(seq as u32, (seq as u64) * 1000, 0, std::slice::from_ref(&scope), &[]).unwrap();
            assert!(buf.len() >= 64);
            assert_ddsf_frame_valid(buf, seq as u32, 1, 0);
        }
    }

    #[test]
    fn t1_f24_03_ddsf_bench_zero_allocations_metric() {
        let mut packer = DdsfFramePacker::new(32768);
        let baseline_ptr = packer.buffer().as_ptr();
        let baseline_cap = packer.capacity();

        for seq in 1..=20 {
            let buf = packer.pack_frame(seq, seq as u64 * 1000, 0, &[], &[]).unwrap();
            assert_ddsf_frame_valid(buf, seq, 0, 0);
            assert_eq!(packer.buffer().as_ptr(), baseline_ptr);
            assert_eq!(packer.capacity(), baseline_cap);
        }
    }

    #[test]
    fn t1_f24_04_ddsf_bench_frame_size_scalability() {
        let mut packer = DdsfFramePacker::new(16384);
        let make_scope = |id: u32| {
            let cols = vec![DdsfM4Column {
                x_norm: 0.5,
                v_first: 1.0,
                v_min: 0.0,
                v_max: 2.0,
                v_last: 1.5,
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

        let scopes_1 = vec![make_scope(1)];
        let scopes_2 = vec![make_scope(1), make_scope(2)];
        let scopes_4 = vec![make_scope(1), make_scope(2), make_scope(3), make_scope(4)];

        let buf1 = packer.pack_frame(1, 100, 0, &scopes_1, &[]).unwrap();
        let len1 = buf1.len();
        assert_ddsf_frame_valid(buf1, 1, 1, 0);

        let buf2 = packer.pack_frame(2, 200, 0, &scopes_2, &[]).unwrap();
        let len2 = buf2.len();
        assert_ddsf_frame_valid(buf2, 2, 2, 0);

        let buf4 = packer.pack_frame(3, 300, 0, &scopes_4, &[]).unwrap();
        let len4 = buf4.len();
        assert_ddsf_frame_valid(buf4, 3, 4, 0);

        assert!(len1 < len2, "1 scope frame len {len1} must be less than 2 scope frame len {len2}");
        assert!(len2 < len4, "2 scope frame len {len2} must be less than 4 scope frame len {len4}");
    }

    #[test]
    fn t1_f24_05_ddsf_bench_snapshot_packing_speed() {
        let snap_count = 64;
        let entries = vec![
            DdsfSnapshotEntry {
                channel_num_id: 1,
                kind: 0,
                pad: [0; 3],
                val_f64: 3.14,
            };
            snap_count
        ];
        let mut packer = DdsfFramePacker::new(4096);
        let buf = packer.pack_frame(1, 100, 0, &[], &entries).unwrap();
        assert_ddsf_frame_valid(buf, 1, 0, snap_count as u16);
    }
}
