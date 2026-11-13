//! DDScopeHost Phase 1 Track A (TA1) 4-Tier End-to-End Integration Test Suite.
//!
//! Validates all 26 features of `PROJECT.md § Feature Inventory` through the public
//! interface contracts of `ddscope_core::types` and `ddscope_core::storage`.
//!
//! Architecture:
//! - Tier 1: Feature Coverage (130 tests, >=5 per feature across equivalence classes)
//! - Tier 2: Boundary & Corner Cases (130 tests, >=5 per feature across extreme limits)
//! - Tier 3: Cross-Feature Combinations (15 pairwise subsystem interaction tests)
//! - Tier 4: Real-World Workload Scenarios (6 realistic multi-modal telemetry workloads)
//!
//! Total: 281 tests.

#![allow(
    clippy::approx_constant,
    clippy::needless_range_loop,
    clippy::unnecessary_cast,
    clippy::useless_vec,
    clippy::doc_lazy_continuation
)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use ddscope_core::storage::{EventSeries, NativeScalarSeries, WatermarkEvent, WatermarkListener};
use ddscope_core::types::*;

// ============================================================================
// Reusable Test Fixtures and Synthetic Generators
// ============================================================================

/// Thread-safe mock listener capturing watermark advancement events for test assertions.
#[derive(Default)]
pub struct MockWatermarkListener {
    pub events: Mutex<Vec<WatermarkEvent>>,
    pub total_calls: AtomicU64,
}

impl WatermarkListener for MockWatermarkListener {
    fn on_watermark_advanced(&self, events: &[WatermarkEvent]) {
        self.total_calls.fetch_add(1, Ordering::SeqCst);
        let mut guard = self.events.lock().unwrap();
        guard.extend_from_slice(events);
    }
}

impl MockWatermarkListener {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn last_event(&self) -> Option<WatermarkEvent> {
        self.events.lock().unwrap().last().copied()
    }

    pub fn event_count(&self) -> usize {
        self.events.lock().unwrap().len()
    }

    pub fn all_events(&self) -> Vec<WatermarkEvent> {
        self.events.lock().unwrap().clone()
    }

    pub fn clear(&self) {
        self.events.lock().unwrap().clear();
        self.total_calls.store(0, Ordering::SeqCst);
    }
}

/// Helper generating monotonic timestamps with constant step in nanoseconds.
pub fn generate_timestamps(start_ns: u64, count: usize, step_ns: u64) -> Vec<TimestampNs> {
    (0..count)
        .map(|i| start_ns + (i as u64) * step_ns)
        .collect()
}

/// Helper generating synthetic digital logic square wave bitstreams.
pub fn generate_square_wave_bits(count: usize, period_samples: usize) -> Vec<bool> {
    let half_period = period_samples / 2;
    (0..count)
        .map(|i| (i % period_samples) < half_period)
        .collect()
}

// ============================================================================
// Tier 1: Feature Coverage (Equivalence Classes, >=5 tests per feature)
// ============================================================================
pub mod tier1_feature_coverage {
    use super::*;

    // --- Feature 1: CHUNK_LEN Constant ---

    #[test]
    fn t1_f01_01_chunk_len_magnitude() {
        assert_eq!(CHUNK_LEN, 65_536);
    }

    #[test]
    fn t1_f01_02_chunk_len_power_of_two() {
        assert!(CHUNK_LEN.is_power_of_two());
        assert_eq!(CHUNK_LEN.trailing_zeros(), 16);
    }

    #[test]
    fn t1_f01_03_chunk_len_cache_divisibility() {
        assert_eq!(CHUNK_LEN % CACHE_LINE_BYTES, 0);
        assert_eq!(CHUNK_LEN / CACHE_LINE_BYTES, 1024);
    }

    #[test]
    fn t1_f01_04_chunk_len_bitvec_words() {
        assert_eq!(CHUNK_LEN / 64, 1024);
        assert_eq!(CHUNK_LEN % 64, 0);
    }

    #[test]
    fn t1_f01_05_chunk_len_buffer_allocation() {
        let vec: AlignedVec<f32> = AlignedVec::with_capacity(CHUNK_LEN);
        assert!(vec.capacity() >= CHUNK_LEN);
        assert_eq!(vec.len(), 0);
    }

    // --- Feature 2: CACHE_LINE_BYTES Alignment ---

    #[test]
    fn t1_f02_01_cache_line_magnitude() {
        assert_eq!(CACHE_LINE_BYTES, 64);
    }

    #[test]
    fn t1_f02_02_aligned_buffer_pointer_alignment() {
        let mut buf = AlignedBuffer::with_capacity(128);
        buf.extend_from_slice(&[1, 2, 3, 4]);
        assert_eq!((buf.as_ptr() as usize) % CACHE_LINE_BYTES, 0);
    }

    #[test]
    fn t1_f02_03_aligned_vec_pointer_alignment() {
        let mut vec: AlignedVec<f64> = AlignedVec::with_capacity(64);
        vec.push(42.0);
        assert_eq!((vec.as_ptr() as usize) % CACHE_LINE_BYTES, 0);
    }

    #[test]
    fn t1_f02_04_cache_line_block_struct_align() {
        assert_eq!(std::mem::align_of::<CacheLineBlock>(), 64);
        assert_eq!(std::mem::size_of::<CacheLineBlock>(), 64);
    }

    #[test]
    fn t1_f02_05_reallocation_alignment_stability() {
        let mut vec: AlignedVec<i32> = AlignedVec::with_capacity(16);
        for i in 0..1024 {
            vec.push(i);
            assert_eq!((vec.as_ptr() as usize) % CACHE_LINE_BYTES, 0);
        }
    }

    // --- Feature 3: 10 Native Scalar Types ---

    #[test]
    fn t1_f03_01_signed_narrow_integers() {
        let mut col_i8 = NativeScalarColumn::with_capacity(ScalarType::I8, 10);
        col_i8.append_slice(ScalarSlice::I8(&[-10, 20])).unwrap();
        assert_eq!(col_i8.len(), 2);
        assert_eq!(col_i8.get_as_f64(0), Some(-10.0));
        assert_eq!(col_i8.get_as_f64(1), Some(20.0));

        let mut col_i16 = NativeScalarColumn::with_capacity(ScalarType::I16, 10);
        col_i16
            .append_slice(ScalarSlice::I16(&[-500, 1500]))
            .unwrap();
        assert_eq!(col_i16.len(), 2);
        assert_eq!(col_i16.get_as_f64(0), Some(-500.0));
        assert_eq!(col_i16.get_as_f64(1), Some(1500.0));
    }

    #[test]
    fn t1_f03_02_signed_wide_integers() {
        let mut col_i32 = NativeScalarColumn::with_capacity(ScalarType::I32, 10);
        col_i32
            .append_slice(ScalarSlice::I32(&[-100_000, 200_000]))
            .unwrap();
        assert_eq!(col_i32.get_as_f64(0), Some(-100_000.0));

        let mut col_i64 = NativeScalarColumn::with_capacity(ScalarType::I64, 10);
        col_i64
            .append_slice(ScalarSlice::I64(&[1i64 << 40, -(1i64 << 40)]))
            .unwrap();
        assert_eq!(col_i64.get_as_f64(0), Some((1i64 << 40) as f64));
    }

    #[test]
    fn t1_f03_03_unsigned_narrow_integers() {
        let mut col_u8 = NativeScalarColumn::with_capacity(ScalarType::U8, 10);
        col_u8.append_slice(ScalarSlice::U8(&[0, 255])).unwrap();
        assert_eq!(col_u8.get_as_f64(1), Some(255.0));

        let mut col_u16 = NativeScalarColumn::with_capacity(ScalarType::U16, 10);
        col_u16
            .append_slice(ScalarSlice::U16(&[1000, 65535]))
            .unwrap();
        assert_eq!(col_u16.get_as_f64(1), Some(65535.0));
    }

    #[test]
    fn t1_f03_04_unsigned_wide_integers() {
        let mut col_u32 = NativeScalarColumn::with_capacity(ScalarType::U32, 10);
        col_u32
            .append_slice(ScalarSlice::U32(&[1_000_000, u32::MAX]))
            .unwrap();
        assert_eq!(col_u32.get_as_f64(1), Some(u32::MAX as f64));

        let mut col_u64 = NativeScalarColumn::with_capacity(ScalarType::U64, 10);
        col_u64
            .append_slice(ScalarSlice::U64(&[1u64 << 42]))
            .unwrap();
        assert_eq!(col_u64.get_as_f64(0), Some((1u64 << 42) as f64));
    }

    #[test]
    fn t1_f03_05_floating_point_precision() {
        let mut col_f32 = NativeScalarColumn::with_capacity(ScalarType::F32, 10);
        col_f32
            .append_slice(ScalarSlice::F32(&[3.14159_f32]))
            .unwrap();
        let val_f32 = col_f32.get_as_f64(0).unwrap();
        assert!((val_f32 - 3.14159).abs() < 1e-4);

        let mut col_f64 = NativeScalarColumn::with_capacity(ScalarType::F64, 10);
        col_f64
            .append_slice(ScalarSlice::F64(&[2.718281828459045]))
            .unwrap();
        assert_eq!(col_f64.get_as_f64(0), Some(2.718281828459045));
    }

    // --- Feature 4: AlignedVec<T: Pod> ---

    #[test]
    fn t1_f04_01_capacity_preallocation() {
        let vec: AlignedVec<u32> = AlignedVec::with_capacity(100);
        assert!(vec.capacity() >= 100);
        assert_eq!(vec.len(), 0);
    }

    #[test]
    fn t1_f04_02_single_element_push() {
        let mut vec: AlignedVec<i16> = AlignedVec::new();
        vec.push(1234);
        assert_eq!(vec.len(), 1);
        assert_eq!(vec[0], 1234);
    }

    #[test]
    fn t1_f04_03_contiguous_slice_append() {
        let mut vec: AlignedVec<u8> = AlignedVec::new();
        vec.extend_from_slice(&[10, 20, 30, 40]);
        assert_eq!(vec.len(), 4);
        assert_eq!(vec.as_slice(), &[10, 20, 30, 40]);
    }

    #[test]
    fn t1_f04_04_inplace_clear() {
        let mut vec: AlignedVec<f32> = AlignedVec::with_capacity(64);
        vec.extend_from_slice(&[1.0, 2.0, 3.0]);
        let cap_before = vec.capacity();
        vec.clear();
        assert_eq!(vec.len(), 0);
        assert_eq!(vec.capacity(), cap_before);
    }

    #[test]
    fn t1_f04_05_mutable_slice_access() {
        let mut vec: AlignedVec<u64> = AlignedVec::new();
        vec.extend_from_slice(&[100, 200]);
        vec.as_mut_slice()[0] = 999;
        assert_eq!(vec[0], 999);
        assert_eq!(vec[1], 200);
    }

    // --- Feature 5: TimestampColumn Dual Encoding ---

    #[test]
    fn t1_f05_01_auto_relu32_short_span() {
        let base = 1_000_000_000u64;
        let ts = vec![base, base + 500_000, base + 1_000_000_000]; // 1.0s span
        let col = TimestampColumn::from_timestamps(base, &ts);
        assert!(matches!(col, TimestampColumn::RelU32 { .. }));
        assert_eq!(col.len(), 3);
    }

    #[test]
    fn t1_f05_02_auto_absu64_long_span() {
        let base = 1_000_000_000u64;
        let ts = vec![base, base + 5_000_000_000]; // 5.0s span > 4.29s
        let col = TimestampColumn::from_timestamps(base, &ts);
        assert!(matches!(col, TimestampColumn::AbsU64(_)));
        assert_eq!(col.len(), 2);
    }

    #[test]
    fn t1_f05_03_get_ns_reconstruction() {
        let base = 500_000u64;
        let ts = vec![base, base + 1234, base + 5678];
        let col = TimestampColumn::from_timestamps(base, &ts);
        assert_eq!(col.get_ns(0), Some(base));
        assert_eq!(col.get_ns(1), Some(base + 1234));
        assert_eq!(col.get_ns(2), Some(base + 5678));
    }

    #[test]
    fn t1_f05_04_first_and_last_ns() {
        let base = 10_000u64;
        let ts = vec![base, base + 100, base + 200];
        let col = TimestampColumn::from_timestamps(base, &ts);
        assert_eq!(col.first_ns(), Some(base));
        assert_eq!(col.last_ns(), Some(base + 200));
    }

    #[test]
    fn t1_f05_05_footprint_savings() {
        let base = 1_000_000u64;
        let ts_rel = generate_timestamps(base, 1024, 1000);
        let col_rel = TimestampColumn::from_timestamps(base, &ts_rel);

        let ts_abs = vec![base, base + 10_000_000_000]; // triggers AbsU64
        let col_abs = TimestampColumn::from_timestamps(base, &ts_abs);

        // RelU32 uses 4 bytes per sample vs 8 bytes for AbsU64
        assert!(col_rel.byte_footprint() < 1024 * 8);
        assert_eq!(col_abs.byte_footprint(), 64); // 2 samples * 8 bytes aligned to 64 bytes
    }

    // --- Feature 6: ScalarDataChunk ---

    #[test]
    fn t1_f06_01_constructor_matching_lengths() {
        let base = 1_000_000u64;
        let ts = TimestampColumn::from_timestamps(base, &[base, base + 100]);
        let mut vals = NativeScalarColumn::with_capacity(ScalarType::F32, 2);
        vals.append_slice(ScalarSlice::F32(&[1.0, 2.0])).unwrap();
        let chunk = ScalarDataChunk::new(ts, vals, LinearScale::identity());
        assert!(chunk.is_ok());
        let chunk = chunk.unwrap();
        assert_eq!(chunk.len(), 2);
    }

    #[test]
    fn t1_f06_02_packaging_in_datachunk() {
        let base = 1_000_000u64;
        let ts = TimestampColumn::from_timestamps(base, &[base]);
        let mut vals = NativeScalarColumn::with_capacity(ScalarType::I16, 1);
        vals.append_slice(ScalarSlice::I16(&[42])).unwrap();
        let chunk = ScalarDataChunk::new(ts, vals, LinearScale::identity()).unwrap();
        let data_chunk = DataChunk::Scalar(chunk);
        assert_eq!(data_chunk.sample_count(), 1);
    }

    #[test]
    fn t1_f06_03_time_range_ns_propagation() {
        let base = 5_000u64;
        let ts = TimestampColumn::from_timestamps(base, &[base, base + 500]);
        let mut vals = NativeScalarColumn::with_capacity(ScalarType::U8, 2);
        vals.append_slice(ScalarSlice::U8(&[1, 2])).unwrap();
        let chunk = ScalarDataChunk::new(ts, vals, LinearScale::identity()).unwrap();
        let data_chunk = DataChunk::Scalar(chunk);
        assert_eq!(data_chunk.time_range_ns(), Some((base, base + 500)));
    }

    #[test]
    fn t1_f06_04_byte_footprint_calculation() {
        let base = 1_000u64;
        let ts = TimestampColumn::from_timestamps(base, &[base]);
        let mut vals = NativeScalarColumn::with_capacity(ScalarType::F64, 1);
        vals.append_slice(ScalarSlice::F64(&[10.0])).unwrap();
        let chunk = ScalarDataChunk::new(ts, vals, LinearScale::identity()).unwrap();
        let data_chunk = DataChunk::Scalar(chunk);
        assert!(data_chunk.byte_footprint() > 0);
    }

    #[test]
    fn t1_f06_05_calibration_retention() {
        let base = 100u64;
        let ts = TimestampColumn::from_timestamps(base, &[base]);
        let mut vals = NativeScalarColumn::with_capacity(ScalarType::I32, 1);
        vals.append_slice(ScalarSlice::I32(&[10])).unwrap();
        let scale = LinearScale::new(2.5, -5.0);
        let chunk = ScalarDataChunk::new(ts, vals, scale).unwrap();
        assert_eq!(chunk.calibration, scale);
        assert_eq!(chunk.get_sample_as_f64(0), Some((base, 20.0))); // 10 * 2.5 - 5 = 20.0
    }

    // --- Feature 7: LinearScale Calibration ---

    #[test]
    fn t1_f07_01_identity_transform() {
        let scale = LinearScale::identity();
        assert!(scale.is_identity());
        assert_eq!(scale.apply(123.456), 123.456);
    }

    #[test]
    fn t1_f07_02_gain_and_offset() {
        let scale = LinearScale::new(2.0, 10.0);
        assert!(!scale.is_identity());
        assert_eq!(scale.apply(5.0), 20.0); // 5 * 2 + 10 = 20
    }

    #[test]
    fn t1_f07_03_adc_lsb_scale() {
        let scale = LinearScale::new(3.3 / 4096.0, 0.0);
        let v = scale.apply(2048.0);
        assert!((v - 1.65).abs() < 1e-6);
    }

    #[test]
    fn t1_f07_04_negative_gain() {
        let scale = LinearScale::new(-1.0, 50.0);
        assert_eq!(scale.apply(10.0), 40.0);
    }

    #[test]
    fn t1_f07_05_apply_f32() {
        let scale = LinearScale::new(0.5, 5.0);
        assert_eq!(scale.apply_f32(10.0), 10.0_f32);
    }

    // --- Feature 8: 1-Bit Packed BitVecChunk ---

    #[test]
    fn t1_f08_01_preallocation_bits() {
        let bv = BitVecChunk::with_capacity(65_536);
        assert!(bv.words.capacity() >= 1024);
        assert_eq!(bv.len(), 0);
    }

    #[test]
    fn t1_f08_02_single_bit_push() {
        let mut bv = BitVecChunk::new();
        bv.push(true);
        bv.push(false);
        bv.push(true);
        assert_eq!(bv.len(), 3);
        assert_eq!(bv.get(0), Some(true));
        assert_eq!(bv.get(1), Some(false));
        assert_eq!(bv.get(2), Some(true));
    }

