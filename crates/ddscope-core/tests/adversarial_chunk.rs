//! Adversarial Stress Test Suite for Milestone 1: BitVecChunk & TimestampColumn
//!
//! Stress tests:
//! 1. BitVecChunk:
//!    - Word boundary traversal on single-bit push (bits 63, 64, 65, 127, 128, etc.)
//!    - Uneven slice extension (1, 63, 64, 65, 127, 1024, 65536 bits)
//!    - Multi-part uneven extensions summing to exactly CHUNK_LEN (65,536)
//!    - Interleaved push and extend_from_slice operations
//!    - Repeated 1-bit extend_from_slice vs push equivalence
//!    - digital_init_level across empty, single-true, single-false, and multi-word chunks
//!    - Edge index extraction across adversarial patterns:
//!      - All-zeros (0 edges)
//!      - All-ones (0 edges intra-chunk, 1 edge if prev_level=false)
//!      - Alternating 010101 and 101010
//!      - Bursty pulses (solitary 1 in 1000 zeros, solitary 0 in 1000 ones)
//!      - Transitions precisely crossing 64-bit word boundaries
//!      - Final chunk boundary transitions (bit 65535)
//!      - Out-of-chunk range handling (> 65536 samples without panic)
//!      - Large pseudo-random sequences vs naive ground-truth oracle across multiple seeds
//! 2. TimestampColumn:
//!    - Exact boundary at MAX_REL_U32_NS (4,294,967,295 ns) -> RelU32
//!    - Overflow at MAX_REL_U32_NS + 1 -> AbsU64
//!    - Out-of-order timestamp (< base) -> AbsU64
//!    - Dynamic push upgrading RelU32 to AbsU64 on span overflow (10,000 samples)
//!    - Dynamic push upgrading RelU32 to AbsU64 on t < base
//!    - Extreme base timestamps (near 0, near u64::MAX)
//!    - Lifecycle clear and reuse
//! 3. ScalarDataChunk:
//!    - Strict length invariant enforcement
//!    - CHUNK_LEN (65,536) upper bound enforcement
//!    - Non-monotonic timestamp rejection (first_ns > last_ns)
//!    - Calibrated physical unit conversion precision

use ddscope_core::types::chunk::{
    BitVecChunk, NativeScalarColumn, ScalarDataChunk, TimestampColumn, CHUNK_LEN,
    MAX_REL_U32_NS,
};
use ddscope_core::types::scalar::{LinearScale, ScalarSlice, ScalarType};

// ============================================================================
// Naive Ground Truth Oracle for BitVec Transitions
// ============================================================================

fn naive_ground_truth_edges(bits: &[bool], prev_level: Option<bool>) -> Vec<u16> {
    let mut edges = Vec::new();
    if bits.is_empty() {
        return edges;
    }

    if let Some(prev) = prev_level {
        if bits[0] != prev {
            edges.push(0);
        }
    }

    for k in 1..bits.len() {
        if bits[k] != bits[k - 1] {
            if let Ok(idx) = u16::try_from(k) {
                edges.push(idx);
            }
        }
    }
    edges
}

// ============================================================================
// 1. BitVecChunk Adversarial Tests
// ============================================================================

#[test]
fn test_bitvec_single_bit_push_word_boundaries() {
    let mut bv = BitVecChunk::new();
    let mut expected = Vec::new();

    // Push 256 individual bits across multiple 64-bit boundaries
    for i in 0..256 {
        let bit = (i % 3 == 0) || (i == 63) || (i == 64) || (i == 127) || (i == 128);
        bv.push(bit);
        expected.push(bit);

        assert_eq!(bv.len(), i + 1);
        assert_eq!(bv.get(i), Some(bit), "bit mismatch at immediate index {}", i);
        let expected_words = (i + 1).div_ceil(64);
        assert_eq!(bv.words.len(), expected_words);
    }

    for (i, &exp) in expected.iter().enumerate() {
        assert_eq!(bv.get(i), Some(exp), "bit mismatch at replay index {}", i);
    }
    assert_eq!(bv.get(256), None);
}

