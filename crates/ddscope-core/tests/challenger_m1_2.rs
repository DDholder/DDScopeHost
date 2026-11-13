//! Adversarial test harness for Milestone 1:
//! - NativeScalarColumn (all 11 types, extreme/boundary values, conversion accuracy, type mismatch matrix)
//! - ScalarDataChunk (length mismatch, 65,536 capacity boundary, inverted time range, calibration)
//! - EventDataChunk (non-decreasing timestamps, simultaneous events, binary search before/at/between/after, stress vs oracle)

use ddscope_core::types::{
    AlignedVec, CoreError, EventDataChunk, LinearScale, NativeScalarColumn, ScalarDataChunk,
    ScalarSlice, ScalarType, ScalarValue, TimestampColumn, TimestampNs, CHUNK_LEN,
};

// =========================================================================
// 1. NativeScalarColumn Extreme Values & Conversion Accuracy
// =========================================================================

#[test]
fn test_native_scalar_column_boundary_values_all_types() {
    // 1. I8
    let mut col_i8 = NativeScalarColumn::with_capacity(ScalarType::I8, 5);
    let i8_data = [i8::MIN, -1, 0, 1, i8::MAX];
    assert_eq!(col_i8.append_slice(ScalarSlice::I8(&i8_data)).unwrap(), 5);
    assert_eq!(col_i8.len(), 5);
    for (i, &expected) in i8_data.iter().enumerate() {
        assert_eq!(col_i8.get_as_f64(i), Some(expected as f64));
        assert_eq!(col_i8.get_scalar_value(i), Some(ScalarValue::I8(expected)));
    }
    assert_eq!(col_i8.get_as_f64(5), None);
    assert_eq!(col_i8.get_scalar_value(5), None);

    // 2. I16
    let mut col_i16 = NativeScalarColumn::with_capacity(ScalarType::I16, 5);
    let i16_data = [i16::MIN, -1, 0, 1, i16::MAX];
    assert_eq!(col_i16.append_slice(ScalarSlice::I16(&i16_data)).unwrap(), 5);
    for (i, &expected) in i16_data.iter().enumerate() {
        assert_eq!(col_i16.get_as_f64(i), Some(expected as f64));
        assert_eq!(col_i16.get_scalar_value(i), Some(ScalarValue::I16(expected)));
    }

    // 3. I32
    let mut col_i32 = NativeScalarColumn::with_capacity(ScalarType::I32, 5);
    let i32_data = [i32::MIN, -1, 0, 1, i32::MAX];
    assert_eq!(col_i32.append_slice(ScalarSlice::I32(&i32_data)).unwrap(), 5);
    for (i, &expected) in i32_data.iter().enumerate() {
        assert_eq!(col_i32.get_as_f64(i), Some(expected as f64));
        assert_eq!(col_i32.get_scalar_value(i), Some(ScalarValue::I32(expected)));
    }

    // 4. I64 (Extreme values & precision boundary check)
    let mut col_i64 = NativeScalarColumn::with_capacity(ScalarType::I64, 8);
    let max_safe_int: i64 = 9007199254740991; // (1 << 53) - 1
    let i64_data = [
        i64::MIN,
        -max_safe_int,
        -1,
        0,
        1,
        max_safe_int,
        i64::MAX - 1,
        i64::MAX,
    ];
    assert_eq!(col_i64.append_slice(ScalarSlice::I64(&i64_data)).unwrap(), 8);
    for (i, &expected) in i64_data.iter().enumerate() {
        assert_eq!(col_i64.get_as_f64(i), Some(expected as f64));
        // Native representation must preserve exact 64-bit integer bit-for-bit
        assert_eq!(col_i64.get_scalar_value(i), Some(ScalarValue::I64(expected)));
    }

    // 5. U8
    let mut col_u8 = NativeScalarColumn::with_capacity(ScalarType::U8, 4);
    let u8_data = [0u8, 1, 128, u8::MAX];
    assert_eq!(col_u8.append_slice(ScalarSlice::U8(&u8_data)).unwrap(), 4);
    for (i, &expected) in u8_data.iter().enumerate() {
        assert_eq!(col_u8.get_as_f64(i), Some(expected as f64));
        assert_eq!(col_u8.get_scalar_value(i), Some(ScalarValue::U8(expected)));
    }

    // 6. U16
    let mut col_u16 = NativeScalarColumn::with_capacity(ScalarType::U16, 4);
    let u16_data = [0u16, 1, 32768, u16::MAX];
    assert_eq!(col_u16.append_slice(ScalarSlice::U16(&u16_data)).unwrap(), 4);
    for (i, &expected) in u16_data.iter().enumerate() {
        assert_eq!(col_u16.get_as_f64(i), Some(expected as f64));
        assert_eq!(col_u16.get_scalar_value(i), Some(ScalarValue::U16(expected)));
    }

    // 7. U32
    let mut col_u32 = NativeScalarColumn::with_capacity(ScalarType::U32, 4);
    let u32_data = [0u32, 1, 2147483648, u32::MAX];
    assert_eq!(col_u32.append_slice(ScalarSlice::U32(&u32_data)).unwrap(), 4);
    for (i, &expected) in u32_data.iter().enumerate() {
        assert_eq!(col_u32.get_as_f64(i), Some(expected as f64));
        assert_eq!(col_u32.get_scalar_value(i), Some(ScalarValue::U32(expected)));
    }

    // 8. U64
    let mut col_u64 = NativeScalarColumn::with_capacity(ScalarType::U64, 6);
    let max_safe_uint: u64 = 9007199254740991; // (1 << 53) - 1
    let u64_data = [0u64, 1, max_safe_uint, max_safe_uint + 1, u64::MAX - 1, u64::MAX];
    assert_eq!(col_u64.append_slice(ScalarSlice::U64(&u64_data)).unwrap(), 6);
    for (i, &expected) in u64_data.iter().enumerate() {
        assert_eq!(col_u64.get_as_f64(i), Some(expected as f64));
        assert_eq!(col_u64.get_scalar_value(i), Some(ScalarValue::U64(expected)));
    }

    // 9. F32 (Subnormals, Inf, -Inf, NaN, -0.0, Min/Max)
    let mut col_f32 = NativeScalarColumn::with_capacity(ScalarType::F32, 10);
    let subnormal_f32 = f32::from_bits(1); // Smallest positive subnormal f32
    let f32_data = [
        f32::MIN,
        -1.0,
        -0.0,
        0.0,
        subnormal_f32,
        f32::MIN_POSITIVE,
        1.0,
        f32::MAX,
        f32::NEG_INFINITY,
        f32::INFINITY,
    ];
    assert_eq!(col_f32.append_slice(ScalarSlice::F32(&f32_data)).unwrap(), 10);
    // Add NaN
    assert_eq!(col_f32.append_slice(ScalarSlice::F32(&[f32::NAN])).unwrap(), 1);
    assert_eq!(col_f32.len(), 11);

    for (i, &expected) in f32_data.iter().enumerate() {
        let val_f64 = col_f32.get_as_f64(i).unwrap();
        assert_eq!(val_f64, expected as f64);
        if let Some(ScalarValue::F32(v)) = col_f32.get_scalar_value(i) {
            assert_eq!(v.to_bits(), expected.to_bits());
        } else {
            panic!("Expected ScalarValue::F32 at index {i}");
        }
    }
    // Check NaN at index 10
    assert!(col_f32.get_as_f64(10).unwrap().is_nan());
    if let Some(ScalarValue::F32(v)) = col_f32.get_scalar_value(10) {
        assert!(v.is_nan());
    } else {
        panic!("Expected ScalarValue::F32(NaN)");
    }

    // 10. F64 (Subnormals, Inf, -Inf, NaN, -0.0, Min/Max)
    let mut col_f64 = NativeScalarColumn::with_capacity(ScalarType::F64, 10);
    let subnormal_f64 = f64::from_bits(1); // Smallest positive subnormal f64
    let f64_data = [
        f64::MIN,
        -1.0,
        -0.0,
        0.0,
        subnormal_f64,
        f64::MIN_POSITIVE,
        1.0,
        f64::MAX,
        f64::NEG_INFINITY,
        f64::INFINITY,
    ];
    assert_eq!(col_f64.append_slice(ScalarSlice::F64(&f64_data)).unwrap(), 10);
    assert_eq!(col_f64.append_slice(ScalarSlice::F64(&[f64::NAN])).unwrap(), 1);

    for (i, &expected) in f64_data.iter().enumerate() {
        let val_f64 = col_f64.get_as_f64(i).unwrap();
        assert_eq!(val_f64.to_bits(), expected.to_bits());
        assert_eq!(col_f64.get_scalar_value(i), Some(ScalarValue::F64(expected)));
    }
    assert!(col_f64.get_as_f64(10).unwrap().is_nan());
    if let Some(ScalarValue::F64(v)) = col_f64.get_scalar_value(10) {
        assert!(v.is_nan());
    } else {
        panic!("Expected ScalarValue::F64(NaN)");
    }

    // 11. Bool
    let mut col_bool = NativeScalarColumn::with_capacity(ScalarType::Bool, 3);
    assert_eq!(col_bool.append_slice(ScalarSlice::Bool(&[true, false, true])).unwrap(), 3);
    assert_eq!(col_bool.get_as_f64(0), Some(1.0));
    assert_eq!(col_bool.get_as_f64(1), Some(0.0));
    assert_eq!(col_bool.get_as_f64(2), Some(1.0));
    assert_eq!(col_bool.get_as_f64(3), None);
    assert_eq!(col_bool.get_scalar_value(0), Some(ScalarValue::Bool(true)));
    assert_eq!(col_bool.get_scalar_value(1), Some(ScalarValue::Bool(false)));
    assert_eq!(col_bool.get_scalar_value(2), Some(ScalarValue::Bool(true)));
    assert_eq!(col_bool.get_scalar_value(3), None);
}