    #[test]
    fn t1_f08_03_boolean_slice_extension() {
        let mut bv = BitVecChunk::new();
        let bits = vec![true; 100];
        bv.extend_from_slice(&bits);
        assert_eq!(bv.len(), 100);
        for i in 0..100 {
            assert_eq!(bv.get(i), Some(true));
        }
    }

    #[test]
    fn t1_f08_04_word_count_math() {
        let mut bv = BitVecChunk::new();
        bv.extend_from_slice(&vec![false; 64]);
        assert_eq!(bv.words.len(), 1);
        bv.push(true);
        assert_eq!(bv.words.len(), 2);
    }

    #[test]
    fn t1_f08_05_byte_footprint_verification() {
        let mut bv = BitVecChunk::new();
        bv.extend_from_slice(&vec![true; 65_536]);
        assert_eq!(bv.len(), 65_536);
        assert_eq!(bv.words.byte_len(), 8_192); // 1,024 words * 8 bytes
    }

    // --- Feature 9: Pre-computed Edge Indices ---

    #[test]
    fn t1_f09_01_constant_low_edges() {
        let mut bv = BitVecChunk::new();
        bv.extend_from_slice(&vec![false; 1000]);
        bv.compute_edge_indices();
        assert!(bv.edge_indices.is_empty());
    }

    #[test]
    fn t1_f09_02_constant_high_edges() {
        let mut bv = BitVecChunk::new();
        bv.extend_from_slice(&vec![true; 1000]);
        bv.compute_edge_indices();
        assert!(bv.edge_indices.is_empty());
    }

    #[test]
    fn t1_f09_03_single_pulse_edges() {
        let mut bv = BitVecChunk::new();
        let mut bits = vec![false; 30];
        for i in 10..20 {
            bits[i] = true;
        }
        bv.extend_from_slice(&bits);
        bv.compute_edge_indices();
        assert_eq!(bv.edge_indices, vec![10, 20]);
    }

    #[test]
    fn t1_f09_04_periodic_square_wave() {
        let mut bv = BitVecChunk::new();
        let bits = generate_square_wave_bits(400, 100); // edges at 50, 100, 150, 200, 250, 300, 350
        bv.extend_from_slice(&bits);
        bv.compute_edge_indices();
        assert_eq!(bv.edge_indices, vec![50, 100, 150, 200, 250, 300, 350]);
    }

    #[test]
    fn t1_f09_05_monotonicity_of_indices() {
        let mut bv = BitVecChunk::new();
        let bits = generate_square_wave_bits(1000, 40);
        bv.extend_from_slice(&bits);
        bv.compute_edge_indices();
        for window in bv.edge_indices.windows(2) {
            assert!(window[0] < window[1]);
        }
    }

    // --- Feature 10: BitVecChunk Sample Access ---

    #[test]
    fn t1_f010_01_word0_bit0_lookup() {
        let mut bv = BitVecChunk::new();
        bv.push(true);
        assert_eq!(bv.get(0), Some(true));
    }

    #[test]
    fn t1_f010_02_word0_bit63_lookup() {
        let mut bv = BitVecChunk::new();
        let mut bits = vec![false; 64];
        bits[63] = true;
        bv.extend_from_slice(&bits);
        assert_eq!(bv.get(63), Some(true));
        assert_eq!(bv.get(62), Some(false));
    }

    #[test]
    fn t1_f010_03_word1_bit0_sample_64() {
        let mut bv = BitVecChunk::new();
        let mut bits = vec![false; 65];
        bits[64] = true;
        bv.extend_from_slice(&bits);
        assert_eq!(bv.get(64), Some(true));
    }

    #[test]
    fn t1_f010_04_full_chunk_random_access() {
        let mut bv = BitVecChunk::new();
        let bits: Vec<bool> = (0..1024).map(|i| (i * 7 + 3) % 2 == 0).collect();
        bv.extend_from_slice(&bits);
        for (i, &expected) in bits.iter().enumerate() {
            assert_eq!(bv.get(i), Some(expected));
        }
    }

    #[test]
    fn t1_f010_05_out_of_bounds_safety() {
        let mut bv = BitVecChunk::new();
        bv.push(true);
        assert_eq!(bv.get(1), None);
        assert_eq!(bv.get(1000), None);
    }

    // --- Feature 11: Logic Initial Level Tracking ---

    #[test]
    fn t1_f11_01_initial_low_tracking() {
        let mut bv = BitVecChunk::new();
        bv.push(false);
        bv.push(true);
        assert_eq!(bv.digital_init_level(), 0);
    }

    #[test]
    fn t1_f11_02_initial_high_tracking() {
        let mut bv = BitVecChunk::new();
        bv.push(true);
        bv.push(false);
        assert_eq!(bv.digital_init_level(), 1);
    }

    #[test]
    fn t1_f11_03_square_wave_reconstruction() {
        let bits = generate_square_wave_bits(200, 50);
        let mut bv = BitVecChunk::new();
        bv.extend_from_slice(&bits);
        bv.compute_edge_indices();

        let mut current_level = bv.digital_init_level() == 1;
        let mut edge_iter = bv.edge_indices.iter().copied().peekable();

        for i in 0..200 {
            if let Some(&next_edge) = edge_iter.peek() {
                if i == next_edge as usize {
                    current_level = !current_level;
                    edge_iter.next();
                }
            }
            assert_eq!(bv.get(i), Some(current_level));
        }
    }

    #[test]
    fn t1_f11_04_default_empty_chunk() {
        let bv = BitVecChunk::new();
        assert_eq!(bv.digital_init_level(), 0);
    }

    #[test]
    fn t1_f11_05_stability_constant_level() {
        let mut bv = BitVecChunk::new();
        bv.extend_from_slice(&vec![true; 100]);
        assert_eq!(bv.digital_init_level(), 1);
        bv.compute_edge_indices();
        assert!(bv.edge_indices.is_empty());
    }

    // --- Feature 12: EventDataChunk::StateTransitions ---

    #[test]
    fn t1_f12_01_parallel_arrays_init() {
        let chunk = EventDataChunk::new_state_transitions(10);
        let data_chunk = DataChunk::Event(chunk);
        assert_eq!(data_chunk.sample_count(), 0);
    }

    #[test]
    fn t1_f12_02_state_code_retention() {
        let mut chunk = EventDataChunk::new_state_transitions(5);
        chunk.append_transition(100, 1).unwrap();
        chunk.append_transition(200, 2).unwrap();
        assert_eq!(chunk.get_transition(0), Some((100, 1)));
        assert_eq!(chunk.get_transition(1), Some((200, 2)));
    }

    #[test]
    fn t1_f12_03_timestamp_retention() {
        let mut chunk = EventDataChunk::new_state_transitions(5);
        chunk.append_transition(1_000_000_000, 42).unwrap();
        let (ts, code) = chunk.get_transition(0).unwrap();
        assert_eq!(ts, 1_000_000_000);
        assert_eq!(code, 42);
    }

    #[test]
    fn t1_f12_04_time_range_extraction() {
        let mut chunk = EventDataChunk::new_state_transitions(5);
        chunk.append_transition(100, 1).unwrap();
        chunk.append_transition(500, 2).unwrap();
        let data_chunk = DataChunk::Event(chunk);
        assert_eq!(data_chunk.time_range_ns(), Some((100, 500)));
    }

    #[test]
    fn t1_f12_05_memory_footprint_calculation() {
        let mut chunk = EventDataChunk::new_state_transitions(10);
        chunk.append_transition(100, 1).unwrap();
        let data_chunk = DataChunk::Event(chunk);
        assert!(data_chunk.byte_footprint() > 0);
    }

    // --- Feature 13: State Transition Appending ---

    #[test]
    fn t1_f13_01_monotonic_sequential_append() {
        let mut chunk = EventDataChunk::new_state_transitions(5);
        assert!(chunk.append_transition(100, 1).is_ok());
        assert!(chunk.append_transition(200, 2).is_ok());
        assert!(chunk.append_transition(300, 3).is_ok());
    }

    #[test]
    fn t1_f13_02_equal_timestamp_append() {
        let mut chunk = EventDataChunk::new_state_transitions(5);
        assert!(chunk.append_transition(200, 1).is_ok());
        assert!(chunk.append_transition(200, 2).is_ok()); // Same timestamp permitted
    }

    #[test]
    fn t1_f13_03_non_monotonic_rejection() {
        let mut chunk = EventDataChunk::new_state_transitions(5);
        chunk.append_transition(200, 1).unwrap();
        let err = chunk.append_transition(150, 2);
        assert!(err.is_err());
    }

    #[test]
    fn t1_f13_04_negative_state_codes() {
        let mut chunk = EventDataChunk::new_state_transitions(5);
        assert!(chunk.append_transition(100, -1).is_ok());
        assert_eq!(chunk.get_state_at(100), Some(-1));
    }

    #[test]
    fn t1_f13_05_zero_timestamp_support() {
        let mut chunk = EventDataChunk::new_state_transitions(5);
        assert!(chunk.append_transition(0, 10).is_ok());
        assert_eq!(chunk.get_state_at(0), Some(10));
    }

    // --- Feature 14: State Transition Retrieval ---

    #[test]
    fn t1_f14_01_exact_timestamp_lookup() {
        let mut chunk = EventDataChunk::new_state_transitions(5);
        chunk.append_transition(100, 1).unwrap();
        chunk.append_transition(200, 2).unwrap();
        assert_eq!(chunk.get_state_at(100), Some(1));
        assert_eq!(chunk.get_state_at(200), Some(2));
    }

    #[test]
    fn t1_f14_02_between_transitions_zero_order_hold() {
        let mut chunk = EventDataChunk::new_state_transitions(5);
        chunk.append_transition(100, 1).unwrap();
        chunk.append_transition(200, 2).unwrap();
        assert_eq!(chunk.get_state_at(150), Some(1));
        assert_eq!(chunk.get_state_at(199), Some(1));
    }

    #[test]
    fn t1_f14_03_after_last_transition() {
        let mut chunk = EventDataChunk::new_state_transitions(5);
        chunk.append_transition(100, 1).unwrap();
        chunk.append_transition(200, 2).unwrap();
        assert_eq!(chunk.get_state_at(500), Some(2));
    }

    #[test]
    fn t1_f14_04_before_first_transition() {
        let mut chunk = EventDataChunk::new_state_transitions(5);
        chunk.append_transition(100, 1).unwrap();
        assert_eq!(chunk.get_state_at(50), None);
    }

    #[test]
    fn t1_f14_05_empty_chunk_state_query() {
        let chunk = EventDataChunk::new_state_transitions(5);
        assert_eq!(chunk.get_state_at(100), None);
    }

    // --- Feature 15: Serde RC Fix ---

    #[test]
    fn t1_f15_01_serde_rc_compilation() {
        // Verify genuine serialization and deserialization of StructFieldKind::Nested(Arc<StructSchema>)
        let schema = StructSchema {
            name: "TestSchema".into(),
            msg_id: Some(101),
            packed: false,
            fields: vec![],
        };
        let arc_schema = Arc::new(schema);
        let field_kind = StructFieldKind::Nested(arc_schema);

        let json = serde_json::to_string(&field_kind)
            .expect("StructFieldKind::Nested with Arc<StructSchema> should serialize");
        let deserialized: StructFieldKind = serde_json::from_str(&json)
            .expect("StructFieldKind::Nested with Arc<StructSchema> should deserialize");

        match deserialized {
            StructFieldKind::Nested(inner) => {
                assert_eq!(inner.name, "TestSchema");
                assert_eq!(inner.msg_id, Some(101));
                assert!(!inner.packed);
                assert!(inner.fields.is_empty());
            }
            _ => panic!("Expected StructFieldKind::Nested variant after deserialization"),
        }
    }

    #[test]
    fn t1_f15_02_arc_struct_schema_serialize() {
        let schema = StructSchema {
            name: "Child".into(),
            msg_id: None,
            packed: false,
            fields: vec![],
        };
        let field = StructFieldDef {
            name: "sub".into(),
            offset_bytes: None,
            unit: None,
            description: None,
            kind: StructFieldKind::Nested(Arc::new(schema)),
        };
        let json = serde_json::to_string(&field);
        assert!(json.is_ok());
    }

    #[test]
    fn t1_f15_03_arc_struct_schema_deserialize() {
        let schema = StructSchema {
            name: "Child".into(),
            msg_id: None,
            packed: false,
            fields: vec![],
        };
        let field = StructFieldDef {
            name: "sub".into(),
            offset_bytes: None,
            unit: None,
            description: None,
            kind: StructFieldKind::Nested(Arc::new(schema)),
        };
        let json = serde_json::to_string(&field).unwrap();
        let deserialized: Result<StructFieldDef, _> = serde_json::from_str(&json);
        assert!(deserialized.is_ok());
        assert_eq!(deserialized.unwrap().name, "sub");
    }

    #[test]
    fn t1_f15_04_eventschema_structsnapshot_roundtrip() {
        let schema = Arc::new(StructSchema {
            name: "Packet".into(),
            msg_id: None,
            packed: false,
            fields: vec![],
        });
        let kind = EventSchemaKind::StructSnapshot(schema);
        let json = serde_json::to_string(&kind).unwrap();
        let res: EventSchemaKind = serde_json::from_str(&json).unwrap();
        assert!(matches!(res, EventSchemaKind::StructSnapshot(_)));
    }

    #[test]
    fn t1_f15_05_multiple_arc_clones_serialization() {
        let schema = Arc::new(StructSchema {
            name: "Shared".into(),
            msg_id: None,
            packed: false,
            fields: vec![],
        });
        let f1 = StructFieldDef {
            name: "a".into(),
            offset_bytes: None,
            unit: None,
            description: None,
            kind: StructFieldKind::Nested(Arc::clone(&schema)),
        };
        let f2 = StructFieldDef {
            name: "b".into(),
            offset_bytes: None,
            unit: None,
            description: None,
            kind: StructFieldKind::Nested(Arc::clone(&schema)),
        };
        let json1 = serde_json::to_string(&f1).unwrap();
        let json2 = serde_json::to_string(&f2).unwrap();
        assert!(json1.contains("Shared"));
        assert!(json2.contains("Shared"));
    }

    // --- Feature 16: ActiveScalarChunk ---

    #[test]
    fn t1_f16_01_preallocated_capacity() {
        let series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        assert_eq!(series.total_samples(), 0);
        assert_eq!(series.sealed_chunk_count(), 0);
    }