#[test]
fn test_bitvec_uneven_extend_from_slice() {
    let slice_lengths = [1, 2, 7, 31, 63, 64, 65, 127, 128, 129, 255, 256, 1024];

    for &len in &slice_lengths {
        let mut bv = BitVecChunk::new();
        let slice: Vec<bool> = (0..len).map(|i| (i % 2) == 1).collect();
        bv.extend_from_slice(&slice);

        assert_eq!(bv.len(), len, "Length mismatch for slice_len={}", len);
        assert_eq!(bv.words.len(), len.div_ceil(64));

        for (i, &expected_val) in slice.iter().enumerate() {
            assert_eq!(
                bv.get(i),
                Some(expected_val),
                "Value mismatch at {} for slice_len={}",
                i,
                len
            );
        }
        assert_eq!(bv.get(len), None);
    }
}

#[test]
fn test_bitvec_exact_chunk_len_uneven_composition() {
    // Uneven parts: 1 + 63 + 64 + 65 + 127 + 1024 + (65536 - 1344) = 65,536 bits
    let part_lens = [1, 63, 64, 65, 127, 1024, CHUNK_LEN - 1344];
    assert_eq!(part_lens.iter().sum::<usize>(), CHUNK_LEN);

    let mut bv = BitVecChunk::with_capacity(CHUNK_LEN);
    let mut all_bits = Vec::with_capacity(CHUNK_LEN);

    let mut lcg: u64 = 0xCAFE_BABE_1234_5678;
    for &len in &part_lens {
        let mut part = Vec::with_capacity(len);
        for _ in 0..len {
            lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1);
            part.push((lcg >> 63) != 0);
        }
        bv.extend_from_slice(&part);
        all_bits.extend_from_slice(&part);
    }

    assert_eq!(bv.len(), CHUNK_LEN);
    assert_eq!(bv.words.len(), 1024); // 65,536 / 64 = 1,024 words

    for (i, &expected_bit) in all_bits.iter().enumerate() {
        assert_eq!(bv.get(i), Some(expected_bit));
    }
    assert_eq!(bv.get(CHUNK_LEN), None);

    bv.compute_edge_indices();
    let oracle = naive_ground_truth_edges(&all_bits, None);
    assert_eq!(bv.edge_indices, oracle);
}

#[test]
fn test_bitvec_interleaved_push_and_extend() {
    let mut bv = BitVecChunk::new();
    let mut ground_truth = Vec::new();

    let chunks: &[&[bool]] = &[
        &[true, false, true],
        &[false; 61],
        &[true],
        &[true, true, false, true, false],
        &[false; 57],
        &[true],
        &[false; 65],
    ];

    for (step, slice) in chunks.iter().enumerate() {
        bv.extend_from_slice(slice);
        ground_truth.extend_from_slice(slice);

        let extra_bit = (step % 2) == 0;
        bv.push(extra_bit);
        ground_truth.push(extra_bit);

        assert_eq!(bv.len(), ground_truth.len());
        for (idx, &expected_bit) in ground_truth.iter().enumerate() {
            assert_eq!(
                bv.get(idx),
                Some(expected_bit),
                "Mismatch at idx={} after step={}",
                idx,
                step
            );
        }
    }

    bv.compute_edge_indices();
    let oracle_edges = naive_ground_truth_edges(&ground_truth, None);
    assert_eq!(bv.edge_indices, oracle_edges);
}

#[test]
fn test_bitvec_repeated_single_bit_extend_vs_push() {
    let mut bv_push = BitVecChunk::new();
    let mut bv_extend = BitVecChunk::new();

    let test_bits = [true, false, false, true, true, true, false, true];
    for &b in &test_bits {
        bv_push.push(b);
        bv_extend.extend_from_slice(&[b]);
    }

    assert_eq!(bv_push.len(), bv_extend.len());
    assert_eq!(bv_push.words.as_slice(), bv_extend.words.as_slice());
}

#[test]
fn test_bitvec_digital_init_level() {
    // Empty chunk -> 0
    let bv_empty = BitVecChunk::new();
    assert_eq!(bv_empty.digital_init_level(), 0);

    // First bit false -> 0
    let mut bv_false = BitVecChunk::new();
    bv_false.push(false);
    bv_false.push(true);
    assert_eq!(bv_false.digital_init_level(), 0);

    // First bit true -> 1
    let mut bv_true = BitVecChunk::new();
    bv_true.push(true);
    bv_true.push(false);
    assert_eq!(bv_true.digital_init_level(), 1);
}