// =========================================================================
// 2. NativeScalarColumn 11x11 Full Type Mismatch Matrix
// =========================================================================

#[test]
fn test_native_scalar_column_full_type_mismatch_matrix() {
    let all_types = [
        ScalarType::Bool,
        ScalarType::U8,
        ScalarType::U16,
        ScalarType::U32,
        ScalarType::U64,
        ScalarType::I8,
        ScalarType::I16,
        ScalarType::I32,
        ScalarType::I64,
        ScalarType::F32,
        ScalarType::F64,
    ];

    let bool_data = [true];
    let u8_data = [42u8];
    let u16_data = [42u16];
    let u32_data = [42u32];
    let u64_data = [42u64];
    let i8_data = [42i8];
    let i16_data = [42i16];
    let i32_data = [42i32];
    let i64_data = [42i64];
    let f32_data = [42.0f32];
    let f64_data = [42.0f64];

    #[allow(clippy::too_many_arguments)]
    fn make_slice<'a>(
        ty: ScalarType,
        bool_data: &'a [bool],
        u8_data: &'a [u8],
        u16_data: &'a [u16],
        u32_data: &'a [u32],
        u64_data: &'a [u64],
        i8_data: &'a [i8],
        i16_data: &'a [i16],
        i32_data: &'a [i32],
        i64_data: &'a [i64],
        f32_data: &'a [f32],
        f64_data: &'a [f64],
    ) -> ScalarSlice<'a> {
        match ty {
            ScalarType::Bool => ScalarSlice::Bool(bool_data),
            ScalarType::U8 => ScalarSlice::U8(u8_data),
            ScalarType::U16 => ScalarSlice::U16(u16_data),
            ScalarType::U32 => ScalarSlice::U32(u32_data),
            ScalarType::U64 => ScalarSlice::U64(u64_data),
            ScalarType::I8 => ScalarSlice::I8(i8_data),
            ScalarType::I16 => ScalarSlice::I16(i16_data),
            ScalarType::I32 => ScalarSlice::I32(i32_data),
            ScalarType::I64 => ScalarSlice::I64(i64_data),
            ScalarType::F32 => ScalarSlice::F32(f32_data),
            ScalarType::F64 => ScalarSlice::F64(f64_data),
        }
    }

    let mut matches_passed = 0;
    let mut mismatches_rejected = 0;

    for &col_ty in &all_types {
        for &slice_ty in &all_types {
            let mut col = NativeScalarColumn::with_capacity(col_ty, 2);
            let slice = make_slice(
                slice_ty,
                &bool_data,
                &u8_data,
                &u16_data,
                &u32_data,
                &u64_data,
                &i8_data,
                &i16_data,
                &i32_data,
                &i64_data,
                &f32_data,
                &f64_data,
            );

            if col_ty == slice_ty {
                let res = col.append_slice(slice);
                assert!(res.is_ok(), "Expected Ok for matching type {col_ty:?}");
                assert_eq!(res.unwrap(), 1);
                assert_eq!(col.len(), 1);
                matches_passed += 1;
            } else {
                let res = col.append_slice(slice);
                assert!(res.is_err(), "Expected CoreError::Storage for {col_ty:?} vs {slice_ty:?}");
                match res {
                    Err(CoreError::Storage(msg)) => {
                        assert!(
                            msg.contains("mismatch"),
                            "Error message should mention mismatch: {msg}"
                        );
                    }
                    Err(other) => panic!("Expected CoreError::Storage, got {other:?}"),
                    Ok(_) => unreachable!(),
                }
                assert_eq!(col.len(), 0, "Failed append should not mutate column");
                mismatches_rejected += 1;
            }
        }
    }

    assert_eq!(matches_passed, 11, "Must match exactly 11 self-pairs");
    assert_eq!(mismatches_rejected, 110, "Must reject exactly 110 mismatched pairs (11*10)");
}