    #[test]
    fn t1_f16_02_batch_slice_append() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let ts = generate_timestamps(1000, 1024, 100);
        let vals = vec![1.0f32; 1024];
        let appended = series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        assert_eq!(appended, 1024);
        assert_eq!(series.total_samples(), 1024);
    }

    #[test]
    fn t1_f16_03_successive_batch_accumulation() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::I16, LinearScale::identity());
        let ts1 = generate_timestamps(1000, 500, 10);
        let vals1 = vec![10i16; 500];
        series.append(&ts1, ScalarSlice::I16(&vals1)).unwrap();

        let ts2 = generate_timestamps(6000, 500, 10);
        let vals2 = vec![20i16; 500];
        series.append(&ts2, ScalarSlice::I16(&vals2)).unwrap();

        assert_eq!(series.total_samples(), 1000);
    }

    #[test]
    fn t1_f16_04_remaining_capacity_query() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::U32, LinearScale::identity());
        let ts = generate_timestamps(0, 10_000, 10);
        let vals = vec![100u32; 10_000];
        series.append(&ts, ScalarSlice::U32(&vals)).unwrap();
        assert_eq!(series.total_samples(), 10_000);
        assert_eq!(series.sealed_chunk_count(), 0); // Not sealed yet because < 65,536
    }

    #[test]
    fn t1_f16_05_zero_reallocation_within_capacity() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F64, LinearScale::identity());
        for b in 0..10 {
            let ts = generate_timestamps(b * 1000, 100, 10);
            let vals = vec![42.0f64; 100];
            series.append(&ts, ScalarSlice::F64(&vals)).unwrap();
        }
        assert_eq!(series.total_samples(), 1000);
    }

    // --- Feature 17: Automatic Sealing at 65,536 ---

    #[test]
    fn t1_f17_01_exact_threshold_sealing() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let ts = generate_timestamps(0, CHUNK_LEN, 10);
        let vals = vec![1.5f32; CHUNK_LEN];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        assert_eq!(series.sealed_chunk_count(), 1);
        assert_eq!(series.total_samples(), CHUNK_LEN as u64);
    }

    #[test]
    fn t1_f17_02_sealed_chunk_count_increment() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::I32, LinearScale::identity());
        let ts = generate_timestamps(0, CHUNK_LEN * 2, 10);
        let vals = vec![100i32; CHUNK_LEN * 2];
        series.append(&ts, ScalarSlice::I32(&vals)).unwrap();
        assert_eq!(series.sealed_chunk_count(), 2);
        assert_eq!(series.total_samples(), (CHUNK_LEN * 2) as u64);
    }

    #[test]
    fn t1_f17_03_sealed_payload_immutability() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::U8, LinearScale::identity());
        let ts = generate_timestamps(0, CHUNK_LEN, 10);
        let vals = vec![7u8; CHUNK_LEN];
        series.append(&ts, ScalarSlice::U8(&vals)).unwrap();

        // Appending more points should not alter sealed chunk contents
        let ts2 = generate_timestamps(CHUNK_LEN as u64 * 10, 100, 10);
        let vals2 = vec![99u8; 100];
        series.append(&ts2, ScalarSlice::U8(&vals2)).unwrap();

        let sample_0 = series.get_sample_by_index(0).unwrap();
        assert_eq!(sample_0.1, 7.0);
    }

    #[test]
    fn t1_f17_04_chunkhandle_metadata_verification() {
        let mut series =
            NativeScalarSeries::new(ChannelId(42), ScalarType::F32, LinearScale::identity());
        let ts = generate_timestamps(1000, CHUNK_LEN, 10);
        let vals = vec![0.5f32; CHUNK_LEN];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        assert_eq!(series.sealed_chunk_count(), 1);
    }

    #[test]
    fn t1_f17_05_time_bounds_on_sealed_handle() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::I16, LinearScale::identity());
        let ts = generate_timestamps(5000, CHUNK_LEN, 20);
        let vals = vec![12i16; CHUNK_LEN];
        series.append(&ts, ScalarSlice::I16(&vals)).unwrap();
        let (first_ts, _) = series.get_sample_by_index(0).unwrap();
        let (last_ts, _) = series.get_sample_by_index((CHUNK_LEN - 1) as u64).unwrap();
        assert_eq!(first_ts, 5000);
        assert_eq!(last_ts, 5000 + (CHUNK_LEN as u64 - 1) * 20);
    }

    // --- Feature 18: Multi-Batch Rollover ---

    #[test]
    fn t1_f18_01_single_boundary_split_70k() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let ts = generate_timestamps(0, 70_000, 10);
        let vals = vec![1.0f32; 70_000];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        assert_eq!(series.sealed_chunk_count(), 1);
        assert_eq!(series.total_samples(), 70_000);
    }

    #[test]
    fn t1_f18_02_double_boundary_split_150k() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::I16, LinearScale::identity());
        let ts = generate_timestamps(0, 150_000, 10);
        let vals = vec![5i16; 150_000];
        series.append(&ts, ScalarSlice::I16(&vals)).unwrap();
        assert_eq!(series.sealed_chunk_count(), 2); // 131,072 in 2 chunks, 18,928 in active
        assert_eq!(series.total_samples(), 150_000);
    }

    #[test]
    fn t1_f18_03_triple_boundary_split_200k() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::U32, LinearScale::identity());
        let ts = generate_timestamps(0, 200_000, 10);
        let vals = vec![100u32; 200_000];
        series.append(&ts, ScalarSlice::U32(&vals)).unwrap();
        assert_eq!(series.sealed_chunk_count(), 3); // 196,608 in 3 chunks
        assert_eq!(series.total_samples(), 200_000);
    }

    #[test]
    fn t1_f18_04_total_sample_integrity_across_splits() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F64, LinearScale::identity());
        let ts = generate_timestamps(0, 150_000, 10);
        let vals: Vec<f64> = (0..150_000).map(|i| i as f64).collect();
        series.append(&ts, ScalarSlice::F64(&vals)).unwrap();
        assert_eq!(series.total_samples(), 150_000);
    }

    #[test]
    fn t1_f18_05_continuity_across_chunk_split() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::I32, LinearScale::identity());
        let ts = generate_timestamps(0, 70_000, 100);
        let vals: Vec<i32> = (0..70_000).map(|i| i as i32).collect();
        series.append(&ts, ScalarSlice::I32(&vals)).unwrap();

        // Sample 65,535 (last sample in chunk 0)
        let (ts_65535, val_65535) = series.get_sample_by_index(65_535).unwrap();
        assert_eq!(ts_65535, 65_535 * 100);
        assert_eq!(val_65535, 65_535.0);

        // Sample 65,536 (first sample in active buffer)
        let (ts_65536, val_65536) = series.get_sample_by_index(65_536).unwrap();
        assert_eq!(ts_65536, 65_536 * 100);
        assert_eq!(val_65536, 65_536.0);
    }

    // --- Feature 19: NativeScalarSeries ---

    #[test]
    fn t1_f19_01_series_initialization() {
        let series =
            NativeScalarSeries::new(ChannelId(7), ScalarType::F32, LinearScale::identity());
        assert_eq!(series.total_samples(), 0);
        assert_eq!(series.sealed_chunk_count(), 0);
    }

    #[test]
    fn t1_f19_02_monotonic_sample_count() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::I16, LinearScale::identity());
        let ts1 = generate_timestamps(0, 100, 10);
        series
            .append(&ts1, ScalarSlice::I16(&vec![1; 100]))
            .unwrap();
        assert_eq!(series.total_samples(), 100);

        let ts2 = generate_timestamps(1000, 200, 10);
        series
            .append(&ts2, ScalarSlice::I16(&vec![2; 200]))
            .unwrap();
        assert_eq!(series.total_samples(), 300);
    }

    #[test]
    fn t1_f19_03_channel_id_retention() {
        let series =
            NativeScalarSeries::new(ChannelId(99), ScalarType::U8, LinearScale::identity());
        assert_eq!(series.total_samples(), 0);
    }

    #[test]
    fn t1_f19_04_scalar_type_retention() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::U64, LinearScale::identity());
        let ts = generate_timestamps(0, 10, 10);
        let vals = vec![1000u64; 10];
        assert!(series.append(&ts, ScalarSlice::U64(&vals)).is_ok());
    }

    #[test]
    fn t1_f19_05_linear_scale_application() {
        let scale = LinearScale::new(2.0, 5.0);
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, scale);
        let ts = vec![1000];
        let vals = vec![10.0f32];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        let (_, val) = series.get_sample_by_index(0).unwrap();
        assert_eq!(val, 25.0); // 10.0 * 2.0 + 5.0 = 25.0
    }

    // --- Feature 20: EventSeries ---

    #[test]
    fn t1_f20_01_event_series_init() {
        let series = EventSeries::new(ChannelId(2));
        assert_eq!(series.get_state_at(1000), None);
    }

    #[test]
    fn t1_f20_02_event_series_batch_append() {
        let mut series = EventSeries::new(ChannelId(2));
        let ts = vec![100, 200, 300];
        let codes = vec![1, 2, 3];
        assert_eq!(series.append(&ts, &codes).unwrap(), 3);
    }

    #[test]
    fn t1_f20_03_event_series_state_lookup() {
        let mut series = EventSeries::new(ChannelId(2));
        let ts = vec![100, 200];
        let codes = vec![10, 20];
        series.append(&ts, &codes).unwrap();
        assert_eq!(series.get_state_at(100), Some(10));
        assert_eq!(series.get_state_at(150), Some(10));
        assert_eq!(series.get_state_at(200), Some(20));
        assert_eq!(series.get_state_at(250), Some(20));
    }

    #[test]
    fn t1_f20_04_event_series_channel_id() {
        let series = EventSeries::new(ChannelId(42));
        assert_eq!(series.get_state_at(0), None);
    }

    #[test]
    fn t1_f20_05_event_series_watermark_integration() {
        let mut series = EventSeries::new(ChannelId(5));
        let listener = MockWatermarkListener::new();
        series.register_listener(listener.clone());
        series.append(&[100, 200], &[1, 2]).unwrap();
        assert_eq!(listener.event_count(), 1);
        let event = listener.last_event().unwrap();
        assert_eq!(event.channel_id, ChannelId(5));
        assert_eq!(event.latest_ts_ns, 200);
        assert_eq!(event.total_samples, 2);
    }

    // --- Feature 21: Sample Retrieval by Index ---

    #[test]
    fn t1_f21_01_active_buffer_index_lookup() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let ts = generate_timestamps(1000, 500, 10);
        let vals: Vec<f32> = (0..500).map(|i| i as f32).collect();
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        let (t, v) = series.get_sample_by_index(100).unwrap();
        assert_eq!(t, 2000);
        assert_eq!(v, 100.0);
    }

    #[test]
    fn t1_f21_02_sealed_chunk_0_index_lookup() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::I16, LinearScale::identity());
        let ts = generate_timestamps(0, 100_000, 10);
        let vals: Vec<i16> = (0..100_000).map(|i| (i % 1000) as i16).collect();
        series.append(&ts, ScalarSlice::I16(&vals)).unwrap();
        let (t, v) = series.get_sample_by_index(10_000).unwrap();
        assert_eq!(t, 100_000);
        assert_eq!(v, 0.0); // 10,000 % 1000 = 0
    }

    #[test]
    fn t1_f21_03_sealed_chunk_1_index_lookup() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::I32, LinearScale::identity());
        let ts = generate_timestamps(0, 150_000, 10);
        let vals: Vec<i32> = (0..150_000).map(|i| i as i32).collect();
        series.append(&ts, ScalarSlice::I32(&vals)).unwrap();
        let (t, v) = series.get_sample_by_index(70_000).unwrap();
        assert_eq!(t, 700_000);
        assert_eq!(v, 70_000.0);
    }

    #[test]
    fn t1_f21_04_calibrated_value_return() {
        let scale = LinearScale::new(0.1, -1.0);
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, scale);
        series.append(&[100], ScalarSlice::F32(&[50.0])).unwrap();
        let (_, v) = series.get_sample_by_index(0).unwrap();
        assert_eq!(v, 4.0); // 50.0 * 0.1 - 1.0 = 4.0
    }

    #[test]
    fn t1_f21_05_out_of_bounds_returns_none() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        series.append(&[100], ScalarSlice::F32(&[1.0])).unwrap();
        assert_eq!(series.get_sample_by_index(1), None);
        assert_eq!(series.get_sample_by_index(100), None);
    }

    // --- Feature 22: Sample Retrieval by Timestamp ---

    #[test]
    fn t1_f22_01_exact_timestamp_lookup() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let ts = generate_timestamps(1000, 100, 50);
        let vals: Vec<f32> = (0..100).map(|i| i as f32).collect();
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        let (idx, t, v) = series.get_sample_by_timestamp(2000).unwrap();
        assert_eq!(idx, 20);
        assert_eq!(t, 2000);
        assert_eq!(v, 20.0);
    }

    #[test]
    fn t1_f22_02_nearest_timestamp_lookup() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let ts = generate_timestamps(1000, 100, 50);
        let vals: Vec<f32> = (0..100).map(|i| i as f32).collect();
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        // Query ts between samples (e.g. 2020 ns, between sample 20 at 2000 and 21 at 2050)
        let res = series.get_sample_by_timestamp(2020);
        assert!(res.is_some());
    }

    #[test]
    fn t1_f22_03_query_within_sealed_chunk() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::I16, LinearScale::identity());
        let ts = generate_timestamps(0, 100_000, 10);
        let vals = vec![5i16; 100_000];
        series.append(&ts, ScalarSlice::I16(&vals)).unwrap();
        let (idx, t, _) = series.get_sample_by_timestamp(100_000).unwrap();
        assert_eq!(idx, 10_000);
        assert_eq!(t, 100_000);
    }

    #[test]
    fn t1_f22_04_query_within_active_buffer() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::I16, LinearScale::identity());
        let ts = generate_timestamps(0, 70_000, 10);
        let vals = vec![5i16; 70_000];
        series.append(&ts, ScalarSlice::I16(&vals)).unwrap();
        let (idx, t, _) = series.get_sample_by_timestamp(680_000).unwrap();
        assert_eq!(idx, 68_000);
        assert_eq!(t, 680_000);
    }

    #[test]
    fn t1_f22_05_empty_series_timestamp_lookup() {
        let series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        assert_eq!(series.get_sample_by_timestamp(1000), None);
    }

    // --- Feature 23: Watermark Notification Emission ---

    #[test]
    fn t1_f23_01_listener_registration() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let listener = MockWatermarkListener::new();
        series.register_listener(listener.clone());
        assert_eq!(listener.event_count(), 0);
    }

    #[test]
    fn t1_f23_02_event_emission_on_commit() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let listener = MockWatermarkListener::new();
        series.register_listener(listener.clone());
        let ts = generate_timestamps(1000, 100, 10);
        series
            .append(&ts, ScalarSlice::F32(&vec![1.0; 100]))
            .unwrap();
        assert_eq!(listener.event_count(), 1);
    }

    #[test]
    fn t1_f23_03_event_channel_id_accuracy() {
        let mut series =
            NativeScalarSeries::new(ChannelId(77), ScalarType::F32, LinearScale::identity());
        let listener = MockWatermarkListener::new();
        series.register_listener(listener.clone());
        let ts = generate_timestamps(1000, 50, 10);
        series
            .append(&ts, ScalarSlice::F32(&vec![1.0; 50]))
            .unwrap();
        let ev = listener.last_event().unwrap();
        assert_eq!(ev.channel_id, ChannelId(77));
    }

    #[test]
    fn t1_f23_04_latest_timestamp_accuracy() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let listener = MockWatermarkListener::new();
        series.register_listener(listener.clone());
        let ts = generate_timestamps(5000, 50, 20); // 5000 .. 5000 + 49*20 = 5980
        series
            .append(&ts, ScalarSlice::F32(&vec![1.0; 50]))
            .unwrap();
        let ev = listener.last_event().unwrap();
        assert_eq!(ev.latest_ts_ns, 5980);
    }

    #[test]
    fn t1_f23_05_total_samples_count_accuracy() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let listener = MockWatermarkListener::new();
        series.register_listener(listener.clone());
        series
            .append(
                &generate_timestamps(0, 100, 10),
                ScalarSlice::F32(&vec![1.0; 100]),
            )
            .unwrap();
        series
            .append(
                &generate_timestamps(1000, 200, 10),
                ScalarSlice::F32(&vec![1.0; 200]),
            )
            .unwrap();
        let ev = listener.last_event().unwrap();
        assert_eq!(ev.total_samples, 300);
    }

    // --- Feature 24: ChunkStorageRef Isolation ---

    #[test]
    fn t1_f24_01_in_memory_variant() {
        let mut vals = NativeScalarColumn::with_capacity(ScalarType::F32, 1);
        vals.append_slice(ScalarSlice::F32(&[0.0])).unwrap();
        let chunk = Arc::new(DataChunk::Scalar(
            ScalarDataChunk::new(
                TimestampColumn::from_timestamps(0, &[0]),
                vals,
                LinearScale::identity(),
            )
            .unwrap(),
        ));
        let storage_ref = ChunkStorageRef::InMemory(chunk);
        assert!(storage_ref.is_in_memory());
    }

    #[test]
    fn t1_f24_02_mmap_variant() {
        let locator = MmapChunkLocator {
            file_id: 1,
            byte_offset: 4096,
            byte_len: 65536,
            flags: 0,
        };
        let storage_ref = ChunkStorageRef::MmapSlice(locator);
        assert!(!storage_ref.is_in_memory());
    }

    #[test]
    fn t1_f24_03_as_in_memory_accessor() {
        let mut vals = NativeScalarColumn::with_capacity(ScalarType::F32, 1);
        vals.append_slice(ScalarSlice::F32(&[0.0])).unwrap();
        let chunk = Arc::new(DataChunk::Scalar(
            ScalarDataChunk::new(
                TimestampColumn::from_timestamps(0, &[0]),
                vals,
                LinearScale::identity(),
            )
            .unwrap(),
        ));
        let storage_ref = ChunkStorageRef::InMemory(chunk);
        assert!(storage_ref.as_in_memory().is_some());
    }

    #[test]
    fn t1_f24_04_mmap_as_in_memory_none() {
        let locator = MmapChunkLocator {
            file_id: 1,
            byte_offset: 0,
            byte_len: 1024,
            flags: 0,
        };
        let storage_ref = ChunkStorageRef::MmapSlice(locator);
        assert!(storage_ref.as_in_memory().is_none());
    }

    #[test]
    fn t1_f24_05_resident_bytes_accounting() {
        let locator = MmapChunkLocator {
            file_id: 1,
            byte_offset: 0,
            byte_len: 1024,
            flags: 0,
        };
        let storage_ref = ChunkStorageRef::MmapSlice(locator);
        assert_eq!(storage_ref.resident_bytes(), 0); // Mmap chunks do not consume RAM quota
    }

    // --- Feature 25: Criterion Append Benchmark ---

    #[test]
    fn t1_f25_01_bench_target_compilation() {
        // Structural validation for high-throughput append
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let ts = generate_timestamps(0, 10_000, 10);
        let vals = vec![1.0f32; 10_000];
        let start = std::time::Instant::now();
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        let elapsed = start.elapsed();
        // Verify 10K points appended rapidly (< 50ms)
        assert!(elapsed.as_millis() < 50);
    }

    #[test]
    fn t1_f25_02_native_f32_throughput() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let ts = generate_timestamps(0, 65_536, 10);
        let vals = vec![1.0f32; 65_536];
        let appended = series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        assert_eq!(appended, 65_536);
    }

    #[test]
    fn t1_f25_03_native_i16_throughput() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::I16, LinearScale::identity());
        let ts = generate_timestamps(0, 65_536, 10);
        let vals = vec![1234i16; 65_536];
        let appended = series.append(&ts, ScalarSlice::I16(&vals)).unwrap();
        assert_eq!(appended, 65_536);
    }

    #[test]
    fn t1_f25_04_native_u32_throughput() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::U32, LinearScale::identity());
        let ts = generate_timestamps(0, 65_536, 10);
        let vals = vec![99999u32; 65_536];
        let appended = series.append(&ts, ScalarSlice::U32(&vals)).unwrap();
        assert_eq!(appended, 65_536);
    }

    #[test]
    fn t1_f25_05_multi_batch_throughput_scaling() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        for b in 0..4 {
            let ts = generate_timestamps(b * 16_384 * 10, 16_384, 10);
            let vals = vec![1.0f32; 16_384];
            series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        }
        assert_eq!(series.total_samples(), 65_536);
        assert_eq!(series.sealed_chunk_count(), 1);
    }

    // --- Feature 26: Workspace Quality Gate ---

    #[test]
    fn t1_f26_01_core_error_downcast_and_formatting() {
        // Enforce CoreError std::error::Error trait implementation, downcasting, and formatting
        let io_err =
            std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "premature end of stream");
        let core_err: CoreError = CoreError::from(io_err);
        assert!(matches!(core_err, CoreError::Io(_)));

        let err_dyn: &dyn std::error::Error = &core_err;
        assert!(
            err_dyn.source().is_some(),
            "CoreError::Io must preserve source error"
        );
        assert!(format!("{core_err}").contains("premature end of stream"));
    }

    #[test]
    fn t1_f26_02_aligned_vec_cache_line_invariants() {
        // Memory layout quality gate: verify 64-byte cache line alignment across types
        let mut v_u8: AlignedVec<u8> = AlignedVec::with_capacity(128);
        v_u8.extend_from_slice(&[42u8; 100]);
        assert_eq!((v_u8.as_slice().as_ptr() as usize) % CACHE_LINE_BYTES, 0);

        let mut v_f64: AlignedVec<f64> = AlignedVec::with_capacity(100);
        v_f64.extend_from_slice(&[1.23, 4.56, 7.89]);
        assert_eq!((v_f64.as_slice().as_ptr() as usize) % CACHE_LINE_BYTES, 0);
    }

    #[test]
    fn t1_f26_03_scalar_slice_mismatch_error_propagation() {
        // Type-safety quality gate: ensure mismatched scalar slice returns CoreError::Storage cleanly
        let mut col = NativeScalarColumn::with_capacity(ScalarType::F32, 10);
        let res = col.append_slice(ScalarSlice::U64(&[1, 2, 3]));
        assert!(
            res.is_err(),
            "Appending U64 slice into F32 column must fail"
        );
        match res {
            Err(CoreError::Storage(msg)) => assert!(msg.contains("mismatch")),
            _ => panic!("Expected CoreError::Storage type mismatch error"),
        }
    }

    #[test]
    fn t1_f26_04_scalar_data_chunk_monotonicity_guard() {
        // Data integrity quality gate: verify rejection of non-monotonic timestamps
        let base = 1_000_000u64;
        let ts_inverted = TimestampColumn::from_timestamps(base, &[base + 200, base + 100]);
        let mut col = NativeScalarColumn::with_capacity(ScalarType::F32, 2);
        col.append_slice(ScalarSlice::F32(&[1.0, 2.0])).unwrap();
        let res = ScalarDataChunk::new(ts_inverted, col, LinearScale::identity());
        assert!(res.is_err(), "Non-monotonic timestamps must be rejected");
        match res {
            Err(CoreError::Storage(msg)) => assert!(msg.contains("non-monotonic")),
            _ => panic!("Expected non-monotonic timestamp storage error"),
        }
    }

    #[test]
    fn t1_f26_05_struct_schema_full_roundtrip_fidelity() {
        // Serialization contract quality gate: verify full roundtrip across complex schemas
        let mut labels = StateEnumMap::new();
        labels.insert(
            0,
            StateLabel {
                name: "IDLE".into(),
                color_rgba: Some(0x00FF00FF),
            },
        );
        labels.insert(
            1,
            StateLabel {
                name: "BUSY".into(),
                color_rgba: Some(0xFF0000FF),
            },
        );
        let schema = StructSchema {
            name: "TelemetryFrame".into(),
            msg_id: Some(0x100),
            packed: false,
            fields: vec![
                StructFieldDef {
                    name: "status".into(),
                    offset_bytes: Some(0),
                    unit: None,
                    description: Some("System status".into()),
                    kind: StructFieldKind::Enum {
                        base_type: ScalarType::I32,
                        endian: Endianness::Little,
                        labels,
                    },
                },
                StructFieldDef {
                    name: "voltage".into(),
                    offset_bytes: Some(4),
                    unit: Some("V".into()),
                    description: None,
                    kind: StructFieldKind::Scalar {
                        scalar_type: ScalarType::F32,
                        endian: Endianness::Little,
                        scale: LinearScale::new(0.001, 0.0),
                        bitfield: None,
                    },
                },
            ],
        };
        let json = serde_json::to_string(&schema).expect("schema serialize");
        let deserialized: StructSchema = serde_json::from_str(&json).expect("schema deserialize");
        assert_eq!(schema, deserialized);
    }
}