#[test]
fn test_bitvec_edge_extraction_all_zeros() {
    for &size in &[1, 2, 63, 64, 65, 127, 128, 129, 1024, CHUNK_LEN] {
        let mut bv = BitVecChunk::new();
        let zeros = vec![false; size];
        bv.extend_from_slice(&zeros);

        bv.compute_edge_indices();
        assert!(bv.edge_indices.is_empty(), "all zeros should have 0 edges (size={})", size);

        bv.compute_edge_indices_with_init(Some(false));
        assert!(bv.edge_indices.is_empty(), "all zeros with init false should have 0 edges (size={})", size);

        bv.compute_edge_indices_with_init(Some(true));
        assert_eq!(
            bv.edge_indices,
            vec![0],
            "all zeros with init true must have edge at 0 (size={})",
            size
        );
    }
}

#[test]
fn test_bitvec_edge_extraction_all_ones() {
    for &size in &[1, 2, 63, 64, 65, 127, 128, 129, 1024, CHUNK_LEN] {
        let mut bv = BitVecChunk::new();
        let ones = vec![true; size];
        bv.extend_from_slice(&ones);

        bv.compute_edge_indices();
        assert!(bv.edge_indices.is_empty(), "all ones should have 0 edges (size={})", size);

        bv.compute_edge_indices_with_init(Some(true));
        assert!(bv.edge_indices.is_empty(), "all ones with init true should have 0 edges (size={})", size);

        bv.compute_edge_indices_with_init(Some(false));
        assert_eq!(
            bv.edge_indices,
            vec![0],
            "all ones with init false must have edge at 0 (size={})",
            size
        );
    }
}

#[test]
fn test_bitvec_edge_extraction_alternating_patterns() {
    for &size in &[2, 3, 63, 64, 65, 127, 128, 129, 500] {
        let pattern_01: Vec<bool> = (0..size).map(|i| (i % 2) == 1).collect();
        let mut bv01 = BitVecChunk::new();
        bv01.extend_from_slice(&pattern_01);
        bv01.compute_edge_indices();

        let expected_01: Vec<u16> = (1..size as u16).collect();
        assert_eq!(bv01.edge_indices, expected_01, "0101 failed for size={}", size);

        let pattern_10: Vec<bool> = (0..size).map(|i| (i % 2) == 0).collect();
        let mut bv10 = BitVecChunk::new();
        bv10.extend_from_slice(&pattern_10);
        bv10.compute_edge_indices();

        let expected_10: Vec<u16> = (1..size as u16).collect();
        assert_eq!(bv10.edge_indices, expected_10, "1010 failed for size={}", size);

        bv10.compute_edge_indices_with_init(Some(false));
        let mut expected_with_init = vec![0];
        expected_with_init.extend(1..size as u16);
        assert_eq!(bv10.edge_indices, expected_with_init);
    }
}

#[test]
fn test_bitvec_edge_extraction_bursty_pulses() {
    let mut bits = vec![false; 2000];
    bits[1000] = true;
    let mut bv = BitVecChunk::new();
    bv.extend_from_slice(&bits);
    bv.compute_edge_indices();
    assert_eq!(bv.edge_indices, vec![1000, 1001]);

    let mut bits_ones = vec![true; 2000];
    bits_ones[1000] = false;
    let mut bv_ones = BitVecChunk::new();
    bv_ones.extend_from_slice(&bits_ones);
    bv_ones.compute_edge_indices();
    assert_eq!(bv_ones.edge_indices, vec![1000, 1001]);

    let mut sparse_bits = vec![false; 4096];
    sparse_bits[63] = true;
    sparse_bits[64] = true;
    sparse_bits[127] = true;
    sparse_bits[2048] = true;
    let mut bv_sparse = BitVecChunk::new();
    bv_sparse.extend_from_slice(&sparse_bits);
    bv_sparse.compute_edge_indices();

    let oracle = naive_ground_truth_edges(&sparse_bits, None);
    assert_eq!(bv_sparse.edge_indices, oracle);
}