// =========================================================================
// 3. ScalarDataChunk Length Mismatch & Capacity Boundary (65536 vs 65537)
// =========================================================================

#[test]
fn test_scalar_data_chunk_length_mismatch() {
    let base: TimestampNs = 1_000_000;
    let ts_5 = TimestampColumn::from_timestamps(base, &[base, base + 1, base + 2, base + 3, base + 4]);
    let mut val_4 = NativeScalarColumn::with_capacity(ScalarType::F32, 4);
    val_4.append_slice(ScalarSlice::F32(&[1.0, 2.0, 3.0, 4.0])).unwrap();

    // 5 timestamps vs 4 values
    let err = ScalarDataChunk::new(ts_5.clone(), val_4, LinearScale::identity())
        .expect_err("Must reject timestamps.len() != values.len()");
    assert!(matches!(err, CoreError::Storage(_)));
    assert!(err.to_string().contains("length mismatch"));

    // 0 timestamps vs 1 value
    let ts_0 = TimestampColumn::from_timestamps(base, &[]);
    let mut val_1 = NativeScalarColumn::with_capacity(ScalarType::F32, 1);
    val_1.append_slice(ScalarSlice::F32(&[1.0])).unwrap();
    let err2 = ScalarDataChunk::new(ts_0, val_1, LinearScale::identity())
        .expect_err("Must reject 0 timestamps vs 1 value");
    assert!(err2.to_string().contains("length mismatch"));

    // 5 timestamps vs 0 values
    let val_0 = NativeScalarColumn::with_capacity(ScalarType::F32, 0);
    let err3 = ScalarDataChunk::new(ts_5, val_0, LinearScale::identity())
        .expect_err("Must reject 5 timestamps vs 0 values");
    assert!(err3.to_string().contains("length mismatch"));
}