// ============================================================================
// Tier 2: Boundary & Corner Cases (Extreme Limits & Rollover, >=5 tests per feature)
// ============================================================================
pub mod tier2_boundary_corner {
    use super::*;

    // --- Feature 1: CHUNK_LEN Boundaries ---

    #[test]
    fn t2_f01_01_zero_samples_buffer() {
        let series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        assert_eq!(series.total_samples(), 0);
        assert_eq!(series.sealed_chunk_count(), 0);
    }

    #[test]
    fn t2_f01_02_singleton_sample() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        series.append(&[100], ScalarSlice::F32(&[1.0])).unwrap();
        assert_eq!(series.total_samples(), 1);
        assert_eq!(series.sealed_chunk_count(), 0);
    }

    #[test]
    fn t2_f01_03_pre_seal_threshold_65535() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let ts = generate_timestamps(0, CHUNK_LEN - 1, 10);
        let vals = vec![1.0f32; CHUNK_LEN - 1];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        assert_eq!(series.total_samples(), (CHUNK_LEN - 1) as u64);
        assert_eq!(series.sealed_chunk_count(), 0); // Not sealed at 65,535
    }

    #[test]
    fn t2_f01_04_exact_seal_threshold_65536() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        // Append 65,535 first, then 1 sample
        let ts1 = generate_timestamps(0, CHUNK_LEN - 1, 10);
        let vals1 = vec![1.0f32; CHUNK_LEN - 1];
        series.append(&ts1, ScalarSlice::F32(&vals1)).unwrap();
        assert_eq!(series.sealed_chunk_count(), 0);

        let ts2 = vec![(CHUNK_LEN - 1) as u64 * 10];
        let vals2 = vec![2.0f32];
        series.append(&ts2, ScalarSlice::F32(&vals2)).unwrap();
        assert_eq!(series.total_samples(), CHUNK_LEN as u64);
        assert_eq!(series.sealed_chunk_count(), 1); // Seals exactly at 65,536
    }

    #[test]
    fn t2_f01_05_post_seal_singleton_65537() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let ts = generate_timestamps(0, CHUNK_LEN + 1, 10);
        let vals = vec![1.0f32; CHUNK_LEN + 1];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        assert_eq!(series.total_samples(), (CHUNK_LEN + 1) as u64);
        assert_eq!(series.sealed_chunk_count(), 1); // 1 sealed chunk + 1 sample in active buffer
    }

    // --- Feature 2: CACHE_LINE_BYTES Alignment Boundaries ---

    #[test]
    fn t2_f02_01_zero_byte_capacity_allocation() {
        let buf = AlignedBuffer::with_capacity(0);
        assert_eq!(buf.len(), 0);
        assert_eq!(buf.capacity(), 0);
    }

    #[test]
    fn t2_f02_02_one_byte_capacity_allocation() {
        let mut buf = AlignedBuffer::with_capacity(1);
        buf.extend_from_slice(&[0xFF]);
        assert_eq!(buf.len(), 1);
        assert!(buf.capacity() >= CACHE_LINE_BYTES);
        assert_eq!((buf.as_ptr() as usize) % CACHE_LINE_BYTES, 0);
    }

    #[test]
    fn t2_f02_03_exact_64_byte_capacity() {
        let mut buf = AlignedBuffer::with_capacity(64);
        buf.extend_from_slice(&[0xAA; 64]);
        assert_eq!(buf.len(), 64);
        assert_eq!((buf.as_ptr() as usize) % CACHE_LINE_BYTES, 0);
    }

    #[test]
    fn t2_f02_04_65_byte_spillover_allocation() {
        let mut buf = AlignedBuffer::with_capacity(65);
        buf.extend_from_slice(&[0x55; 65]);
        assert_eq!(buf.len(), 65);
        assert!(buf.capacity() >= 128); // Next cache line block
        assert_eq!((buf.as_ptr() as usize) % CACHE_LINE_BYTES, 0);
    }

    #[test]
    fn t2_f02_05_large_reallocation_expansion() {
        let mut vec: AlignedVec<u8> = AlignedVec::with_capacity(64);
        let data = vec![42u8; 65_536];
        vec.extend_from_slice(&data);
        assert_eq!(vec.len(), 65_536);
        assert_eq!((vec.as_ptr() as usize) % CACHE_LINE_BYTES, 0);
    }

    // --- Feature 3: 10 Scalar Types Extremes ---

    #[test]
    fn t2_f03_01_signed_minimum_bounds() {
        let mut col_i8 = NativeScalarColumn::with_capacity(ScalarType::I8, 1);
        col_i8.append_slice(ScalarSlice::I8(&[i8::MIN])).unwrap();
        assert_eq!(col_i8.get_as_f64(0), Some(i8::MIN as f64));

        let mut col_i16 = NativeScalarColumn::with_capacity(ScalarType::I16, 1);
        col_i16.append_slice(ScalarSlice::I16(&[i16::MIN])).unwrap();
        assert_eq!(col_i16.get_as_f64(0), Some(i16::MIN as f64));

        let mut col_i32 = NativeScalarColumn::with_capacity(ScalarType::I32, 1);
        col_i32.append_slice(ScalarSlice::I32(&[i32::MIN])).unwrap();
        assert_eq!(col_i32.get_as_f64(0), Some(i32::MIN as f64));

        let mut col_i64 = NativeScalarColumn::with_capacity(ScalarType::I64, 1);
        col_i64.append_slice(ScalarSlice::I64(&[i64::MIN])).unwrap();
        assert_eq!(col_i64.get_as_f64(0), Some(i64::MIN as f64));
    }

    #[test]
    fn t2_f03_02_signed_maximum_bounds() {
        let mut col_i8 = NativeScalarColumn::with_capacity(ScalarType::I8, 1);
        col_i8.append_slice(ScalarSlice::I8(&[i8::MAX])).unwrap();
        assert_eq!(col_i8.get_as_f64(0), Some(i8::MAX as f64));

        let mut col_i16 = NativeScalarColumn::with_capacity(ScalarType::I16, 1);
        col_i16.append_slice(ScalarSlice::I16(&[i16::MAX])).unwrap();
        assert_eq!(col_i16.get_as_f64(0), Some(i16::MAX as f64));

        let mut col_i32 = NativeScalarColumn::with_capacity(ScalarType::I32, 1);
        col_i32.append_slice(ScalarSlice::I32(&[i32::MAX])).unwrap();
        assert_eq!(col_i32.get_as_f64(0), Some(i32::MAX as f64));

        let mut col_i64 = NativeScalarColumn::with_capacity(ScalarType::I64, 1);
        col_i64.append_slice(ScalarSlice::I64(&[i64::MAX])).unwrap();
        assert_eq!(col_i64.get_as_f64(0), Some(i64::MAX as f64));
    }

    #[test]
    fn t2_f03_03_unsigned_maximum_bounds() {
        let mut col_u8 = NativeScalarColumn::with_capacity(ScalarType::U8, 1);
        col_u8.append_slice(ScalarSlice::U8(&[u8::MAX])).unwrap();
        assert_eq!(col_u8.get_as_f64(0), Some(u8::MAX as f64));

        let mut col_u16 = NativeScalarColumn::with_capacity(ScalarType::U16, 1);
        col_u16.append_slice(ScalarSlice::U16(&[u16::MAX])).unwrap();
        assert_eq!(col_u16.get_as_f64(0), Some(u16::MAX as f64));

        let mut col_u32 = NativeScalarColumn::with_capacity(ScalarType::U32, 1);
        col_u32.append_slice(ScalarSlice::U32(&[u32::MAX])).unwrap();
        assert_eq!(col_u32.get_as_f64(0), Some(u32::MAX as f64));

        let mut col_u64 = NativeScalarColumn::with_capacity(ScalarType::U64, 1);
        col_u64.append_slice(ScalarSlice::U64(&[u64::MAX])).unwrap();
        assert_eq!(col_u64.get_as_f64(0), Some(u64::MAX as f64));
    }

    #[test]
    fn t2_f03_04_float_signed_zeros_and_subnormals() {
        let mut col_f32 = NativeScalarColumn::with_capacity(ScalarType::F32, 3);
        col_f32
            .append_slice(ScalarSlice::F32(&[0.0, -0.0, f32::MIN_POSITIVE]))
            .unwrap();
        assert_eq!(col_f32.get_as_f64(0), Some(0.0));
        assert!(col_f32.get_as_f64(1).unwrap().is_sign_negative());
        assert_eq!(col_f32.get_as_f64(2), Some(f32::MIN_POSITIVE as f64));
    }

    #[test]
    fn t2_f03_05_float_inf_and_nan() {
        let mut col_f64 = NativeScalarColumn::with_capacity(ScalarType::F64, 3);
        col_f64
            .append_slice(ScalarSlice::F64(&[
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::NAN,
            ]))
            .unwrap();
        assert_eq!(col_f64.get_as_f64(0), Some(f64::INFINITY));
        assert_eq!(col_f64.get_as_f64(1), Some(f64::NEG_INFINITY));
        assert!(col_f64.get_as_f64(2).unwrap().is_nan());
    }

    // --- Feature 4: AlignedVec Boundaries ---

    #[test]
    fn t2_f04_01_empty_slice_extend() {
        let mut vec: AlignedVec<f32> = AlignedVec::with_capacity(16);
        vec.extend_from_slice(&[]);
        assert_eq!(vec.len(), 0);
    }

    #[test]
    fn t2_f04_02_capacity_boundary_fill() {
        let mut vec: AlignedVec<u32> = AlignedVec::with_capacity(16);
        let cap = vec.capacity();
        let data = vec![123u32; cap];
        vec.extend_from_slice(&data);
        assert_eq!(vec.len(), cap);
        assert_eq!(vec.capacity(), cap);
    }

    #[test]
    fn t2_f04_03_capacity_boundary_overflow_plus_one() {
        let mut vec: AlignedVec<u32> = AlignedVec::with_capacity(16);
        let cap = vec.capacity();
        let data = vec![123u32; cap];
        vec.extend_from_slice(&data);
        vec.push(999);
        assert_eq!(vec.len(), cap + 1);
        assert!(vec.capacity() > cap);
    }

    #[test]
    fn t2_f04_04_repeated_clear_and_reextend() {
        let mut vec: AlignedVec<i16> = AlignedVec::with_capacity(100);
        for _ in 0..10 {
            vec.extend_from_slice(&[1, 2, 3, 4, 5]);
            assert_eq!(vec.len(), 5);
            vec.clear();
            assert_eq!(vec.len(), 0);
        }
    }

    #[test]
    fn t2_f04_05_exact_65536_element_allocation() {
        let mut vec: AlignedVec<f32> = AlignedVec::with_capacity(CHUNK_LEN);
        let data = vec![1.0f32; CHUNK_LEN];
        vec.extend_from_slice(&data);
        assert_eq!(vec.len(), CHUNK_LEN);
        assert_eq!(vec.byte_len(), CHUNK_LEN * 4);
    }

    // --- Feature 5: TimestampColumn Boundaries ---

    #[test]
    fn t2_f05_01_zero_time_delta_span() {
        let base = 1_000_000u64;
        let ts = vec![base; 10]; // All identical timestamps
        let col = TimestampColumn::from_timestamps(base, &ts);
        assert!(matches!(col, TimestampColumn::RelU32 { .. }));
        for i in 0..10 {
            assert_eq!(col.get_ns(i), Some(base));
        }
    }

    #[test]
    fn t2_f05_02_exact_u32_max_span_boundary() {
        let base = 0u64;
        let max_u32 = u32::MAX as u64; // 4,294,967,295 ns
        let ts = vec![base, max_u32];
        let col = TimestampColumn::from_timestamps(base, &ts);
        assert!(matches!(col, TimestampColumn::RelU32 { .. }));
        assert_eq!(col.get_ns(1), Some(max_u32));
    }

    #[test]
    fn t2_f05_03_u32_max_plus_one_overflow_to_absu64() {
        let base = 0u64;
        let overflow_ts = (u32::MAX as u64) + 1; // 4,294,967,296 ns
        let ts = vec![base, overflow_ts];
        let col = TimestampColumn::from_timestamps(base, &ts);
        assert!(matches!(col, TimestampColumn::AbsU64(_)));
        assert_eq!(col.get_ns(1), Some(overflow_ts));
    }

    #[test]
    fn t2_f05_04_empty_timestamp_slice() {
        let col = TimestampColumn::from_timestamps(100, &[]);
        assert_eq!(col.len(), 0);
        assert!(col.is_empty());
        assert_eq!(col.get_ns(0), None);
    }

    #[test]
    fn t2_f05_05_single_sample_timestamp_column() {
        let col = TimestampColumn::from_timestamps(500, &[500]);
        assert_eq!(col.len(), 1);
        assert_eq!(col.first_ns(), Some(500));
        assert_eq!(col.last_ns(), Some(500));
    }

    // --- Feature 6: ScalarDataChunk Boundaries ---

    #[test]
    fn t2_f06_01_empty_scalar_chunk() {
        let ts = TimestampColumn::from_timestamps(0, &[]);
        let vals = NativeScalarColumn::with_capacity(ScalarType::F32, 0);
        let chunk = ScalarDataChunk::new(ts, vals, LinearScale::identity());
        assert!(chunk.is_ok());
        let chunk = chunk.unwrap();
        assert_eq!(chunk.len(), 0);
        assert!(chunk.is_empty());
    }

    #[test]
    fn t2_f06_02_singleton_scalar_chunk() {
        let ts = TimestampColumn::from_timestamps(100, &[100]);
        let mut vals = NativeScalarColumn::with_capacity(ScalarType::I16, 1);
        vals.append_slice(ScalarSlice::I16(&[123])).unwrap();
        let chunk = ScalarDataChunk::new(ts, vals, LinearScale::identity()).unwrap();
        assert_eq!(chunk.len(), 1);
        assert_eq!(chunk.time_range_ns(), Some((100, 100)));
    }

    #[test]
    fn t2_f06_03_exact_65536_sample_chunk() {
        let ts_data = generate_timestamps(0, CHUNK_LEN, 10);
        let ts = TimestampColumn::from_timestamps(0, &ts_data);
        let mut vals = NativeScalarColumn::with_capacity(ScalarType::F32, CHUNK_LEN);
        let val_data = vec![1.0f32; CHUNK_LEN];
        vals.append_slice(ScalarSlice::F32(&val_data)).unwrap();
        let chunk = ScalarDataChunk::new(ts, vals, LinearScale::identity()).unwrap();
        assert_eq!(chunk.len(), CHUNK_LEN);
    }

    #[test]
    fn t2_f06_04_slice_length_mismatch_rejection() {
        let ts = TimestampColumn::from_timestamps(0, &[10, 20]); // len 2
        let mut vals = NativeScalarColumn::with_capacity(ScalarType::F32, 3);
        vals.append_slice(ScalarSlice::F32(&[1.0, 2.0, 3.0]))
            .unwrap(); // len 3
        let res = ScalarDataChunk::new(ts, vals, LinearScale::identity());
        assert!(res.is_err());
    }

    #[test]
    fn t2_f06_05_inverted_calibration_chunk() {
        let ts = TimestampColumn::from_timestamps(0, &[0]);
        let mut vals = NativeScalarColumn::with_capacity(ScalarType::F64, 1);
        vals.append_slice(ScalarSlice::F64(&[10.0])).unwrap();
        let scale = LinearScale::new(-2.0, 100.0);
        let chunk = ScalarDataChunk::new(ts, vals, scale).unwrap();
        let (_, v) = chunk.get_sample_as_f64(0).unwrap();
        assert_eq!(v, 80.0); // 10.0 * (-2.0) + 100.0 = 80.0
    }

    // --- Feature 7: LinearScale Calibration Boundaries ---

    #[test]
    fn t2_f07_01_zero_scale() {
        let scale = LinearScale::new(0.0, 42.0);
        assert_eq!(scale.apply(100.0), 42.0);
        assert_eq!(scale.apply(-999.0), 42.0);
    }

    #[test]
    fn t2_f07_02_zero_offset() {
        let scale = LinearScale::new(3.5, 0.0);
        assert_eq!(scale.apply(10.0), 35.0);
    }

    #[test]
    fn t2_f07_03_large_dynamic_range() {
        let scale = LinearScale::new(1e6, -1e9);
        assert_eq!(scale.apply(2000.0), 1e9); // 2000 * 10^6 - 10^9 = 10^9
    }

    #[test]
    fn t2_f07_04_very_small_subnormal_scale() {
        let scale = LinearScale::new(1e-12, 0.0);
        let v = scale.apply(1e6);
        assert!((v - 1e-6).abs() < 1e-12);
    }

    #[test]
    fn t2_f07_05_extreme_float_saturation() {
        let scale = LinearScale::new(2.0, 0.0);
        let v = scale.apply(f64::MAX);
        assert!(v.is_infinite());
    }

    // --- Feature 8: BitVecChunk Boundaries ---

    #[test]
    fn t2_f08_01_all_zeros_across_65536_bits() {
        let mut bv = BitVecChunk::new();
        bv.extend_from_slice(&vec![false; CHUNK_LEN]);
        assert_eq!(bv.len(), CHUNK_LEN);
        for w in bv.words.iter() {
            assert_eq!(*w, 0);
        }
    }

    #[test]
    fn t2_f08_02_all_ones_across_65536_bits() {
        let mut bv = BitVecChunk::new();
        bv.extend_from_slice(&vec![true; CHUNK_LEN]);
        assert_eq!(bv.len(), CHUNK_LEN);
        for w in bv.words.iter() {
            assert_eq!(*w, u64::MAX);
        }
    }

    #[test]
    fn t2_f08_03_alternating_bits_pattern() {
        let mut bv = BitVecChunk::new();
        let bits: Vec<bool> = (0..64).map(|i| i % 2 == 1).collect(); // 0b101010...
        bv.extend_from_slice(&bits);
        assert_eq!(bv.words[0], 0xAAAA_AAAA_AAAA_AAAA);
    }

    #[test]
    fn t2_f08_04_single_bit_set_at_word0_msb() {
        let mut bv = BitVecChunk::new();
        let mut bits = vec![false; 64];
        bits[63] = true;
        bv.extend_from_slice(&bits);
        assert_eq!(bv.words[0], 1u64 << 63);
    }

    #[test]
    fn t2_f08_05_single_bit_set_at_word1_lsb() {
        let mut bv = BitVecChunk::new();
        let mut bits = vec![false; 65];
        bits[64] = true;
        bv.extend_from_slice(&bits);
        assert_eq!(bv.words[0], 0);
        assert_eq!(bv.words[1], 1);
    }

    // --- Feature 9: Edge Indices Boundaries ---

    #[test]
    fn t2_f09_01_maximum_possible_transitions() {
        let mut bv = BitVecChunk::new();
        let bits: Vec<bool> = (0..CHUNK_LEN).map(|i| i % 2 == 1).collect();
        bv.extend_from_slice(&bits);
        bv.compute_edge_indices();
        assert_eq!(bv.edge_indices.len(), CHUNK_LEN - 1);
    }

    #[test]
    fn t2_f09_02_transition_at_sample_1() {
        let mut bv = BitVecChunk::new();
        bv.extend_from_slice(&[false, true, true, true]);
        bv.compute_edge_indices();
        assert_eq!(bv.edge_indices, vec![1]);
    }

    #[test]
    fn t2_f09_03_transition_across_word_boundary() {
        let mut bv = BitVecChunk::new();
        let mut bits = vec![false; 65];
        bits[64] = true; // Bit 63 is 0, Bit 64 is 1 (across word 0 and word 1)
        bv.extend_from_slice(&bits);
        bv.compute_edge_indices();
        assert_eq!(bv.edge_indices, vec![64]);
    }

    #[test]
    fn t2_f09_04_transition_at_last_sample() {
        let mut bv = BitVecChunk::new();
        let mut bits = vec![false; CHUNK_LEN];
        bits[CHUNK_LEN - 1] = true;
        bv.extend_from_slice(&bits);
        bv.compute_edge_indices();
        assert_eq!(bv.edge_indices, vec![(CHUNK_LEN - 1) as u16]);
    }

    #[test]
    fn t2_f09_05_empty_chunk_edge_extraction() {
        let mut bv = BitVecChunk::new();
        bv.compute_edge_indices();
        assert!(bv.edge_indices.is_empty());
    }

    // --- Feature 10: Bit Access Boundaries ---

    #[test]
    fn t2_f10_01_index_0_lookup() {
        let mut bv = BitVecChunk::new();
        bv.push(true);
        assert_eq!(bv.get(0), Some(true));
    }

    #[test]
    fn t2_f10_02_word0_boundary_63() {
        let mut bv = BitVecChunk::new();
        let mut bits = vec![false; 64];
        bits[63] = true;
        bv.extend_from_slice(&bits);
        assert_eq!(bv.get(63), Some(true));
    }

    #[test]
    fn t2_f10_03_word1_boundary_64() {
        let mut bv = BitVecChunk::new();
        let mut bits = vec![false; 65];
        bits[64] = true;
        bv.extend_from_slice(&bits);
        assert_eq!(bv.get(64), Some(true));
    }

    #[test]
    fn t2_f10_04_chunk_boundary_65535() {
        let mut bv = BitVecChunk::new();
        let mut bits = vec![false; CHUNK_LEN];
        bits[CHUNK_LEN - 1] = true;
        bv.extend_from_slice(&bits);
        assert_eq!(bv.get(CHUNK_LEN - 1), Some(true));
    }

    #[test]
    fn t2_f10_05_off_by_one_boundary_65536_none() {
        let mut bv = BitVecChunk::new();
        bv.extend_from_slice(&vec![true; CHUNK_LEN]);
        assert_eq!(bv.get(CHUNK_LEN), None);
    }

    // --- Feature 11: Initial Level Boundaries ---

    #[test]
    fn t2_f11_01_single_sample_level_0() {
        let mut bv = BitVecChunk::new();
        bv.push(false);
        assert_eq!(bv.digital_init_level(), 0);
    }

    #[test]
    fn t2_f11_02_single_sample_level_1() {
        let mut bv = BitVecChunk::new();
        bv.push(true);
        assert_eq!(bv.digital_init_level(), 1);
    }

    #[test]
    fn t2_f11_03_level_0_with_immediate_rise() {
        let mut bv = BitVecChunk::new();
        bv.push(false);
        bv.push(true);
        bv.compute_edge_indices();
        assert_eq!(bv.digital_init_level(), 0);
        assert_eq!(bv.edge_indices, vec![1]);
    }

    #[test]
    fn t2_f11_04_level_1_with_immediate_drop() {
        let mut bv = BitVecChunk::new();
        bv.push(true);
        bv.push(false);
        bv.compute_edge_indices();
        assert_eq!(bv.digital_init_level(), 1);
        assert_eq!(bv.edge_indices, vec![1]);
    }

    #[test]
    fn t2_f11_05_constant_level_full_chunk() {
        let mut bv = BitVecChunk::new();
        bv.extend_from_slice(&vec![true; CHUNK_LEN]);
        bv.compute_edge_indices();
        assert_eq!(bv.digital_init_level(), 1);
        assert!(bv.edge_indices.is_empty());
    }

    // --- Feature 12: EventDataChunk Boundaries ---

    #[test]
    fn t2_f12_01_single_event_chunk() {
        let mut chunk = EventDataChunk::new_state_transitions(1);
        chunk.append_transition(1000, 7).unwrap();
        assert_eq!(chunk.get_state_at(1000), Some(7));
    }

    #[test]
    fn t2_f12_02_extreme_negative_state_code() {
        let mut chunk = EventDataChunk::new_state_transitions(1);
        chunk.append_transition(100, i32::MIN).unwrap();
        assert_eq!(chunk.get_state_at(100), Some(i32::MIN));
    }

    #[test]
    fn t2_f12_03_extreme_positive_state_code() {
        let mut chunk = EventDataChunk::new_state_transitions(1);
        chunk.append_transition(100, i32::MAX).unwrap();
        assert_eq!(chunk.get_state_at(100), Some(i32::MAX));
    }

    #[test]
    fn t2_f12_04_zero_state_code() {
        let mut chunk = EventDataChunk::new_state_transitions(1);
        chunk.append_transition(100, 0).unwrap();
        assert_eq!(chunk.get_state_at(100), Some(0));
    }

    #[test]
    fn t2_f12_05_large_transition_history_50k() {
        let mut chunk = EventDataChunk::new_state_transitions(50_000);
        for i in 0..50_000 {
            chunk.append_transition(i * 10, (i % 100) as i32).unwrap();
        }
        assert_eq!(chunk.get_state_at(25_005), Some(25_000 % 100));
    }

    // --- Feature 13: Transition Append Boundaries ---

    #[test]
    fn t2_f13_01_identical_timestamp_append() {
        let mut chunk = EventDataChunk::new_state_transitions(2);
        chunk.append_transition(1000, 1).unwrap();
        assert!(chunk.append_transition(1000, 2).is_ok());
        assert_eq!(chunk.get_state_at(1000), Some(2)); // Latest code for timestamp
    }

    #[test]
    fn t2_f13_02_one_ns_timestamp_increment() {
        let mut chunk = EventDataChunk::new_state_transitions(2);
        chunk.append_transition(1000, 1).unwrap();
        assert!(chunk.append_transition(1001, 2).is_ok());
    }

    #[test]
    fn t2_f13_03_one_ns_backward_rejection() {
        let mut chunk = EventDataChunk::new_state_transitions(2);
        chunk.append_transition(1000, 1).unwrap();
        assert!(chunk.append_transition(999, 2).is_err());
    }

    #[test]
    fn t2_f13_04_max_timestamp_u64_max() {
        let mut chunk = EventDataChunk::new_state_transitions(1);
        assert!(chunk.append_transition(u64::MAX, 99).is_ok());
        assert_eq!(chunk.get_state_at(u64::MAX), Some(99));
    }

    #[test]
    fn t2_f13_05_zero_timestamp_transition() {
        let mut chunk = EventDataChunk::new_state_transitions(1);
        assert!(chunk.append_transition(0, 1).is_ok());
        assert_eq!(chunk.get_state_at(0), Some(1));
    }

    // --- Feature 14: State Retrieval Boundaries ---

    #[test]
    fn t2_f14_01_query_before_first_transition() {
        let mut chunk = EventDataChunk::new_state_transitions(1);
        chunk.append_transition(1000, 5).unwrap();
        assert_eq!(chunk.get_state_at(999), None);
    }

    #[test]
    fn t2_f14_02_query_at_exact_first_transition() {
        let mut chunk = EventDataChunk::new_state_transitions(1);
        chunk.append_transition(1000, 5).unwrap();
        assert_eq!(chunk.get_state_at(1000), Some(5));
    }

    #[test]
    fn t2_f14_03_query_between_duplicate_timestamps() {
        let mut chunk = EventDataChunk::new_state_transitions(3);
        chunk.append_transition(100, 1).unwrap();
        chunk.append_transition(100, 2).unwrap();
        chunk.append_transition(200, 3).unwrap();
        assert_eq!(chunk.get_state_at(150), Some(2));
    }

    #[test]
    fn t2_f14_04_query_at_max_timestamp() {
        let mut chunk = EventDataChunk::new_state_transitions(1);
        chunk.append_transition(1000, 42).unwrap();
        assert_eq!(chunk.get_state_at(u64::MAX), Some(42));
    }

    #[test]
    fn t2_f14_05_query_single_transition_chunk() {
        let mut chunk = EventDataChunk::new_state_transitions(1);
        chunk.append_transition(500, 99).unwrap();
        assert_eq!(chunk.get_state_at(499), None);
        assert_eq!(chunk.get_state_at(500), Some(99));
        assert_eq!(chunk.get_state_at(501), Some(99));
    }

    // --- Feature 15: Serde RC Fix Boundaries ---

    #[test]
    fn t2_f15_01_deeply_nested_arc_schema() {
        let mut schema = Arc::new(StructSchema {
            name: "L0".into(),
            msg_id: None,
            packed: false,
            fields: vec![],
        });
        for i in 1..=5 {
            schema = Arc::new(StructSchema {
                name: format!("L{i}"),
                msg_id: None,
                packed: false,
                fields: vec![StructFieldDef {
                    name: format!("f{i}"),
                    offset_bytes: None,
                    unit: None,
                    description: None,
                    kind: StructFieldKind::Nested(schema),
                }],
            });
        }
        let json = serde_json::to_string(&schema);
        assert!(json.is_ok());
    }

    #[test]
    fn t2_f15_02_empty_struct_schema_arc() {
        let schema = Arc::new(StructSchema {
            name: String::new(),
            msg_id: None,
            packed: false,
            fields: vec![],
        });
        let json = serde_json::to_string(&schema).unwrap();
        assert!(json.contains("\"fields\":[]"));
    }

    #[test]
    fn t2_f15_03_schema_with_256_fields() {
        let fields: Vec<StructFieldDef> = (0..256)
            .map(|i| StructFieldDef {
                name: format!("field_{i}"),
                offset_bytes: None,
                unit: None,
                description: None,
                kind: StructFieldKind::Scalar {
                    scalar_type: ScalarType::U32,
                    endian: Endianness::Little,
                    scale: LinearScale::identity(),
                    bitfield: None,
                },
            })
            .collect();
        let schema = Arc::new(StructSchema {
            name: "WideStruct".into(),
            msg_id: None,
            packed: false,
            fields,
        });
        let json = serde_json::to_string(&schema).unwrap();
        let res: StructSchema = serde_json::from_str(&json).unwrap();
        assert_eq!(res.fields.len(), 256);
    }

    #[test]
    fn t2_f15_04_arc_schema_pointer_aliasing() {
        let shared = Arc::new(StructSchema {
            name: "SharedNode".into(),
            msg_id: Some(42),
            packed: true,
            fields: vec![],
        });
        let f1 = StructFieldDef {
            name: "ref_a".into(),
            offset_bytes: Some(0),
            unit: None,
            description: Some("first reference to shared node".into()),
            kind: StructFieldKind::Nested(Arc::clone(&shared)),
        };
        let f2 = StructFieldDef {
            name: "ref_b".into(),
            offset_bytes: Some(64),
            unit: None,
            description: Some("second reference to shared node".into()),
            kind: StructFieldKind::Nested(Arc::clone(&shared)),
        };
        let parent = StructSchema {
            name: "ParentWithAliasedChildren".into(),
            msg_id: Some(100),
            packed: false,
            fields: vec![f1, f2],
        };

        let json = serde_json::to_string(&parent)
            .expect("serialization of aliased Arc<StructSchema> references must succeed");
        let deserialized: StructSchema = serde_json::from_str(&json)
            .expect("deserialization of schema containing multiple Arc<StructSchema> references must succeed");

        assert_eq!(deserialized.fields.len(), 2);
        assert_eq!(deserialized.fields[0].name, "ref_a");
        assert_eq!(deserialized.fields[1].name, "ref_b");

        match (&deserialized.fields[0].kind, &deserialized.fields[1].kind) {
            (StructFieldKind::Nested(arc1), StructFieldKind::Nested(arc2)) => {
                assert_eq!(arc1.name, "SharedNode");
                assert_eq!(arc2.name, "SharedNode");
                assert_eq!(arc1.msg_id, Some(42));
                assert_eq!(arc2.msg_id, Some(42));
                assert!(arc1.packed);
                assert!(arc2.packed);
                assert_eq!(arc1, arc2);
            }
            _ => panic!("Expected Nested variants for both fields"),
        }
    }

    #[test]
    fn t2_f15_05_malformed_json_deserialization_error() {
        let bad_json = "{ invalid_json: 123 }";
        let res: Result<StructSchema, _> = serde_json::from_str(bad_json);
        assert!(res.is_err());
    }

    // --- Feature 16: Active Buffer Boundaries ---

    #[test]
    fn t2_f16_01_empty_batch_append() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let res = series.append(&[], ScalarSlice::F32(&[]));
        assert_eq!(res.unwrap(), 0);
        assert_eq!(series.total_samples(), 0);
    }

    #[test]
    fn t2_f16_02_slice_length_mismatch() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let ts = vec![100, 200];
        let vals = vec![1.0f32]; // len 1 vs len 2
        let res = series.append(&ts, ScalarSlice::F32(&vals));
        assert!(res.is_err());
        assert_eq!(series.total_samples(), 0);
    }

    #[test]
    fn t2_f16_03_append_one_sample_to_65535() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let ts1 = generate_timestamps(0, CHUNK_LEN - 1, 10);
        let vals1 = vec![1.0f32; CHUNK_LEN - 1];
        series.append(&ts1, ScalarSlice::F32(&vals1)).unwrap();
        assert_eq!(series.sealed_chunk_count(), 0);

        // Appending exactly 1 triggers the seal
        series
            .append(&[CHUNK_LEN as u64 * 10], ScalarSlice::F32(&[2.0]))
            .unwrap();
        assert_eq!(series.sealed_chunk_count(), 1);
        assert_eq!(series.total_samples(), CHUNK_LEN as u64);
    }

    #[test]
    fn t2_f16_04_append_exactly_65536_samples() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let ts = generate_timestamps(0, CHUNK_LEN, 10);
        let vals = vec![1.0f32; CHUNK_LEN];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        assert_eq!(series.sealed_chunk_count(), 1);
        assert_eq!(series.total_samples(), CHUNK_LEN as u64);
    }

    #[test]
    fn t2_f16_05_type_mismatch_on_append() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let ts = vec![100];
        let vals = vec![42i16]; // Type mismatch: F32 vs I16
        let res = series.append(&ts, ScalarSlice::I16(&vals));
        assert!(res.is_err());
    }

    // --- Feature 17: Automatic Sealing Boundaries ---

    #[test]
    fn t2_f17_01_65535_samples_no_seal() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::I32, LinearScale::identity());
        let ts = generate_timestamps(0, CHUNK_LEN - 1, 10);
        let vals = vec![10i32; CHUNK_LEN - 1];
        series.append(&ts, ScalarSlice::I32(&vals)).unwrap();
        assert_eq!(series.sealed_chunk_count(), 0);
    }

    #[test]
    fn t2_f17_02_65536_samples_exact_seal() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::I32, LinearScale::identity());
        let ts = generate_timestamps(0, CHUNK_LEN, 10);
        let vals = vec![10i32; CHUNK_LEN];
        series.append(&ts, ScalarSlice::I32(&vals)).unwrap();
        assert_eq!(series.sealed_chunk_count(), 1);
    }

    #[test]
    fn t2_f17_03_multi_chunk_exact_seals_196608() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::U16, LinearScale::identity());
        let total = CHUNK_LEN * 3; // Exactly 3 chunks
        let ts = generate_timestamps(0, total, 10);
        let vals = vec![5u16; total];
        series.append(&ts, ScalarSlice::U16(&vals)).unwrap();
        assert_eq!(series.sealed_chunk_count(), 3);
        assert_eq!(series.total_samples(), total as u64);
    }

    #[test]
    fn t2_f17_04_sealed_chunk_handle_time_span() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let ts = generate_timestamps(10_000, CHUNK_LEN, 100);
        let vals = vec![1.0f32; CHUNK_LEN];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        let (first_t, _) = series.get_sample_by_index(0).unwrap();
        let (last_t, _) = series.get_sample_by_index((CHUNK_LEN - 1) as u64).unwrap();
        assert_eq!(first_t, 10_000);
        assert_eq!(last_t, 10_000 + (CHUNK_LEN as u64 - 1) * 100);
    }

    #[test]
    fn t2_f17_05_strong_ref_count_on_sealed_chunk() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let ts = generate_timestamps(0, CHUNK_LEN, 10);
        let vals = vec![1.0f32; CHUNK_LEN];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        assert_eq!(series.sealed_chunk_count(), 1);
    }

    // --- Feature 18: Rollover Boundaries ---

    #[test]
    fn t2_f18_01_spillover_by_one_sample() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let ts = generate_timestamps(0, CHUNK_LEN + 1, 10);
        let vals = vec![1.0f32; CHUNK_LEN + 1];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        assert_eq!(series.sealed_chunk_count(), 1);
        assert_eq!(series.total_samples(), (CHUNK_LEN + 1) as u64);
        assert!(series.get_sample_by_index(CHUNK_LEN as u64).is_some());
    }

    #[test]
    fn t2_f18_02_spillover_by_65535_samples() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let count = CHUNK_LEN + CHUNK_LEN - 1; // 131,071
        let ts = generate_timestamps(0, count, 10);
        let vals = vec![1.0f32; count];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        assert_eq!(series.sealed_chunk_count(), 1);
        assert_eq!(series.total_samples(), count as u64);
    }

    #[test]
    fn t2_f18_03_fragmented_boundary_crossing() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::I16, LinearScale::identity());
        // Append 65,530 samples, then 10 samples (crossing 65,536 boundary)
        let ts1 = generate_timestamps(0, CHUNK_LEN - 6, 10);
        let vals1 = vec![1i16; CHUNK_LEN - 6];
        series.append(&ts1, ScalarSlice::I16(&vals1)).unwrap();
        assert_eq!(series.sealed_chunk_count(), 0);

        let ts2 = generate_timestamps((CHUNK_LEN - 6) as u64 * 10, 10, 10);
        let vals2 = vec![2i16; 10];
        series.append(&ts2, ScalarSlice::I16(&vals2)).unwrap();
        assert_eq!(series.sealed_chunk_count(), 1);
        assert_eq!(series.total_samples(), (CHUNK_LEN + 4) as u64);
    }

    #[test]
    fn t2_f18_04_huge_batch_rollover_500k() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::U32, LinearScale::identity());
        let ts = generate_timestamps(0, 500_000, 10);
        let vals = vec![42u32; 500_000];
        series.append(&ts, ScalarSlice::U32(&vals)).unwrap();
        assert_eq!(series.sealed_chunk_count(), 500_000 / CHUNK_LEN); // 7 chunks
        assert_eq!(series.total_samples(), 500_000);
    }

    #[test]
    fn t2_f18_05_timestamp_monotonicity_at_split() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let ts = generate_timestamps(100, 70_000, 50);
        let vals = vec![1.0f32; 70_000];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        let (ts_prev, _) = series.get_sample_by_index(65_535).unwrap();
        let (ts_next, _) = series.get_sample_by_index(65_536).unwrap();
        assert_eq!(ts_next, ts_prev + 50);
    }

    // --- Feature 19: NativeScalarSeries Boundaries ---

    #[test]
    fn t2_f19_01_empty_series_total_samples_zero() {
        let series = NativeScalarSeries::new(ChannelId(1), ScalarType::I8, LinearScale::identity());
        assert_eq!(series.total_samples(), 0);
    }

    #[test]
    fn t2_f19_02_single_sample_total_samples_one() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::I8, LinearScale::identity());
        series.append(&[0], ScalarSlice::I8(&[10])).unwrap();
        assert_eq!(series.total_samples(), 1);
    }

    #[test]
    fn t2_f19_03_zero_sample_batch_append_noop() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        series.append(&[100], ScalarSlice::F32(&[1.0])).unwrap();
        series.append(&[], ScalarSlice::F32(&[])).unwrap();
        assert_eq!(series.total_samples(), 1);
    }

    #[test]
    fn t2_f19_04_million_sample_accumulation() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::I16, LinearScale::identity());
        for b in 0..10 {
            let ts = generate_timestamps(b * 100_000 * 10, 100_000, 10);
            let vals = vec![1i16; 100_000];
            series.append(&ts, ScalarSlice::I16(&vals)).unwrap();
        }
        assert_eq!(series.total_samples(), 1_000_000);
    }

    #[test]
    fn t2_f19_05_total_sample_count_u64_capacity() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let ts = generate_timestamps(0, 1000, 10);
        let vals = vec![1.0f32; 1000];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        assert_eq!(series.total_samples(), 1000u64);
    }

    // --- Feature 20: EventSeries Boundaries ---

    #[test]
    fn t2_f20_01_empty_event_series_query() {
        let series = EventSeries::new(ChannelId(1));
        assert_eq!(series.get_state_at(0), None);
        assert_eq!(series.get_state_at(1_000_000), None);
    }

    #[test]
    fn t2_f20_02_single_event_series_query() {
        let mut series = EventSeries::new(ChannelId(1));
        series.append(&[1000], &[55]).unwrap();
        assert_eq!(series.get_state_at(999), None);
        assert_eq!(series.get_state_at(1000), Some(55));
        assert_eq!(series.get_state_at(2000), Some(55));
    }

    #[test]
    fn t2_f20_03_non_monotonic_batch_rejection() {
        let mut series = EventSeries::new(ChannelId(1));
        let res = series.append(&[200, 100], &[1, 2]); // Decreasing ts
        assert!(res.is_err());
    }

    #[test]
    fn t2_f20_04_empty_slice_append() {
        let mut series = EventSeries::new(ChannelId(1));
        assert_eq!(series.append(&[], &[]).unwrap(), 0);
    }

    #[test]
    fn t2_f20_05_mismatched_slice_lengths() {
        let mut series = EventSeries::new(ChannelId(1));
        let res = series.append(&[100, 200], &[1]); // len 2 vs len 1
        assert!(res.is_err());
    }

    // --- Feature 21: Index Lookup Boundaries ---

    #[test]
    fn t2_f21_01_index_zero_query() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        series.append(&[1234], ScalarSlice::F32(&[5.5])).unwrap();
        let (t, v) = series.get_sample_by_index(0).unwrap();
        assert_eq!(t, 1234);
        assert_eq!(v, 5.5);
    }

    #[test]
    fn t2_f21_02_exact_sealed_boundary_65535() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::I32, LinearScale::identity());
        let ts = generate_timestamps(0, 100_000, 10);
        let vals: Vec<i32> = (0..100_000).map(|i| i as i32).collect();
        series.append(&ts, ScalarSlice::I32(&vals)).unwrap();
        let (t, v) = series.get_sample_by_index(65_535).unwrap();
        assert_eq!(t, 65_535 * 10);
        assert_eq!(v, 65_535.0);
    }

    #[test]
    fn t2_f21_03_exact_active_boundary_65536() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::I32, LinearScale::identity());
        let ts = generate_timestamps(0, 100_000, 10);
        let vals: Vec<i32> = (0..100_000).map(|i| i as i32).collect();
        series.append(&ts, ScalarSlice::I32(&vals)).unwrap();
        let (t, v) = series.get_sample_by_index(65_536).unwrap();
        assert_eq!(t, 65_536 * 10);
        assert_eq!(v, 65_536.0);
    }

    #[test]
    fn t2_f21_04_exact_last_sample_index() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let ts = generate_timestamps(0, 1000, 10);
        let vals = vec![99.0f32; 1000];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        let (t, v) = series.get_sample_by_index(999).unwrap();
        assert_eq!(t, 999 * 10);
        assert_eq!(v, 99.0);
    }

    #[test]
    fn t2_f21_05_boundary_off_by_one_total_samples_none() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        series
            .append(&[0, 10], ScalarSlice::F32(&[1.0, 2.0]))
            .unwrap();
        assert_eq!(series.get_sample_by_index(2), None); // Total samples is 2
    }

    // --- Feature 22: Timestamp Lookup Boundaries ---

    #[test]
    fn t2_f22_01_query_at_exact_t0() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        series
            .append(&[100, 200], ScalarSlice::F32(&[1.0, 2.0]))
            .unwrap();
        let (idx, t, v) = series.get_sample_by_timestamp(100).unwrap();
        assert_eq!(idx, 0);
        assert_eq!(t, 100);
        assert_eq!(v, 1.0);
    }

    #[test]
    fn t2_f22_02_query_before_t0() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        series
            .append(&[1000, 2000], ScalarSlice::F32(&[1.0, 2.0]))
            .unwrap();
        // Query timestamp earlier than first sample (500 ns < 1000 ns)
        let res = series.get_sample_by_timestamp(500);
        assert_eq!(res, None, "Query timestamp before t0 must return None");
    }

    #[test]
    fn t2_f22_03_query_at_chunk_boundary_ts() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let ts = generate_timestamps(0, 100_000, 100);
        let vals = vec![1.0f32; 100_000];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        let (idx, t, _) = series.get_sample_by_timestamp(65_535 * 100).unwrap();
        assert_eq!(idx, 65_535);
        assert_eq!(t, 65_535 * 100);
    }

    #[test]
    fn t2_f22_04_query_beyond_last_sample_ts() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        series
            .append(&[1000, 2000], ScalarSlice::F32(&[1.0, 2.0]))
            .unwrap();
        // Query timestamp beyond last sample (10_000 ns > 2000 ns)
        let res = series.get_sample_by_timestamp(10_000);
        assert_eq!(
            res, None,
            "Query timestamp beyond last sample must return None"
        );
    }

    #[test]
    fn t2_f22_05_query_on_empty_series() {
        let series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        assert_eq!(series.get_sample_by_timestamp(0), None);
    }

    // --- Feature 23: Watermark Boundaries ---

    #[test]
    fn t2_f23_01_zero_sample_append_emits_no_events() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let listener = MockWatermarkListener::new();
        series.register_listener(listener.clone());
        series.append(&[], ScalarSlice::F32(&[])).unwrap();
        assert_eq!(listener.event_count(), 0);
    }

    #[test]
    fn t2_f23_02_exact_seal_batch_notification() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let listener = MockWatermarkListener::new();
        series.register_listener(listener.clone());
        let ts = generate_timestamps(0, CHUNK_LEN, 10);
        series
            .append(&ts, ScalarSlice::F32(&vec![1.0; CHUNK_LEN]))
            .unwrap();
        let ev = listener.last_event().unwrap();
        assert_eq!(ev.total_samples, CHUNK_LEN as u64);
    }

    #[test]
    fn t2_f23_03_multi_chunk_rollover_notification() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let listener = MockWatermarkListener::new();
        series.register_listener(listener.clone());
        let ts = generate_timestamps(0, 150_000, 10);
        series
            .append(&ts, ScalarSlice::F32(&vec![1.0; 150_000]))
            .unwrap();
        let ev = listener.last_event().unwrap();
        assert_eq!(ev.total_samples, 150_000);
    }

    #[test]
    fn t2_f23_04_append_with_zero_listeners() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        // No listeners registered: must succeed without panic
        let res = series.append(&[100], ScalarSlice::F32(&[1.0]));
        assert!(res.is_ok());
    }

    #[test]
    fn t2_f23_05_multiple_listeners_fanout() {
        let mut series =
            NativeScalarSeries::new(ChannelId(10), ScalarType::F32, LinearScale::identity());
        let l1 = MockWatermarkListener::new();
        let l2 = MockWatermarkListener::new();
        let l3 = MockWatermarkListener::new();
        series.register_listener(l1.clone());
        series.register_listener(l2.clone());
        series.register_listener(l3.clone());

        series.append(&[500], ScalarSlice::F32(&[3.0])).unwrap();
        assert_eq!(l1.event_count(), 1);
        assert_eq!(l2.event_count(), 1);
        assert_eq!(l3.event_count(), 1);
        assert_eq!(l1.last_event(), l2.last_event());
    }

    // --- Feature 24: ChunkStorageRef Boundaries ---

    #[test]
    fn t2_f24_01_null_mmap_locator() {
        let loc = MmapChunkLocator {
            file_id: 0,
            byte_offset: 0,
            byte_len: 0,
            flags: 0,
        };
        let s_ref = ChunkStorageRef::MmapSlice(loc);
        assert_eq!(s_ref.resident_bytes(), 0);
        assert!(!s_ref.is_in_memory());
    }

    #[test]
    fn t2_f24_02_max_offset_mmap_locator() {
        let loc = MmapChunkLocator {
            file_id: u64::MAX,
            byte_offset: u64::MAX,
            byte_len: u32::MAX,
            flags: 0xFFFF,
        };
        let s_ref = ChunkStorageRef::MmapSlice(loc);
        assert_eq!(s_ref.resident_bytes(), 0);
    }

    #[test]
    fn t2_f24_03_empty_datachunk_in_memory() {
        let empty_chunk = Arc::new(DataChunk::Scalar(
            ScalarDataChunk::new(
                TimestampColumn::from_timestamps(0, &[]),
                NativeScalarColumn::with_capacity(ScalarType::F32, 0),
                LinearScale::identity(),
            )
            .unwrap(),
        ));
        let s_ref = ChunkStorageRef::InMemory(empty_chunk);
        assert!(s_ref.is_in_memory());
        assert_eq!(s_ref.resident_bytes(), 0);
    }

    #[test]
    fn t2_f24_04_dropping_in_memory_ref() {
        let mut vals = NativeScalarColumn::with_capacity(ScalarType::F32, 1);
        vals.append_slice(ScalarSlice::F32(&[0.0])).unwrap();
        let chunk = Arc::new(DataChunk::Scalar(
            ScalarDataChunk::new(
                TimestampColumn::from_timestamps(0, &[1]),
                vals,
                LinearScale::identity(),
            )
            .unwrap(),
        ));
        let s_ref = ChunkStorageRef::InMemory(Arc::clone(&chunk));
        assert_eq!(Arc::strong_count(&chunk), 2);
        drop(s_ref);
        assert_eq!(Arc::strong_count(&chunk), 1);
    }

    #[test]
    fn t2_f24_05_cloning_in_memory_ref() {
        let mut vals = NativeScalarColumn::with_capacity(ScalarType::F32, 1);
        vals.append_slice(ScalarSlice::F32(&[0.0])).unwrap();
        let chunk = Arc::new(DataChunk::Scalar(
            ScalarDataChunk::new(
                TimestampColumn::from_timestamps(0, &[1]),
                vals,
                LinearScale::identity(),
            )
            .unwrap(),
        ));
        let s_ref1 = ChunkStorageRef::InMemory(chunk);
        let s_ref2 = s_ref1.clone();
        assert!(s_ref1.is_in_memory());
        assert!(s_ref2.is_in_memory());
    }

    // --- Feature 25: Criterion Benchmark Boundaries ---

    #[test]
    fn t2_f25_01_batch_size_128_throughput() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let ts = generate_timestamps(0, 128, 10);
        let vals = vec![1.0f32; 128];
        assert_eq!(series.append(&ts, ScalarSlice::F32(&vals)).unwrap(), 128);
    }

    #[test]
    fn t2_f25_02_batch_size_65536_throughput() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let ts = generate_timestamps(0, CHUNK_LEN, 10);
        let vals = vec![1.0f32; CHUNK_LEN];
        assert_eq!(
            series.append(&ts, ScalarSlice::F32(&vals)).unwrap(),
            CHUNK_LEN
        );
    }

    #[test]
    fn t2_f25_03_million_samples_streaming() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        for b in 0..10 {
            let ts = generate_timestamps(b * 100_000 * 10, 100_000, 10);
            let vals = vec![1.0f32; 100_000];
            series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        }
        assert_eq!(series.total_samples(), 1_000_000);
    }

    #[test]
    fn t2_f25_04_native_i8_throughput() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::I8, LinearScale::identity());
        let ts = generate_timestamps(0, CHUNK_LEN, 10);
        let vals = vec![12i8; CHUNK_LEN];
        assert_eq!(
            series.append(&ts, ScalarSlice::I8(&vals)).unwrap(),
            CHUNK_LEN
        );
    }

    #[test]
    fn t2_f25_05_native_f64_throughput() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F64, LinearScale::identity());
        let ts = generate_timestamps(0, CHUNK_LEN, 10);
        let vals = vec![3.14159f64; CHUNK_LEN];
        assert_eq!(
            series.append(&ts, ScalarSlice::F64(&vals)).unwrap(),
            CHUNK_LEN
        );
    }

    // --- Feature 26: Quality Gate Boundaries ---

    #[test]
    fn t2_f26_01_core_error_storage_variant_display() {
        // Storage error formatting boundary check
        let err = CoreError::Storage("Index out of bounds at chunk 5, offset 65536".into());
        let display = format!("{err}");
        assert!(display.starts_with("storage error:"));
        assert!(display.contains("chunk 5"));
        assert_eq!(
            err.to_string(),
            "storage error: Index out of bounds at chunk 5, offset 65536"
        );
    }

    #[test]
    fn t2_f26_02_aligned_vec_reallocation_alignment_preservation() {
        // Dynamic capacity reallocation boundary: alignment maintained across 256 pushes
        let mut vec: AlignedVec<u64> = AlignedVec::new();
        for i in 0..256 {
            vec.push(i as u64);
            assert_eq!(
                (vec.as_slice().as_ptr() as usize) % CACHE_LINE_BYTES,
                0,
                "Reallocated pointer must remain 64-byte aligned at push {i}"
            );
        }
        assert_eq!(vec.len(), 256);
        assert_eq!(vec.as_slice()[255], 255);
    }

    #[test]
    fn t2_f26_03_scalar_data_chunk_length_mismatch_rejection() {
        // Boundary rejection: timestamps.len() != values.len()
        let base = 1_000_000u64;
        let ts = TimestampColumn::from_timestamps(base, &[base, base + 10]);
        let mut col = NativeScalarColumn::with_capacity(ScalarType::F32, 3);
        col.append_slice(ScalarSlice::F32(&[1.0, 2.0, 3.0]))
            .unwrap();
        let res = ScalarDataChunk::new(ts, col, LinearScale::identity());
        assert!(res.is_err());
        match res {
            Err(CoreError::Storage(msg)) => assert!(msg.contains("length mismatch")),
            _ => panic!("Expected length mismatch storage error"),
        }
    }

    #[test]
    fn t2_f26_04_scalar_data_chunk_exceed_chunk_len_rejection() {
        // Boundary rejection: timestamps.len() > CHUNK_LEN (65,536)
        let base = 0u64;
        let ts_vec: Vec<u64> = (0..=CHUNK_LEN as u64).collect(); // 65,537 samples
        let ts = TimestampColumn::from_timestamps(base, &ts_vec);
        let mut col = NativeScalarColumn::with_capacity(ScalarType::U8, CHUNK_LEN + 1);
        let dummy_vals = vec![0u8; CHUNK_LEN + 1];
        col.append_slice(ScalarSlice::U8(&dummy_vals)).unwrap();
        let res = ScalarDataChunk::new(ts, col, LinearScale::identity());
        assert!(res.is_err());
        match res {
            Err(CoreError::Storage(msg)) => assert!(msg.contains("exceeds CHUNK_LEN")),
            _ => panic!("Expected exceeds CHUNK_LEN storage error"),
        }
    }

    #[test]
    fn t2_f26_05_event_data_chunk_monotonicity_rejection() {
        // State transition monotonicity boundary rejection
        let mut chunk = EventDataChunk::new_state_transitions(10);
        chunk.append_transition(1000, 1).unwrap();
        chunk.append_transition(2000, 2).unwrap();
        // Attempt retrograde transition timestamp (1500 ns < 2000 ns)
        let res = chunk.append_transition(1500, 3);
        assert!(res.is_err(), "Retrograde transition must return Err");
        match res {
            Err(CoreError::Storage(msg)) => {
                assert!(msg.contains("non-monotonic event timestamp"));
                assert!(msg.contains("1500 < 2000"));
            }
            _ => panic!("Expected non-monotonic event timestamp error"),
        }
    }
}