#[test]
fn test_bitvec_word_border_transitions() {
    let mut bits = vec![false; 130];
    bits[63] = true;
    bits[64] = false;
    bits[65] = true;
    bits[66] = false;

    let mut bv = BitVecChunk::new();
    bv.extend_from_slice(&bits);
    bv.compute_edge_indices();

    let oracle = naive_ground_truth_edges(&bits, None);
    assert_eq!(bv.edge_indices, oracle);
    assert_eq!(bv.edge_indices, vec![63, 64, 65, 66]);

    let mut inv_bits = vec![true; 130];
    inv_bits[63] = false;
    inv_bits[64] = true;
    inv_bits[65] = false;

    let mut bv_inv = BitVecChunk::new();
    bv_inv.extend_from_slice(&inv_bits);
    bv_inv.compute_edge_indices();
    let oracle_inv = naive_ground_truth_edges(&inv_bits, None);
    assert_eq!(bv_inv.edge_indices, oracle_inv);
}

#[test]
fn test_bitvec_chunk_boundary_transition_at_65535() {
    let mut bits = vec![false; CHUNK_LEN];
    bits[65535] = true;

    let mut bv = BitVecChunk::new();
    bv.extend_from_slice(&bits);
    assert_eq!(bv.len(), CHUNK_LEN);

    bv.compute_edge_indices();
    assert_eq!(bv.edge_indices, vec![65535]);

    bits[65534] = true;
    let mut bv2 = BitVecChunk::new();
    bv2.extend_from_slice(&bits);
    bv2.compute_edge_indices();
    assert_eq!(bv2.edge_indices, vec![65534]);
}

#[test]
fn test_bitvec_beyond_chunk_len_safety() {
    let len = 65_538;
    let mut bv = BitVecChunk::with_capacity(len);
    let mut bits = vec![false; len];
    bits[65535] = true;
    bits[65536] = false;
    bv.extend_from_slice(&bits);

    bv.compute_edge_indices();
    assert!(bv.edge_indices.contains(&65535));
    assert_eq!(bv.edge_indices.len(), 1);
}

#[test]
fn test_bitvec_pseudo_random_oracle_equivalence_multiple_seeds() {
    let seeds: [u64; 5] = [
        0x1234_5678_9ABC_DEF0,
        0xDEAD_BEEF_CAFE_BABE,
        0x0123_4567_89AB_CDEF,
        0xFFFF_FFFF_0000_0001,
        0x4242_4242_4242_4242,
    ];

    let test_sizes = [1, 2, 63, 64, 65, 127, 128, 129, 255, 256, 1023, 1024, CHUNK_LEN];

    for &seed in &seeds {
        let mut lcg = seed;
        for &size in &test_sizes {
            let mut bits = Vec::with_capacity(size);
            for _ in 0..size {
                lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1);
                bits.push((lcg >> 63) != 0);
            }

            let mut bv = BitVecChunk::with_capacity(size);
            bv.extend_from_slice(&bits);

            // 1. None init
            bv.compute_edge_indices();
            let oracle_none = naive_ground_truth_edges(&bits, None);
            assert_eq!(
                bv.edge_indices, oracle_none,
                "Oracle mismatch (init=None, seed={:#X}, size={})",
                seed, size
            );

            // 2. Some(false) init
            bv.compute_edge_indices_with_init(Some(false));
            let oracle_false = naive_ground_truth_edges(&bits, Some(false));
            assert_eq!(
                bv.edge_indices, oracle_false,
                "Oracle mismatch (init=Some(false), seed={:#X}, size={})",
                seed, size
            );

            // 3. Some(true) init
            bv.compute_edge_indices_with_init(Some(true));
            let oracle_true = naive_ground_truth_edges(&bits, Some(true));
            assert_eq!(
                bv.edge_indices, oracle_true,
                "Oracle mismatch (init=Some(true), seed={:#X}, size={})",
                seed, size
            );
        }
    }
}

// ============================================================================
// 2. TimestampColumn Adversarial Tests
// ============================================================================