#[test]
fn test_scalar_data_chunk_capacity_boundary_65536_and_65537() {
    let base: TimestampNs = 1_000_000_000;
    
    // Boundary Test A: Exactly 65,536 samples (CHUNK_LEN) -> Must Succeed
    let ts_exact: Vec<TimestampNs> = (0..CHUNK_LEN as u64).map(|i| base + i * 10).collect();
    let ts_col_exact = TimestampColumn::from_timestamps(base, &ts_exact);
    let mut val_col_exact = NativeScalarColumn::with_capacity(ScalarType::U16, CHUNK_LEN);
    let u16_data: Vec<u16> = (0..CHUNK_LEN).map(|i| (i % 65536) as u16).collect();
    val_col_exact.append_slice(ScalarSlice::U16(&u16_data)).unwrap();

    let chunk_exact = ScalarDataChunk::new(ts_col_exact, val_col_exact, LinearScale::identity())
        .expect("ScalarDataChunk with exactly 65,536 samples must succeed");
    assert_eq!(chunk_exact.len(), CHUNK_LEN);
    assert_eq!(chunk_exact.time_range_ns(), Some((base, base + (CHUNK_LEN as u64 - 1) * 10)));
    assert_eq!(chunk_exact.get_sample_as_f64(0), Some((base, 0.0)));
    assert_eq!(
        chunk_exact.get_sample_as_f64(CHUNK_LEN - 1),
        Some((base + (CHUNK_LEN as u64 - 1) * 10, (CHUNK_LEN - 1) as f64))
    );
    assert_eq!(chunk_exact.get_sample_as_f64(CHUNK_LEN), None);

    // Boundary Test B: Exactly 65,537 samples (CHUNK_LEN + 1) -> Must Fail with CoreError::Storage
    let count_over = CHUNK_LEN + 1;
    let ts_over: Vec<TimestampNs> = (0..count_over as u64).map(|i| base + i * 10).collect();
    let ts_col_over = TimestampColumn::from_timestamps(base, &ts_over);
    let mut val_col_over = NativeScalarColumn::with_capacity(ScalarType::U16, count_over);
    let u16_data_over: Vec<u16> = (0..count_over).map(|i| (i % 65536) as u16).collect();
    val_col_over.append_slice(ScalarSlice::U16(&u16_data_over)).unwrap();

    let err_over = ScalarDataChunk::new(ts_col_over, val_col_over, LinearScale::identity())
        .expect_err("ScalarDataChunk with 65,537 samples must be rejected");
    match err_over {
        CoreError::Storage(msg) => {
            assert!(
                msg.contains("exceeds CHUNK_LEN"),
                "Error must state that chunk length exceeds CHUNK_LEN: {msg}"
            );
        }
        other => panic!("Expected CoreError::Storage, got {other:?}"),
    }
}

// =========================================================================
// 4. ScalarDataChunk Monotonicity, Inverted Time Range, and Calibration
// =========================================================================