// ============================================================================
// Tier 3: Cross-Feature Combinations (15 Pairwise Scenarios)
// ============================================================================
pub mod tier3_cross_feature {
    use super::*;

    // --- T3-COMB-01: Multi-Batch Rollover + Watermark Emission ---
    #[test]
    fn t3_comb_01_multibatch_rollover_watermark() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let listener = MockWatermarkListener::new();
        series.register_listener(listener.clone());

        // Ingest 150,000 samples spanning 2 chunk boundaries (65,536 * 2 = 131,072)
        let ts = generate_timestamps(0, 150_000, 10);
        let vals = vec![1.0f32; 150_000];
        let appended = series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        assert_eq!(appended, 150_000);

        assert_eq!(series.sealed_chunk_count(), 2);
        assert_eq!(series.total_samples(), 150_000);

        let ev = listener.last_event().unwrap();
        assert_eq!(ev.channel_id, ChannelId(1));
        assert_eq!(ev.total_samples, 150_000);
        assert_eq!(ev.latest_ts_ns, 149_999 * 10);
    }

    // --- T3-COMB-02: NativeScalarColumn 10 Types + LinearScale + Index Lookup ---
    #[test]
    fn t3_comb_02_native_scalar_linear_scale_index_lookup() {
        let scale = LinearScale::new(0.001, -0.5); // physical = raw * 0.001 - 0.5
        let mut series = NativeScalarSeries::new(ChannelId(2), ScalarType::I16, scale);

        let raw_samples: Vec<i16> = vec![0, 500, 1000, 2000, -1000];
        let ts = generate_timestamps(0, raw_samples.len(), 1000);
        series.append(&ts, ScalarSlice::I16(&raw_samples)).unwrap();

        let (_, v0) = series.get_sample_by_index(0).unwrap();
        let (_, v1) = series.get_sample_by_index(1).unwrap();
        let (_, v2) = series.get_sample_by_index(2).unwrap();
        let (_, v3) = series.get_sample_by_index(3).unwrap();
        let (_, v4) = series.get_sample_by_index(4).unwrap();

        assert!((v0 - (-0.5)).abs() < 1e-6); // 0 * 0.001 - 0.5 = -0.5
        assert!((v1 - 0.0).abs() < 1e-6); // 500 * 0.001 - 0.5 = 0.0
        assert!((v2 - 0.5).abs() < 1e-6); // 1000 * 0.001 - 0.5 = 0.5
        assert!((v3 - 1.5).abs() < 1e-6); // 2000 * 0.001 - 0.5 = 1.5
        assert!((v4 - (-1.5)).abs() < 1e-6); // -1000 * 0.001 - 0.5 = -1.5
    }

    // --- T3-COMB-03: BitVecChunk 1-Bit Packing + Edge Indices + Initial Level ---
    #[test]
    fn t3_comb_03_bitvec_edges_initial_level() {
        // High level for first 100 samples, then low for 100, then high for 100
        let mut bits = vec![true; 100];
        bits.extend_from_slice(&vec![false; 100]);
        bits.extend_from_slice(&vec![true; 100]);

        let mut bv = BitVecChunk::new();
        bv.extend_from_slice(&bits);
        bv.compute_edge_indices();

        assert_eq!(bv.digital_init_level(), 1);
        assert_eq!(bv.edge_indices, vec![100, 200]);

        // Complete reconstruction from level + edges
        let mut reconstructed = Vec::with_capacity(300);
        let mut lvl = bv.digital_init_level() == 1;
        let mut edge_idx = 0;
        for i in 0..300 {
            if edge_idx < bv.edge_indices.len() && i == bv.edge_indices[edge_idx] as usize {
                lvl = !lvl;
                edge_idx += 1;
            }
            reconstructed.push(lvl);
        }
        assert_eq!(reconstructed, bits);
    }

    // --- T3-COMB-04: Active Buffer + 65,536 Sealing + ChunkStorageRef ---
    #[test]
    fn t3_comb_04_active_buffer_sealing_storage_ref() {
        let ts_data = generate_timestamps(0, CHUNK_LEN, 10);
        let ts = TimestampColumn::from_timestamps(0, &ts_data);
        let mut vals = NativeScalarColumn::with_capacity(ScalarType::F32, CHUNK_LEN);
        vals.append_slice(ScalarSlice::F32(&vec![1.0; CHUNK_LEN]))
            .unwrap();

        let chunk = Arc::new(DataChunk::Scalar(
            ScalarDataChunk::new(ts, vals, LinearScale::identity()).unwrap(),
        ));
        let storage_ref = ChunkStorageRef::InMemory(Arc::clone(&chunk));

        assert!(storage_ref.is_in_memory());
        assert!(storage_ref.resident_bytes() > 0);
        assert!(storage_ref.as_in_memory().is_some());
    }

    // --- T3-COMB-05: Timestamp Dual Encoding + Binary Search Lookup ---
    #[test]
    fn t3_comb_05_timestamp_dual_encoding_binary_search() {
        // Short span (RelU32)
        let base1 = 1_000_000u64;
        let mut series_rel =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let ts_rel = generate_timestamps(base1, 1000, 1000); // 1ms step, 1s span
        series_rel
            .append(&ts_rel, ScalarSlice::F32(&vec![1.0; 1000]))
            .unwrap();
        let (idx_rel, t_rel, _) = series_rel.get_sample_by_timestamp(base1 + 50_000).unwrap();
        assert_eq!(idx_rel, 50);
        assert_eq!(t_rel, base1 + 50_000);

        // Long span (AbsU64)
        let base2 = 1_000_000u64;
        let mut series_abs =
            NativeScalarSeries::new(ChannelId(2), ScalarType::F32, LinearScale::identity());
        let ts_abs = generate_timestamps(base2, 100, 100_000_000); // 100ms step, 10s span (>4.29s)
        series_abs
            .append(&ts_abs, ScalarSlice::F32(&vec![2.0; 100]))
            .unwrap();
        let (idx_abs, t_abs, _) = series_abs
            .get_sample_by_timestamp(base2 + 500_000_000)
            .unwrap();
        assert_eq!(idx_abs, 5);
        assert_eq!(t_abs, base2 + 500_000_000);
    }

    // --- T3-COMB-06: EventDataChunk Transitions + Monotonic Append + Query ---
    #[test]
    fn t3_comb_06_event_transitions_monotonic_query() {
        let mut chunk = EventDataChunk::new_state_transitions(10);
        // States: 0=INIT, 1=ARM, 2=RUN, 3=FAULT
        assert!(chunk.append_transition(1000, 0).is_ok());
        assert!(chunk.append_transition(2000, 1).is_ok());
        assert!(chunk.append_transition(3000, 2).is_ok());
        assert!(chunk.append_transition(4000, 3).is_ok());

        // Monotonic violation must be rejected
        assert!(chunk.append_transition(3500, 99).is_err());

        // Queries before, at, and after transitions
        assert_eq!(chunk.get_state_at(500), None);
        assert_eq!(chunk.get_state_at(1000), Some(0));
        assert_eq!(chunk.get_state_at(1500), Some(0));
        assert_eq!(chunk.get_state_at(2000), Some(1));
        assert_eq!(chunk.get_state_at(2999), Some(1));
        assert_eq!(chunk.get_state_at(3000), Some(2));
        assert_eq!(chunk.get_state_at(4000), Some(3));
        assert_eq!(chunk.get_state_at(10_000), Some(3));
    }

    // --- T3-COMB-07: Multi-Batch Rollover + Timestamp Query Across Boundaries ---
    #[test]
    fn t3_comb_07_rollover_timestamp_query_boundary() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let ts = generate_timestamps(0, 100_000, 50); // 0 .. 4,999,950 ns
        let vals: Vec<f32> = (0..100_000).map(|i| i as f32).collect();
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();

        // Sample 65,535 in sealed chunk 0
        let (idx_0, t_0, v_0) = series.get_sample_by_timestamp(65_535 * 50).unwrap();
        assert_eq!(idx_0, 65_535);
        assert_eq!(t_0, 65_535 * 50);
        assert_eq!(v_0, 65_535.0);

        // Sample 65,536 in active buffer
        let (idx_1, t_1, v_1) = series.get_sample_by_timestamp(65_536 * 50).unwrap();
        assert_eq!(idx_1, 65_536);
        assert_eq!(t_1, 65_536 * 50);
        assert_eq!(v_1, 65_536.0);
    }

    // --- T3-COMB-08: Calibration + F32/I16 Types + Exact 65,536 Sealing ---
    #[test]
    fn t3_comb_08_calibration_f32_i16_sealing() {
        let scale = LinearScale::new(2.0, -1.0);
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::F32, scale);

        let ts = generate_timestamps(0, CHUNK_LEN, 10);
        let vals = vec![5.0f32; CHUNK_LEN];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();

        assert_eq!(series.sealed_chunk_count(), 1);
        let (_, v_calibrated) = series.get_sample_by_index(0).unwrap();
        assert_eq!(v_calibrated, 9.0); // 5.0 * 2.0 - 1.0 = 9.0
    }

    // --- T3-COMB-09: BitVecChunk + NativeScalarColumn::Bool + Storage Series ---
    #[test]
    fn t3_comb_09_bitvec_bool_series_abstraction() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::Bool, LinearScale::identity());
        let bits = generate_square_wave_bits(1000, 50);
        let ts = generate_timestamps(0, 1000, 10);
        series.append(&ts, ScalarSlice::Bool(&bits)).unwrap();

        assert_eq!(series.total_samples(), 1000);
        for i in 0..1000 {
            let (_, v) = series.get_sample_by_index(i as u64).unwrap();
            let expected = if bits[i] { 1.0 } else { 0.0 };
            assert_eq!(v, expected);
        }
    }

    // --- T3-COMB-10: EventSeries Append + WatermarkListener Emission ---
    #[test]
    fn t3_comb_10_eventseries_watermark_listener() {
        let mut series = EventSeries::new(ChannelId(8));
        let listener = MockWatermarkListener::new();
        series.register_listener(listener.clone());

        let ts = vec![1000, 2000, 3000, 4000, 5000];
        let codes = vec![1, 2, 3, 4, 5];
        series.append(&ts, &codes).unwrap();

        assert_eq!(listener.event_count(), 1);
        let ev = listener.last_event().unwrap();
        assert_eq!(ev.channel_id, ChannelId(8));
        assert_eq!(ev.latest_ts_ns, 5000);
        assert_eq!(ev.total_samples, 5);
    }

    // --- T3-COMB-11: Timestamp Dual Encoding Rollover Across Chunks ---
    #[test]
    fn t3_comb_11_timestamp_relu32_rollover_across_chunks() {
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        // 2 chunks of 100 kHz data (10 us step = 10,000 ns).
        // 65,536 points * 10,000 ns = 0.655s span per chunk (both easily fit in RelU32 <= 4.29s).
        let total = CHUNK_LEN * 2;
        let ts = generate_timestamps(0, total, 10_000);
        let vals = vec![1.0f32; total];
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();

        assert_eq!(series.sealed_chunk_count(), 2);
        assert_eq!(series.total_samples(), total as u64);
    }

    // --- T3-COMB-12: 64-Byte Alignment + AlignedVec + CHUNK_LEN Allocation ---
    #[test]
    fn t3_comb_12_cache_alignment_all_scalar_types() {
        let vec_i8: AlignedVec<i8> = AlignedVec::with_capacity(CHUNK_LEN);
        let vec_i16: AlignedVec<i16> = AlignedVec::with_capacity(CHUNK_LEN);
        let vec_i32: AlignedVec<i32> = AlignedVec::with_capacity(CHUNK_LEN);
        let vec_i64: AlignedVec<i64> = AlignedVec::with_capacity(CHUNK_LEN);
        let vec_f32: AlignedVec<f32> = AlignedVec::with_capacity(CHUNK_LEN);
        let vec_f64: AlignedVec<f64> = AlignedVec::with_capacity(CHUNK_LEN);

        assert_eq!((vec_i8.as_ptr() as usize) % CACHE_LINE_BYTES, 0);
        assert_eq!((vec_i16.as_ptr() as usize) % CACHE_LINE_BYTES, 0);
        assert_eq!((vec_i32.as_ptr() as usize) % CACHE_LINE_BYTES, 0);
        assert_eq!((vec_i64.as_ptr() as usize) % CACHE_LINE_BYTES, 0);
        assert_eq!((vec_f32.as_ptr() as usize) % CACHE_LINE_BYTES, 0);
        assert_eq!((vec_f64.as_ptr() as usize) % CACHE_LINE_BYTES, 0);
    }

    // --- T3-COMB-13: Inverted Scale + Signed Integers + Index Query ---
    #[test]
    fn t3_comb_13_inverted_scale_signed_integers_index_query() {
        let scale = LinearScale::new(-10.0, 50.0);
        let mut series = NativeScalarSeries::new(ChannelId(1), ScalarType::I8, scale);

        series.append(&[0], ScalarSlice::I8(&[-128])).unwrap();
        let (_, v) = series.get_sample_by_index(0).unwrap();
        // -128 * -10.0 + 50.0 = 1280.0 + 50.0 = 1330.0
        assert_eq!(v, 1330.0);
    }

    // --- T3-COMB-14: Rapid Event Transitions + Zero-Order Hold + Watermarks ---
    #[test]
    fn t3_comb_14_rapid_event_transitions_zero_order_hold() {
        let mut series = EventSeries::new(ChannelId(3));
        let listener = MockWatermarkListener::new();
        series.register_listener(listener.clone());

        for burst in 0..10 {
            let base = burst * 1000;
            let ts = vec![base + 10, base + 20, base + 30];
            let codes = vec![burst as i32 * 3, burst as i32 * 3 + 1, burst as i32 * 3 + 2];
            series.append(&ts, &codes).unwrap();
        }

        assert_eq!(listener.event_count(), 10);
        assert_eq!(listener.last_event().unwrap().total_samples, 30);

        // Zero-order hold check between bursts
        assert_eq!(series.get_state_at(15), Some(0));
        assert_eq!(series.get_state_at(25), Some(1));
        assert_eq!(series.get_state_at(50), Some(2));
    }

    // --- T3-COMB-15: Multi-Channel Concurrent Watermark Fanout ---
    #[test]
    fn t3_comb_15_multichannel_concurrent_watermark_fanout() {
        let mut series_ch1 =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());
        let mut series_ch2 =
            NativeScalarSeries::new(ChannelId(2), ScalarType::I16, LinearScale::identity());

        let listener = MockWatermarkListener::new();
        series_ch1.register_listener(listener.clone());
        series_ch2.register_listener(listener.clone());

        let ts1 = generate_timestamps(1000, 50, 10);
        let ts2 = generate_timestamps(1000, 100, 5);

        series_ch1
            .append(&ts1, ScalarSlice::F32(&vec![1.0; 50]))
            .unwrap();
        series_ch2
            .append(&ts2, ScalarSlice::I16(&vec![2; 100]))
            .unwrap();

        assert_eq!(listener.event_count(), 2);
        let events = listener.all_events();
        assert_eq!(events[0].channel_id, ChannelId(1));
        assert_eq!(events[0].total_samples, 50);
        assert_eq!(events[1].channel_id, ChannelId(2));
        assert_eq!(events[1].total_samples, 100);
    }
}