#[test]
fn test_timestamp_exact_max_rel_u32_boundary() {
    let base: u64 = 1_000_000_000;

    let ts_exact = vec![base, base + MAX_REL_U32_NS];
    let col = TimestampColumn::from_timestamps(base, &ts_exact);

    match &col {
        TimestampColumn::RelU32 { base_ts_ns, rel_ns } => {
            assert_eq!(*base_ts_ns, base);
            assert_eq!(rel_ns.len(), 2);
            assert_eq!(rel_ns[0], 0);
            assert_eq!(rel_ns[1], u32::MAX);
        }
        TimestampColumn::AbsU64(_) => panic!("Expected RelU32 at exact MAX_REL_U32_NS boundary"),
    }
    assert_eq!(col.get_ns(0), Some(base));
    assert_eq!(col.get_ns(1), Some(base + MAX_REL_U32_NS));
    assert_eq!(col.get_ns(2), None);
}

#[test]
fn test_timestamp_overflow_max_rel_u32_plus_one() {
    let base: u64 = 1_000_000_000;

    let ts_overflow = vec![base, base + MAX_REL_U32_NS + 1];
    let col = TimestampColumn::from_timestamps(base, &ts_overflow);

    match &col {
        TimestampColumn::AbsU64(abs) => {
            assert_eq!(abs.len(), 2);
            assert_eq!(abs[0], base);
            assert_eq!(abs[1], base + MAX_REL_U32_NS + 1);
        }
        TimestampColumn::RelU32 { .. } => {
            panic!("Expected AbsU64 when span exceeds MAX_REL_U32_NS by 1 ns");
        }
    }
    assert_eq!(col.get_ns(0), Some(base));
    assert_eq!(col.get_ns(1), Some(base + MAX_REL_U32_NS + 1));
}

#[test]
fn test_timestamp_out_of_order_base() {
    let base: u64 = 5_000_000_000;

    let ts_before = vec![base - 1, base, base + 1_000];
    let col = TimestampColumn::from_timestamps(base, &ts_before);

    match &col {
        TimestampColumn::AbsU64(abs) => {
            assert_eq!(abs.len(), 3);
            assert_eq!(abs[0], base - 1);
            assert_eq!(abs[1], base);
            assert_eq!(abs[2], base + 1_000);
        }
        TimestampColumn::RelU32 { .. } => {
            panic!("Expected AbsU64 when timestamp precedes base");
        }
    }
}

#[test]
fn test_timestamp_dynamic_push_upgrade_on_overflow_large_dataset() {
    let base: u64 = 2_000_000_000;
    let count = 10_000;
    let step_ns = 400_000; // 10,000 * 400,000 = 4,000,000,000 ns (< 4.29s)

    let mut col = TimestampColumn::from_timestamps(base, &[]);
    for i in 0..count {
        col.push(base + (i as u64) * step_ns);
    }
    assert!(matches!(col, TimestampColumn::RelU32 { .. }));
    assert_eq!(col.len(), count);

    // Push right at upper boundary
    col.push(base + MAX_REL_U32_NS);
    assert!(matches!(col, TimestampColumn::RelU32 { .. }));
    assert_eq!(col.len(), count + 1);

    // Push 1 ns beyond limit -> triggers upgrade to AbsU64
    let overflow_ts = base + MAX_REL_U32_NS + 1;
    col.push(overflow_ts);
    assert!(matches!(col, TimestampColumn::AbsU64(_)));
    assert_eq!(col.len(), count + 2);

    // Verify complete data integrity for all 10,002 samples
    for i in 0..count {
        assert_eq!(
            col.get_ns(i),
            Some(base + (i as u64) * step_ns),
            "Corruption at sample {}",
            i
        );
    }
    assert_eq!(col.get_ns(count), Some(base + MAX_REL_U32_NS));
    assert_eq!(col.get_ns(count + 1), Some(overflow_ts));
}

#[test]
fn test_timestamp_dynamic_push_upgrade_on_negative_delta() {
    let base: u64 = 10_000_000_000;
    let mut col = TimestampColumn::from_timestamps(base, &[base, base + 500]);
    assert!(matches!(col, TimestampColumn::RelU32 { .. }));

    col.push(base - 100);
    assert!(matches!(col, TimestampColumn::AbsU64(_)));
    assert_eq!(col.len(), 3);
    assert_eq!(col.get_ns(0), Some(base));
    assert_eq!(col.get_ns(1), Some(base + 500));
    assert_eq!(col.get_ns(2), Some(base - 100));
}