#[test]
fn test_scalar_data_chunk_inverted_time_range_and_monotonicity() {
    // 1. Inverted time range: first_ns > last_ns
    let mut ts_inverted_vec = AlignedVec::with_capacity(3);
    ts_inverted_vec.extend_from_slice(&[5_000u64, 4_000, 3_000]);
    let ts_inverted = TimestampColumn::AbsU64(ts_inverted_vec);

    let mut vals = NativeScalarColumn::with_capacity(ScalarType::F32, 3);
    vals.append_slice(ScalarSlice::F32(&[1.0, 2.0, 3.0])).unwrap();

    let err = ScalarDataChunk::new(ts_inverted, vals, LinearScale::identity())
        .expect_err("Must reject first_ns > last_ns");
    assert!(matches!(err, CoreError::Storage(_)));
    assert!(err.to_string().contains("non-monotonic timestamps"));

    // 2. Simultaneous timestamps: first_ns == last_ns (allowed)
    let mut ts_sim_vec = AlignedVec::with_capacity(2);
    ts_sim_vec.extend_from_slice(&[10_000u64, 10_000]);
    let ts_sim = TimestampColumn::AbsU64(ts_sim_vec);

    let mut vals_sim = NativeScalarColumn::with_capacity(ScalarType::F32, 2);
    vals_sim.append_slice(ScalarSlice::F32(&[10.0, 20.0])).unwrap();

    let chunk_sim = ScalarDataChunk::new(ts_sim, vals_sim, LinearScale::identity())
        .expect("Simultaneous timestamps (first == last) are valid");
    assert_eq!(chunk_sim.len(), 2);
    assert_eq!(chunk_sim.time_range_ns(), Some((10_000, 10_000)));

    // 3. Empty ScalarDataChunk
    let ts_empty = TimestampColumn::from_slice(&[]);
    let vals_empty = NativeScalarColumn::with_capacity(ScalarType::F32, 0);
    let chunk_empty = ScalarDataChunk::new(ts_empty, vals_empty, LinearScale::identity())
        .expect("Empty ScalarDataChunk is valid");
    assert_eq!(chunk_empty.len(), 0);
    assert!(chunk_empty.is_empty());
    assert_eq!(chunk_empty.time_range_ns(), None);
    assert_eq!(chunk_empty.get_sample_as_f64(0), None);
}

#[test]
fn test_scalar_data_chunk_linear_calibration_accuracy() {
    let base: TimestampNs = 500_000;
    let ts_col = TimestampColumn::from_timestamps(base, &[base, base + 100, base + 200, base + 300]);

    // Raw samples: [0, 10, 100, 255] as U8
    let mut val_col = NativeScalarColumn::with_capacity(ScalarType::U8, 4);
    val_col.append_slice(ScalarSlice::U8(&[0, 10, 100, 255])).unwrap();

    // Calibration: y = raw * (-0.05) + 12.5
    let scale = LinearScale::new(-0.05, 12.5);
    let chunk = ScalarDataChunk::new(ts_col, val_col, scale).unwrap();

    assert_eq!(chunk.get_sample_as_f64(0), Some((base, 0.0 * (-0.05) + 12.5)));
    assert_eq!(chunk.get_sample_as_f64(1), Some((base + 100, 10.0 * (-0.05) + 12.5)));
    assert_eq!(chunk.get_sample_as_f64(2), Some((base + 200, 100.0 * (-0.05) + 12.5)));
    assert_eq!(chunk.get_sample_as_f64(3), Some((base + 300, 255.0 * (-0.05) + 12.5)));
    assert_eq!(chunk.get_sample_as_f64(4), None);

    // Test with Float NaN and Infinity values
    let ts_float = TimestampColumn::from_timestamps(base, &[base, base + 10, base + 20]);
    let mut val_float = NativeScalarColumn::with_capacity(ScalarType::F64, 3);
    val_float.append_slice(ScalarSlice::F64(&[0.0, f64::INFINITY, f64::NAN])).unwrap();

    let scale_mul = LinearScale::new(2.0, 5.0);
    let chunk_float = ScalarDataChunk::new(ts_float, val_float, scale_mul).unwrap();
    assert_eq!(chunk_float.get_sample_as_f64(0), Some((base, 5.0)));
    assert_eq!(chunk_float.get_sample_as_f64(1), Some((base + 10, f64::INFINITY)));
    let nan_sample = chunk_float.get_sample_as_f64(2).unwrap();
    assert_eq!(nan_sample.0, base + 20);
    assert!(nan_sample.1.is_nan());
}

// =========================================================================
// 5. EventDataChunk Non-Decreasing Timestamps, Simultaneous Events, Binary Search
// =========================================================================