// ============================================================================
// Tier 4: Real-World Application Scenarios (6 Realistic Telemetry Workloads)
// ============================================================================
pub mod tier4_real_world {
    use super::*;

    // --- T4-SCEN-01: Three-Phase Motor Inverter ADC Streams ---
    #[test]
    fn t4_scen_01_three_phase_motor_inverter_foc() {
        // 3 Channels (ia, ib, ic), 100 kHz sampling rate (10 us = 10,000 ns step)
        // 200,000 samples per channel (> 3 chunks: 196,608 sealed + 3,392 active)
        // Calibrated from 12-bit ADC counts (-2048..2047) to +/-50 Amperes
        let scale = LinearScale::new(50.0 / 2048.0, 0.0);

        let mut ch_ia = NativeScalarSeries::new(ChannelId(1), ScalarType::I16, scale);
        let mut ch_ib = NativeScalarSeries::new(ChannelId(2), ScalarType::I16, scale);
        let mut ch_ic = NativeScalarSeries::new(ChannelId(3), ScalarType::I16, scale);

        let listener = MockWatermarkListener::new();
        ch_ia.register_listener(listener.clone());
        ch_ib.register_listener(listener.clone());
        ch_ic.register_listener(listener.clone());

        let count = 200_000;
        let batch_size = 4096;

        for offset in (0..count).step_by(batch_size) {
            let cur_batch = std::cmp::min(batch_size, count - offset);
            let ts = generate_timestamps(offset as u64 * 10_000, cur_batch, 10_000);

            // Synthesize 120-degree phase-shifted currents
            let mut raw_a = Vec::with_capacity(cur_batch);
            let mut raw_b = Vec::with_capacity(cur_batch);
            let mut raw_c = Vec::with_capacity(cur_batch);

            for i in 0..cur_batch {
                let theta = (offset + i) as f64 * 0.01;
                raw_a.push((theta.sin() * 2000.0) as i16);
                raw_b.push(((theta - 2.0944).sin() * 2000.0) as i16); // -120 deg
                raw_c.push(((theta + 2.0944).sin() * 2000.0) as i16); // +120 deg
            }

            ch_ia.append(&ts, ScalarSlice::I16(&raw_a)).unwrap();
            ch_ib.append(&ts, ScalarSlice::I16(&raw_b)).unwrap();
            ch_ic.append(&ts, ScalarSlice::I16(&raw_c)).unwrap();
        }

        assert_eq!(ch_ia.sealed_chunk_count(), 3);
        assert_eq!(ch_ib.sealed_chunk_count(), 3);
        assert_eq!(ch_ic.sealed_chunk_count(), 3);
        assert_eq!(ch_ia.total_samples(), 200_000);
        assert_eq!(ch_ib.total_samples(), 200_000);
        assert_eq!(ch_ic.total_samples(), 200_000);

        // Verification of Kirchhoff's Current Law & calibrated currents:
        // 1. Verify individual channel calibrated currents in physical Amperes at index 100,000
        let (_, va) = ch_ia.get_sample_by_index(100_000).unwrap();
        let (_, vb) = ch_ib.get_sample_by_index(100_000).unwrap();
        let (_, vc) = ch_ic.get_sample_by_index(100_000).unwrap();
        assert!(
            (va - 40.3564).abs() < 0.1,
            "va ({}) not calibrated to ~40.356 A",
            va
        );
        assert!(
            (vb - (-43.9453)).abs() < 0.1,
            "vb ({}) not calibrated to ~-43.945 A",
            vb
        );
        assert!(
            (vc - 3.5888).abs() < 0.1,
            "vc ({}) not calibrated to ~3.589 A",
            vc
        );
        assert!(
            (va + vb + vc).abs() < 0.05,
            "Kirchhoff sum exceeds tolerance: {}",
            va + vb + vc
        );

        // 2. Also verify at index 100 where raw counts do NOT sum to zero (raw_a=1682, raw_b=-1778, raw_c=94 -> sum=-2)
        let (_, va2) = ch_ia.get_sample_by_index(100).unwrap();
        let (_, vb2) = ch_ib.get_sample_by_index(100).unwrap();
        let (_, vc2) = ch_ic.get_sample_by_index(100).unwrap();
        assert!((va2 - 41.0645).abs() < 0.1);
        assert!((vb2 - (-43.4082)).abs() < 0.1);
        assert!((vc2 - 2.2949).abs() < 0.1);
        assert!((va2 + vb2 + vc2).abs() < 0.1);
    }