#[test]
fn test_timestamp_extreme_base_values() {
    let col_zero = TimestampColumn::from_timestamps(0, &[0, 100, MAX_REL_U32_NS]);
    assert!(matches!(col_zero, TimestampColumn::RelU32 { .. }));
    assert_eq!(col_zero.get_ns(2), Some(MAX_REL_U32_NS));

    let max_base = u64::MAX - MAX_REL_U32_NS;
    let col_high = TimestampColumn::from_timestamps(max_base, &[max_base, u64::MAX]);
    assert!(matches!(col_high, TimestampColumn::RelU32 { .. }));
    assert_eq!(col_high.get_ns(0), Some(max_base));
    assert_eq!(col_high.get_ns(1), Some(u64::MAX));
}

#[test]
fn test_timestamp_empty_column_behavior() {
    let col_empty = TimestampColumn::from_timestamps(100, &[]);
    assert_eq!(col_empty.len(), 0);
    assert!(col_empty.is_empty());
    assert_eq!(col_empty.first_ns(), None);
    assert_eq!(col_empty.last_ns(), None);
    assert_eq!(col_empty.get_ns(0), None);

    let from_slice_empty = TimestampColumn::from_slice(&[]);
    assert_eq!(from_slice_empty.len(), 0);
    assert!(from_slice_empty.is_empty());
}

#[test]
fn test_timestamp_clear_and_reuse() {
    let base = 500_000;
    let mut col = TimestampColumn::from_timestamps(base, &[base, base + 10]);
    assert_eq!(col.len(), 2);

    col.clear();
    assert_eq!(col.len(), 0);
    assert!(col.is_empty());
    assert_eq!(col.get_ns(0), None);

    col.push(base + 20);
    assert_eq!(col.len(), 1);
    assert_eq!(col.get_ns(0), Some(base + 20));
}

// ============================================================================
// 3. ScalarDataChunk Validation Tests
// ============================================================================

#[test]
fn test_scalar_data_chunk_strict_invariants() {
    let base = 1_000_000;
    let scale = LinearScale::new(2.5, -1.0);

    // 1. Valid chunk
    let ts = TimestampColumn::from_slice(&[base, base + 100]);
    let mut vals = NativeScalarColumn::with_capacity(ScalarType::F64, 2);
    vals.append_slice(ScalarSlice::F64(&[10.0, 20.0])).unwrap();
    let chunk = ScalarDataChunk::new(ts.clone(), vals, scale).unwrap();
    assert_eq!(chunk.len(), 2);
    assert_eq!(chunk.get_sample_as_f64(0), Some((base, 10.0 * 2.5 - 1.0)));
    assert_eq!(chunk.get_sample_as_f64(1), Some((base + 100, 20.0 * 2.5 - 1.0)));

    // 2. Length mismatch
    let mut vals_mismatch = NativeScalarColumn::with_capacity(ScalarType::F64, 3);
    vals_mismatch.append_slice(ScalarSlice::F64(&[10.0, 20.0, 30.0])).unwrap();
    let err_mismatch = ScalarDataChunk::new(ts, vals_mismatch, scale).unwrap_err();
    assert!(err_mismatch.to_string().contains("length mismatch"));

    // 3. Exceeds CHUNK_LEN
    let mut big_ts = TimestampColumn::from_timestamps(0, &[]);
    let mut big_vals = NativeScalarColumn::with_capacity(ScalarType::U8, CHUNK_LEN + 1);
    for _ in 0..=CHUNK_LEN {
        big_ts.push(1);
    }
    big_vals.append_slice(ScalarSlice::U8(&vec![0u8; CHUNK_LEN + 1])).unwrap();
    let err_cap = ScalarDataChunk::new(big_ts, big_vals, scale).unwrap_err();
    assert!(err_cap.to_string().contains("exceeds CHUNK_LEN"));

    // 4. Clock inversion (first > last)
    let bad_ts = TimestampColumn::AbsU64({
        let mut v = ddscope_core::types::AlignedVec::with_capacity(2);
        v.extend_from_slice(&[1000, 500]);
        v
    });
    let mut ok_vals = NativeScalarColumn::with_capacity(ScalarType::U8, 2);
    ok_vals.append_slice(ScalarSlice::U8(&[1, 2])).unwrap();
    let err_mono = ScalarDataChunk::new(bad_ts, ok_vals, scale).unwrap_err();
    assert!(err_mono.to_string().contains("non-monotonic"));
}