#[test]
fn test_event_data_chunk_non_decreasing_timestamps() {
    let mut chunk = EventDataChunk::new_state_transitions(4);
    chunk.append_transition(100, 1).unwrap();
    chunk.append_transition(200, 2).unwrap();

    // Backward timestamp: 199 < 200 -> must return CoreError::Storage
    let err = chunk.append_transition(199, 999).expect_err("Must reject decreasing timestamp");
    assert!(matches!(err, CoreError::Storage(_)));
    assert!(err.to_string().contains("non-monotonic event timestamp"));

    // State integrity check: rejected append did not insert partial data
    assert_eq!(chunk.len(), 2);
    assert_eq!(chunk.get_transition(0), Some((100, 1)));
    assert_eq!(chunk.get_transition(1), Some((200, 2)));
    assert_eq!(chunk.get_transition(2), None);

    // Appending equal timestamp is allowed
    chunk.append_transition(200, 3).unwrap();
    assert_eq!(chunk.len(), 3);
}

#[test]
fn test_event_data_chunk_simultaneous_events_latest_state() {
    let mut chunk = EventDataChunk::new_state_transitions(8);

    // Sequence of transitions with multiple simultaneous timestamps:
    // t=100: code=10
    // t=200: code=20, then code=21, then code=22 (3 simultaneous transitions at 200)
    // t=300: code=30, then code=31
    chunk.append_transition(100, 10).unwrap();
    chunk.append_transition(200, 20).unwrap();
    chunk.append_transition(200, 21).unwrap();
    chunk.append_transition(200, 22).unwrap();
    chunk.append_transition(300, 30).unwrap();
    chunk.append_transition(300, 31).unwrap();

    assert_eq!(chunk.len(), 6);

    // Exactly at t=200, the latest state is 22
    assert_eq!(chunk.get_state_at(200), Some(22));
    // Between 200 and 300 (e.g. t=250), state is still 22
    assert_eq!(chunk.get_state_at(250), Some(22));
    // Exactly at t=300, the latest state is 31
    assert_eq!(chunk.get_state_at(300), Some(31));
    // After last event (e.g. t=500), state is 31
    assert_eq!(chunk.get_state_at(500), Some(31));
}

#[test]
fn test_event_data_chunk_binary_search_before_at_between_after() {
    let mut chunk = EventDataChunk::new_state_transitions(5);
    let events = [
        (1000u64, 10i32),
        (2000u64, 20i32),
        (3000u64, 30i32),
        (4000u64, 40i32),
    ];
    for &(ts, code) in &events {
        chunk.append_transition(ts, code).unwrap();
    }

    // 1. Before first event (< 1000) -> returns None
    assert_eq!(chunk.get_state_at(0), None);
    assert_eq!(chunk.get_state_at(500), None);
    assert_eq!(chunk.get_state_at(999), None);

    // 2. Exactly at event timestamps -> returns state at event
    assert_eq!(chunk.get_state_at(1000), Some(10));
    assert_eq!(chunk.get_state_at(2000), Some(20));
    assert_eq!(chunk.get_state_at(3000), Some(30));
    assert_eq!(chunk.get_state_at(4000), Some(40));

    // 3. Between events (t_i < t < t_{i+1}) -> returns state at t_i
    assert_eq!(chunk.get_state_at(1001), Some(10));
    assert_eq!(chunk.get_state_at(1500), Some(10));
    assert_eq!(chunk.get_state_at(1999), Some(10));

    assert_eq!(chunk.get_state_at(2001), Some(20));
    assert_eq!(chunk.get_state_at(2999), Some(20));

    assert_eq!(chunk.get_state_at(3001), Some(30));
    assert_eq!(chunk.get_state_at(3999), Some(30));

    // 4. After last event (> 4000) -> returns state at last event (40)
    assert_eq!(chunk.get_state_at(4001), Some(40));
    assert_eq!(chunk.get_state_at(100_000), Some(40));
    assert_eq!(chunk.get_state_at(u64::MAX), Some(40));

    // 5. Empty chunk edge case
    let empty_chunk = EventDataChunk::new_state_transitions(0);
    assert_eq!(empty_chunk.get_state_at(0), None);
    assert_eq!(empty_chunk.get_state_at(1000), None);
    assert_eq!(empty_chunk.get_state_at(u64::MAX), None);

    // 6. Single event chunk edge case
    let mut single_chunk = EventDataChunk::new_state_transitions(1);
    single_chunk.append_transition(500, 77).unwrap();
    assert_eq!(single_chunk.get_state_at(499), None);
    assert_eq!(single_chunk.get_state_at(500), Some(77));
    assert_eq!(single_chunk.get_state_at(501), Some(77));
    assert_eq!(single_chunk.get_state_at(u64::MAX), Some(77));
}

// =========================================================================
// 6. EventDataChunk Stress Test: 5,000 Transitions vs Ground Truth Oracle
// =========================================================================