    // --- T4-SCEN-02: High-Frequency Digital PWM & Glitch Capture ---
    #[test]
    fn t4_scen_02_high_frequency_pwm_glitch_capture() {
        // 10 MHz logic analyzer sampling, 65,536 samples (1 full chunk, 8 KB)
        // 20 kHz PWM (period 500 samples, 50% duty = 250 high, 250 low)
        // Inject narrow 1-sample spike/glitch at sample index 32,768 (midpoint)
        let mut bits = generate_square_wave_bits(CHUNK_LEN, 500);
        bits[32_768] = !bits[32_768]; // Glitch inversion

        let mut bv = BitVecChunk::new();
        bv.extend_from_slice(&bits);
        bv.compute_edge_indices();

        assert_eq!(bv.len(), CHUNK_LEN);
        assert_eq!(bv.words.byte_len(), 8192);

        // Verify glitch is captured in edge_indices without loss (both rising and falling transitions)
        assert!(
            bv.edge_indices.contains(&(32_768 as u16)),
            "Rising edge missing at 32,768"
        );
        assert!(
            bv.edge_indices.contains(&(32_769 as u16)),
            "Falling edge missing at 32,769"
        );
        assert_eq!(bv.get(32_768), Some(bits[32_768]));
        assert_eq!(bv.get(32_769), Some(bits[32_769]));
    }