#[test]
fn test_event_data_chunk_stress_random_queries_against_oracle() {
    let mut chunk = EventDataChunk::new_state_transitions(5000);
    let mut oracle_history: Vec<(TimestampNs, i32)> = Vec::with_capacity(5000);

    let mut lcg: u64 = 0xDEAD_BEEF_CAFE_BABE;
    let mut cur_ts: TimestampNs = 1_000_000;

    for i in 0..5000 {
        lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1);
        let delta = (lcg >> 48) % 50; // delta in 0..49 (testing simultaneous timestamps when delta == 0)
        cur_ts += delta;
        let code = i % 256;

        chunk.append_transition(cur_ts, code).unwrap();
        oracle_history.push((cur_ts, code));
    }

    assert_eq!(chunk.len(), 5000);

    // Reference linear scan oracle
    let oracle_lookup = |query_ts: TimestampNs| -> Option<i32> {
        let mut last_state = None;
        for &(ts, code) in &oracle_history {
            if ts <= query_ts {
                last_state = Some(code);
            } else {
                break;
            }
        }
        last_state
    };

    // Query 20,000 random points across the timeline:
    let min_ts = oracle_history.first().unwrap().0;
    let max_ts = oracle_history.last().unwrap().0;

    // Queries before min_ts
    assert_eq!(chunk.get_state_at(min_ts - 100), oracle_lookup(min_ts - 100));
    assert_eq!(chunk.get_state_at(0), oracle_lookup(0));

    // Queries after max_ts
    assert_eq!(chunk.get_state_at(max_ts + 100), oracle_lookup(max_ts + 100));
    assert_eq!(chunk.get_state_at(u64::MAX), oracle_lookup(u64::MAX));

    // 20,000 Pseudo-random query timestamps spanning [min_ts - 100, max_ts + 100]
    let span = (max_ts + 200) - (min_ts - 100);
    for _ in 0..20_000 {
        lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1);
        let q_ts = (min_ts - 100) + (lcg % span);

        let actual = chunk.get_state_at(q_ts);
        let expected = oracle_lookup(q_ts);
        assert_eq!(
            actual, expected,
            "Binary search get_state_at({q_ts}) mismatch: got {actual:?}, expected {expected:?}"
        );
    }
}


// =========================================================================
// 7. Additional Adversarial Edge Cases
// =========================================================================

#[test]
fn test_native_scalar_column_alignment_preservation_under_reallocation() {
    // Repeatedly reallocate across cache-line boundaries and verify 64-byte alignment
    let mut col_f64 = NativeScalarColumn::with_capacity(ScalarType::F64, 1);
    for batch in 0..100 {
        let chunk: Vec<f64> = (0..64).map(|i| (batch * 64 + i) as f64).collect();
        col_f64.append_slice(ScalarSlice::F64(&chunk)).unwrap();
    }
    assert_eq!(col_f64.len(), 6400);

    if let NativeScalarColumn::F64(vec) = &col_f64 {
        let ptr_addr = vec.as_ptr() as usize;
        assert_eq!(
            ptr_addr % ddscope_core::types::CACHE_LINE_BYTES,
            0,
            "Backing pointer must remain 64-byte aligned after repeated reallocations"
        );
    } else {
        panic!("Expected F64 column");
    }
}

#[test]
fn test_native_scalar_column_clear_and_reuse_all_types() {
    let all_types = [
        ScalarType::Bool,
        ScalarType::U8,
        ScalarType::U16,
        ScalarType::U32,
        ScalarType::U64,
        ScalarType::I8,
        ScalarType::I16,
        ScalarType::I32,
        ScalarType::I64,
        ScalarType::F32,
        ScalarType::F64,
    ];

    for &ty in &all_types {
        let mut col = NativeScalarColumn::with_capacity(ty, 8);
        assert_eq!(col.len(), 0);
        assert!(col.is_empty());

        // First append
        match &mut col {
            NativeScalarColumn::Bool(c) => { c.push(true); }
            NativeScalarColumn::U8(c) => { c.push(1); }
            NativeScalarColumn::U16(c) => { c.push(1); }
            NativeScalarColumn::U32(c) => { c.push(1); }
            NativeScalarColumn::U64(c) => { c.push(1); }
            NativeScalarColumn::I8(c) => { c.push(1); }
            NativeScalarColumn::I16(c) => { c.push(1); }
            NativeScalarColumn::I32(c) => { c.push(1); }
            NativeScalarColumn::I64(c) => { c.push(1); }
            NativeScalarColumn::F32(c) => { c.push(1.0); }
            NativeScalarColumn::F64(c) => { c.push(1.0); }
        }
        assert_eq!(col.len(), 1);

        // Clear
        col.clear();
        assert_eq!(col.len(), 0);
        assert!(col.is_empty());
        assert_eq!(col.get_as_f64(0), None);
        assert_eq!(col.get_scalar_value(0), None);

        // Re-append works cleanly
        match &mut col {
            NativeScalarColumn::Bool(c) => { c.push(false); }
            NativeScalarColumn::U8(c) => { c.push(99); }
            NativeScalarColumn::U16(c) => { c.push(99); }
            NativeScalarColumn::U32(c) => { c.push(99); }
            NativeScalarColumn::U64(c) => { c.push(99); }
            NativeScalarColumn::I8(c) => { c.push(99); }
            NativeScalarColumn::I16(c) => { c.push(99); }
            NativeScalarColumn::I32(c) => { c.push(99); }
            NativeScalarColumn::I64(c) => { c.push(99); }
            NativeScalarColumn::F32(c) => { c.push(99.0); }
            NativeScalarColumn::F64(c) => { c.push(99.0); }
        }
        assert_eq!(col.len(), 1);
        if ty == ScalarType::Bool {
            assert_eq!(col.get_as_f64(0), Some(0.0));
        } else {
            assert_eq!(col.get_as_f64(0), Some(99.0));
        }
    }
}

#[test]
fn test_native_scalar_column_empty_slice_behavior() {
    let mut col_u8 = NativeScalarColumn::with_capacity(ScalarType::U8, 4);

    // Empty slice matching type
    assert_eq!(col_u8.append_slice(ScalarSlice::U8(&[])).unwrap(), 0);
    assert_eq!(col_u8.len(), 0);

    // Empty slice with mismatched type:
    // By implementation design in chunk.rs, slice.len() == 0 returns early with Ok(0).
    // Verify empirical behavior:
    let res = col_u8.append_slice(ScalarSlice::F64(&[]));
    assert_eq!(res.unwrap(), 0);
}

#[test]
fn test_event_data_chunk_text_records_rejection() {
    let mut text_chunk = EventDataChunk::TextRecords {
        timestamps_ns: AlignedVec::new(),
        messages: Vec::new(),
    };

    // Appending numeric transition to TextRecords must be rejected
    let err = text_chunk
        .append_transition(100, 1)
        .expect_err("Cannot append transition to TextRecords");
    assert!(matches!(err, CoreError::Storage(_)));
    assert!(err.to_string().contains("cannot append state transition to TextRecords"));

    // Querying get_state_at on TextRecords returns None
    assert_eq!(text_chunk.get_state_at(100), None);
    assert_eq!(text_chunk.get_transition(0), None);
}

#[test]
fn test_event_data_chunk_simultaneous_boundary_conditions() {
    let mut chunk = EventDataChunk::new_state_transitions(10);

    // Case A: Multiple simultaneous transitions at the VERY START (index 0, 1, 2 all at t=100)
    chunk.append_transition(100, 1).unwrap();
    chunk.append_transition(100, 2).unwrap();
    chunk.append_transition(100, 3).unwrap();

    // Querying before t=100 -> None
    assert_eq!(chunk.get_state_at(99), None);
    // Querying at t=100 -> latest code (3)
    assert_eq!(chunk.get_state_at(100), Some(3));
    // Querying after t=100 -> latest code (3)
    assert_eq!(chunk.get_state_at(150), Some(3));

    // Case B: Multiple simultaneous transitions at the VERY END (t=500)
    chunk.append_transition(500, 10).unwrap();
    chunk.append_transition(500, 20).unwrap();
    chunk.append_transition(500, 30).unwrap();

    assert_eq!(chunk.get_state_at(499), Some(3));
    assert_eq!(chunk.get_state_at(500), Some(30));
    assert_eq!(chunk.get_state_at(501), Some(30));
    assert_eq!(chunk.get_state_at(u64::MAX), Some(30));
}

#[test]
fn test_scalar_data_chunk_single_sample_and_zero_scale() {
    let base: TimestampNs = 42;
    let ts_single = TimestampColumn::from_timestamps(base, &[base]);
    let mut val_single = NativeScalarColumn::with_capacity(ScalarType::I32, 1);
    val_single.append_slice(ScalarSlice::I32(&[12345])).unwrap();

    // Zero scale (y = 0 * raw + 7.5 = 7.5)
    let scale_zero = LinearScale::new(0.0, 7.5);
    let chunk = ScalarDataChunk::new(ts_single, val_single, scale_zero).unwrap();

    assert_eq!(chunk.len(), 1);
    assert_eq!(chunk.time_range_ns(), Some((base, base)));
    assert_eq!(chunk.get_sample_as_f64(0), Some((base, 7.5)));
    assert_eq!(chunk.get_sample_as_f64(1), None);
}