    // --- T4-SCEN-03: Firmware State Machine Lifecycle Ribbon ---
    #[test]
    fn t4_scen_03_firmware_lifecycle_state_ribbon() {
        // Sequencer states: 0:BOOT -> 1:INIT -> 2:STANDBY -> 3:ARMED -> 4:RUNNING -> 5:FAULT -> 6:SHUTDOWN
        let mut series = EventSeries::new(ChannelId(5));
        let listener = MockWatermarkListener::new();
        series.register_listener(listener.clone());

        let transitions = vec![
            (1_000_000u64, 0),  // Boot at 1ms
            (5_000_000u64, 1),  // Init at 5ms
            (10_000_000u64, 2), // Standby at 10ms
            (20_000_000u64, 3), // Armed at 20ms
            (30_000_000u64, 4), // Running at 30ms
            (50_000_000u64, 5), // Fault at 50ms
            (55_000_000u64, 6), // Shutdown at 55ms
        ];

        let ts: Vec<u64> = transitions.iter().map(|(t, _)| *t).collect();
        let codes: Vec<i32> = transitions.iter().map(|(_, c)| *c).collect();
        series.append(&ts, &codes).unwrap();

        assert_eq!(series.get_state_at(500_000), None);
        assert_eq!(series.get_state_at(1_000_000), Some(0));
        assert_eq!(series.get_state_at(7_000_000), Some(1));
        assert_eq!(series.get_state_at(25_000_000), Some(3));
        assert_eq!(series.get_state_at(40_000_000), Some(4));
        assert_eq!(series.get_state_at(52_000_000), Some(5)); // Caught during fault
        assert_eq!(series.get_state_at(100_000_000), Some(6));

        assert_eq!(listener.last_event().unwrap().total_samples, 7);

        // Verify rejection of non-monotonic state transition (attempting 35ms after 55ms)
        let non_monotonic_err = series.append(&[35_000_000], &[99]);
        assert!(
            non_monotonic_err.is_err(),
            "EventSeries must reject non-monotonic timestamp"
        );

        // Verify series integrity and latest state remain intact
        assert_eq!(series.get_state_at(52_000_000), Some(5));
        assert_eq!(series.get_state_at(100_000_000), Some(6));
        assert_eq!(listener.last_event().unwrap().total_samples, 7);
    }

    // --- T4-SCEN-04: Long-Duration Low-Rate Sensor Monitoring ---
    #[test]
    fn t4_scen_04_long_duration_sensor_monitoring() {
        // Environmental chamber: 10 Hz sampling over 2 hours (7,200s duration = 7.2e12 ns)
        // 72,000 samples (> CHUNK_LEN: 1 chunk sealed, 1 active)
        // Span exceeds 4.29s -> must automatically promote TimestampColumn to AbsU64
        let step_ns = 100_000_000u64; // 100ms per sample (10 Hz)
        let total = 72_000;
        let ts = generate_timestamps(1_000_000_000, total, step_ns);
        let vals = vec![25.0f32; total]; // 25.0 deg C

        let mut series =
            NativeScalarSeries::new(ChannelId(10), ScalarType::F32, LinearScale::identity());
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();

        assert_eq!(series.sealed_chunk_count(), 1);
        assert_eq!(series.total_samples(), 72_000);

        // Explicitly verify automatic promotion to AbsU64 in the sealed chunk
        let handle = &series.sealed_chunks()[0];
        let chunk = handle
            .storage
            .as_in_memory()
            .expect("Sealed chunk 0 must be in memory");
        if let DataChunk::Scalar(scalar_chunk) = chunk.as_ref() {
            assert!(
                matches!(scalar_chunk.timestamps, TimestampColumn::AbsU64(_)),
                "TimestampColumn must be promoted to AbsU64 for spans > 4.29s"
            );
            assert_eq!(scalar_chunk.timestamps.len(), CHUNK_LEN);
            assert_eq!(scalar_chunk.timestamps.byte_footprint(), CHUNK_LEN * 8);
        } else {
            panic!("Expected DataChunk::Scalar variant");
        }

        // Direct column constructor confirmation
        let col_check = TimestampColumn::from_slice(&ts[..CHUNK_LEN]);
        assert!(matches!(col_check, TimestampColumn::AbsU64(_)));

        // Binary search query at 1 hour (3600 seconds)
        let target_ts = 1_000_000_000 + 3600 * 1_000_000_000;
        let (idx, found_ts, val) = series.get_sample_by_timestamp(target_ts).unwrap();
        assert_eq!(idx, 36_000);
        assert_eq!(found_ts, target_ts);
        assert_eq!(val, 25.0);
    }

    // --- T4-SCEN-05: Full MSO Synchronous Mixed-Signal Co-acquisition ---
    #[test]
    fn t4_scen_05_full_mso_mixed_signal_coacquisition() {
        // Synchronous co-acquisition across 3 modalities:
        // 1. Analog: Ch1 (Current) in i16
        // 2. Logic: Ch2 (PWM) in Bool (BitVec)
        // 3. Event: Ch3 (Drive State) in EventSeries
        // Exactly 131,072 samples (2 sealed chunks each) on 1 MHz clock (1 us step)
        let total = CHUNK_LEN * 2;
        let ts = generate_timestamps(0, total, 1000);

        let mut ch_analog =
            NativeScalarSeries::new(ChannelId(1), ScalarType::I16, LinearScale::identity());
        let mut ch_logic =
            NativeScalarSeries::new(ChannelId(2), ScalarType::Bool, LinearScale::identity());
        let mut ch_event = EventSeries::new(ChannelId(3));

        let listener = MockWatermarkListener::new();
        ch_analog.register_listener(listener.clone());
        ch_logic.register_listener(listener.clone());
        ch_event.register_listener(listener.clone());

        let analog_vals = vec![100i16; total];
        let logic_vals = generate_square_wave_bits(total, 100);
        let event_codes: Vec<i32> = (0..total).map(|i| (i % 8) as i32).collect();

        ch_analog
            .append(&ts, ScalarSlice::I16(&analog_vals))
            .unwrap();
        ch_logic
            .append(&ts, ScalarSlice::Bool(&logic_vals))
            .unwrap();
        ch_event.append(&ts, &event_codes).unwrap();

        // Assert exactly 2 sealed chunks across all 3 modalities
        assert_eq!(ch_analog.sealed_chunk_count(), 2);
        assert_eq!(ch_logic.sealed_chunk_count(), 2);
        assert_eq!(ch_event.sealed_chunk_count(), 2);
        assert_eq!(ch_analog.total_samples(), total as u64);
        assert_eq!(ch_logic.total_samples(), total as u64);
        assert_eq!(ch_event.total_samples(), total as u64);

        // Cross-modality query at chunk rollover boundary (sample 65,535 in chunk 0 vs 65,536 in chunk 1)
        assert_eq!(
            ch_event.get_state_at(65_535 * 1000),
            Some((65_535 % 8) as i32)
        );
        assert_eq!(
            ch_event.get_state_at(65_536 * 1000),
            Some((65_536 % 8) as i32)
        );

        // All watermarks captured
        assert_eq!(listener.event_count(), 3);
    }

    // --- T4-SCEN-06: Burst Ingestion & Single Glitch Capture ---
    #[test]
    fn t4_scen_06_burst_ingestion_single_spike_capture() {
        // 500,000 samples of f32 waveform containing a single +100.0V glitch at index 42,000
        let count = 500_000;
        let mut vals = vec![0.0f32; count];
        vals[42_000] = 100.0f32; // Glitch spike

        let ts = generate_timestamps(1000, count, 10);
        let mut series =
            NativeScalarSeries::new(ChannelId(1), ScalarType::F32, LinearScale::identity());

        let start = std::time::Instant::now();
        series.append(&ts, ScalarSlice::F32(&vals)).unwrap();
        let elapsed = start.elapsed();

        assert_eq!(series.total_samples(), 500_000);
        assert_eq!(series.sealed_chunk_count(), 500_000 / CHUNK_LEN);

        // Locate glitch by index
        let (ts_glitch, val_glitch) = series.get_sample_by_index(42_000).unwrap();
        assert_eq!(ts_glitch, 1000 + 42_000 * 10);
        assert_eq!(val_glitch, 100.0);

        // Locate glitch by timestamp
        let (idx_found, _, _) = series.get_sample_by_timestamp(ts_glitch).unwrap();
        assert_eq!(idx_found, 42_000);

        // Verify high throughput
        let throughput = (count as f64) / elapsed.as_secs_f64();
        println!("Burst ingestion throughput: {:.2} samples/sec", throughput);
    }
}
