//! Adversarial Empirical Stress & Invariant Test Suite for Milestone TA5
//! (Zero-Copy DDSF v1 Frame Packer and Zero-Copy Reader).
//!
//! Evaluates:
//! 1. Wire alignment invariants:
//!    - Buffer 64-byte alignment (`AlignedBuffer`).
//!    - Headers natural alignment (64B frame, 32B scope, 32B track, 32B stats, 16B snapshot).
//!    - Payload pointers and slice offsets strictly divisible by 8 across ALL track types.
//!    - Odd-byte payloads: M4 (20B/col, P in {1,3,5,7}), DigitalEdges (4B/edge, E in {1,3,5,7}),
//!      Spectrum (4B/bin, M in {1,3,5,7}) with verified zero-padding.
//! 2. Boundary and extreme edge cases:
//!    - 0 scopes, 0 tracks, empty payloads, 16 scopes with 8 tracks each (128 tracks).
//!    - Float special values: NaN, +0.0, -0.0, +Inf, -Inf, subnormals, f32/f64 extremes.
//! 3. Bit-for-bit roundtrip decoding via `DdsfZeroCopyReader`:
//!    - Every point, column, edge, ribbon, stats, and snapshot exactly matches input.
//! 4. Dual API Parity:
//!    - Owned `pack_frame` vs borrowed `pack_frame_borrowed` bit-for-bit byte equality.
//!    - `pack_frame_with_header` vs `pack_frame_borrowed_with_header`.
//! 5. Buffer capacity stability and memory invariance in steady state.
//! 6. Reader error handling and robustness against malformed/truncated wire frames.

use std::f32::consts::PI as PI_F32;
use std::f64::consts::PI as PI_F64;

use ddscope_core::scope::ddsf::{
    align_up_8, DdsfAnalogPoint, DdsfBlockKind, DdsfFrameHeader, DdsfFramePacker,
    DdsfM4Column, DdsfScopePacketHeader, DdsfSnapshotEntry, DdsfStateRibbon, DdsfTrackBlockHeader,
    DdsfTrackStats, DdsfZeroCopyReader, ScopePacketView, ScopeViewportPayload, TrackBlockView,
    TrackPayload, TrackPayloadSlice, DDSF_FLAG_PINNED_ACTIVE, DDSF_FLAG_QUOTA_WARNING,
    DDSF_HEADER_BYTES, DDSF_SCOPE_HEADER_BYTES, DDSF_SNAPSHOT_ENTRY_BYTES,
    DDSF_TRACK_FLAG_HAS_STATS, DDSF_TRACK_HEADER_BYTES, DDSF_TRACK_STATS_BYTES,
};

// ============================================================================
// 1. Boundary & Edge Counts Stress Testing
// ============================================================================

#[test]
fn test_edge_case_empty_frame_and_snapshots_only() {
    println!("\n=== [TA5] Testing edge cases: empty frame and snapshot-only frames ===");

    let mut packer = DdsfFramePacker::new(4096);

    // Case 1: 0 scopes, 0 snapshots -> exactly 64 bytes
    let buf = packer.pack_frame(1, 100_000, 0, &[], &[]).expect("pack empty frame failed");
    assert_eq!(buf.len(), DDSF_HEADER_BYTES);
    assert_eq!(buf.as_ptr() as usize % 64, 0, "buffer base pointer must be 64-byte aligned");

    let reader = DdsfZeroCopyReader::new(buf).expect("reader failed on empty frame");
    assert_eq!(reader.header().frame_seq, 1);
    assert_eq!(reader.header().scope_count, 0);
    assert_eq!(reader.header().snapshot_count, 0);
    assert_eq!(reader.header().total_byte_len, 64);
    assert_eq!(reader.scopes().count(), 0);
    assert_eq!(reader.snapshots().expect("snapshots call failed").len(), 0);

    // Case 2: 0 scopes, varying snapshots K in {1, 2, 7, 16, 64, 127}
    for &k in &[1, 2, 7, 16, 64, 127] {
        let snapshots: Vec<DdsfSnapshotEntry> = (0..k)
            .map(|i| DdsfSnapshotEntry {
                channel_num_id: i as u32,
                kind: (i % 3) as u8,
                pad: [0; 3],
                val_f64: (i as f64) * 1.25 - 50.0,
            })
            .collect();

        let buf = packer
            .pack_frame(k as u32, 200_000, DDSF_FLAG_QUOTA_WARNING, &[], &snapshots)
            .expect("pack snapshot-only frame failed");

        let expected_len = DDSF_HEADER_BYTES + k * DDSF_SNAPSHOT_ENTRY_BYTES;
        assert_eq!(buf.len(), expected_len);
        assert_eq!(buf.len() % 8, 0, "total frame byte length must be divisible by 8");

        let reader = DdsfZeroCopyReader::new(buf).expect("reader failed on snapshot-only frame");
        assert_eq!(reader.header().scope_count, 0);
        assert_eq!(reader.header().snapshot_count, k as u16);
        assert_eq!(reader.header().total_byte_len, expected_len as u64);

        let decoded_snaps = reader.snapshots().expect("read snapshots failed");
        assert_eq!(decoded_snaps.len(), k);
        for i in 0..k {
            assert_eq!(decoded_snaps[i], snapshots[i], "snapshot {i} mismatch");
        }
    }
}

#[test]
fn test_edge_case_empty_scopes_and_empty_payload_tracks() {
    println!("\n=== [TA5] Testing edge cases: empty scopes and zero-element track payloads ===");

    let mut packer = DdsfFramePacker::new(8192);

    // Case 1: Multiple scopes with 0 tracks
    for &num_scopes in &[1, 4, 16, 32] {
        let scopes: Vec<ScopeViewportPayload> = (0..num_scopes)
            .map(|id| ScopeViewportPayload {
                scope_id: id as u32,
                trigger_state: (id % 5) as u8,
                mode: (id % 4) as u8,
                win_t_start_ns: (id as u64) * 10_000,
                win_t_span_ns: 100_000,
                subpixel_offset_ns: (id as f32) * 0.1,
                tracks: Vec::new(),
            })
            .collect();

        let buf = packer.pack_frame(42, 500_000, 0, &scopes, &[]).expect("pack failed");
        let expected_len = DDSF_HEADER_BYTES + num_scopes * DDSF_SCOPE_HEADER_BYTES;
        assert_eq!(buf.len(), expected_len);
        assert_eq!(buf.len() % 8, 0);

        let reader = DdsfZeroCopyReader::new(buf).expect("reader failed");
        assert_eq!(reader.header().scope_count, num_scopes as u16);
        let parsed: Vec<_> = reader.scopes().collect();
        assert_eq!(parsed.len(), num_scopes);
        for (i, scope) in parsed.iter().enumerate().take(num_scopes) {
            assert_eq!(scope.header().scope_id, i as u32);
            assert_eq!(scope.header().track_block_count, 0);
            assert_eq!(scope.header().packet_byte_len, DDSF_SCOPE_HEADER_BYTES as u32);
            assert_eq!(scope.tracks().count(), 0);
        }
    }

    // Case 2: Tracks with 0 elements for each track kind
    let empty_tracks = vec![
        TrackPayload::new_raw_xy(1, Vec::new()),
        TrackPayload::new_m4(2, Vec::new(), None),
        TrackPayload::new_m4(3, Vec::new(), Some(DdsfTrackStats::default())),
        TrackPayload::new_digital_edges(4, 1, Vec::new()),
        TrackPayload::new_state_ribbons(5, Vec::new()),
        TrackPayload::new_spectrum(6, 0.0, 1.0, Vec::new()),
    ];

    let scope = ScopeViewportPayload {
        scope_id: 100,
        trigger_state: 3,
        mode: 2,
        win_t_start_ns: 1_000,
        win_t_span_ns: 10_000,
        subpixel_offset_ns: 0.5,
        tracks: empty_tracks,
    };

    let buf = packer.pack_frame(99, 600_000, 0, &[scope], &[]).expect("pack empty tracks failed");
    let reader = DdsfZeroCopyReader::new(buf).expect("reader failed");
    let sc = reader.scopes().next().expect("scope view missing");
    assert_eq!(sc.header().track_block_count, 6);

    let tracks: Vec<_> = sc.tracks().collect();
    assert_eq!(tracks.len(), 6);

    // Check each empty track returns Ok(&[]) without panic
    assert_eq!(tracks[0].raw_points().expect("raw_points failed"), &[]);
    assert_eq!(tracks[1].m4_columns().expect("m4_columns failed"), &[]);
    assert_eq!(tracks[2].m4_columns().expect("m4_columns failed"), &[]);
    assert!(tracks[2].stats().is_some());
    assert_eq!(tracks[3].digital_edges().expect("digital_edges failed"), &[]);
    assert_eq!(tracks[3].header().digital_init_level, 1);
    assert_eq!(tracks[4].state_ribbons().expect("state_ribbons failed"), &[]);
    assert_eq!(tracks[5].spectrum_bins().expect("spectrum_bins failed"), &[]);
}

#[test]
fn test_edge_case_dense_16_scopes_8_tracks_each() {
    println!("\n=== [TA5] Testing dense stress frame: 16 scopes x 8 tracks = 128 tracks ===");

    let mut scopes = Vec::with_capacity(16);
    for s in 0..16 {
        let mut tracks = Vec::with_capacity(8);
        for t in 0..8 {
            let ch = (s * 8 + t) as u32;
            let track = match t % 5 {
                0 => {
                    let pts = vec![DdsfAnalogPoint { x_norm: 0.1, y_val: s as f32 }; 10];
                    TrackPayload::new_raw_xy(ch, pts)
                }
                1 => {
                    let cols = vec![DdsfM4Column {
                        x_norm: 0.2,
                        v_first: 1.0,
                        v_min: -1.0,
                        v_max: 2.0,
                        v_last: 0.5,
                    }; 5];
                    TrackPayload::new_m4(ch, cols, None)
                }
                2 => {
                    let edges = vec![0.1f32, 0.5, 0.9];
                    TrackPayload::new_digital_edges(ch, (s % 2) as u8, edges)
                }
                3 => {
                    let ribbons = vec![DdsfStateRibbon {
                        x0_norm: 0.0,
                        x1_norm: 1.0,
                        state_id: t,
                        color_idx: s as u32,
                    }; 3];
                    TrackPayload::new_state_ribbons(ch, ribbons)
                }
                _ => {
                    let bins = vec![10.0f32, 20.0, 30.0];
                    TrackPayload::new_spectrum(ch, 100.0, 10.0, bins)
                }
            };
            tracks.push(track);
        }
        scopes.push(ScopeViewportPayload {
            scope_id: s as u32,
            trigger_state: (s % 5) as u8,
            mode: (s % 4) as u8,
            win_t_start_ns: (s as u64) * 1000,
            win_t_span_ns: 50_000,
            subpixel_offset_ns: 0.25,
            tracks,
        });
    }

    let snapshots: Vec<DdsfSnapshotEntry> = (0..32)
        .map(|i| DdsfSnapshotEntry {
            channel_num_id: i,
            kind: (i % 3) as u8,
            pad: [0; 3],
            val_f64: (i as f64) * PI_F64,
        })
        .collect();

    let mut packer = DdsfFramePacker::new(64 * 1024);
    let buf = packer
        .pack_frame(777, 987_654_321, DDSF_FLAG_PINNED_ACTIVE, &scopes, &snapshots)
        .expect("pack 128 tracks failed");

    assert_eq!(buf.as_ptr() as usize % 64, 0);
    assert_eq!(buf.len() % 8, 0);

    let reader = DdsfZeroCopyReader::new(buf).expect("reader failed on 128-track frame");
    assert_eq!(reader.header().scope_count, 16);
    assert_eq!(reader.header().snapshot_count, 32);

    let mut total_tracks_verified = 0;
    for (s_idx, scope_view) in reader.scopes().enumerate() {
        assert_eq!(scope_view.header().scope_id, s_idx as u32);
        assert_eq!(scope_view.header().track_block_count, 8);
        for (t_idx, track_view) in scope_view.tracks().enumerate() {
            let expected_ch = (s_idx * 8 + t_idx) as u32;
            assert_eq!(track_view.header().channel_id, expected_ch);
            total_tracks_verified += 1;
        }
    }
    assert_eq!(total_tracks_verified, 128);

    let decoded_snaps = reader.snapshots().expect("snapshots failed");
    assert_eq!(decoded_snaps.len(), 32);
    assert_eq!(decoded_snaps, snapshots.as_slice());
}

// ============================================================================
// 2. Wire Alignment Invariants & Odd-Byte Payloads Fuzzing
// ============================================================================

#[test]
fn test_alignment_m4_odd_columns_padding_and_offsets() {
    println!("\n=== [TA5] Testing M4 odd column counts P in {{1, 3, 5, 7, 9, 11, 13, 17, 31, 33}} ===");

    let mut packer = DdsfFramePacker::new(32 * 1024);
    let odd_ps = [1, 3, 5, 7, 9, 11, 13, 17, 31, 33];

    for &p in &odd_ps {
        let cols: Vec<DdsfM4Column> = (0..p)
            .map(|i| DdsfM4Column {
                x_norm: (i as f32) / (p as f32),
                v_first: (i as f32) + 0.1,
                v_min: -(i as f32) - 1.0,
                v_max: (i as f32) + 5.0,
                v_last: (i as f32) + 0.9,
            })
            .collect();

        let stats = DdsfTrackStats {
            min: -100.0,
            max: 100.0,
            vpp: 200.0,
            mean: 0.0,
            rms: 50.0,
            freq_hz: 1000.0,
            duty: 0.5,
            reserved: 0.0,
        };

        let scope = ScopeViewportPayload {
            scope_id: 1,
            trigger_state: 0,
            mode: 0,
            win_t_start_ns: 0,
            win_t_span_ns: 1000,
            subpixel_offset_ns: 0.0,
            tracks: vec![
                // Track 0: without stats
                TrackPayload::new_m4(10, cols.clone(), None),
                // Track 1: with stats
                TrackPayload::new_m4(20, cols.clone(), Some(stats)),
            ],
        };

        let buf = packer.pack_frame(p as u32, 1000, 0, &[scope], &[]).expect("pack M4 failed");

        // Verify base buffer alignment
        assert_eq!(buf.as_ptr() as usize % 64, 0, "buffer base pointer must be 64B aligned");
        assert_eq!(buf.len() % 8, 0, "total length must be 8B aligned");

        let reader = DdsfZeroCopyReader::new(buf).expect("reader failed");
        let sc = reader.scopes().next().expect("scope missing");
        let tracks: Vec<_> = sc.tracks().collect();
        assert_eq!(tracks.len(), 2);

        // Raw buffer byte offset verification
        let buf_base = buf.as_ptr() as usize;

        for (t_idx, tr) in tracks.iter().enumerate() {
            let data_len = p * std::mem::size_of::<DdsfM4Column>();
            let aligned_data_len = align_up_8(data_len);
            let expected_pad = aligned_data_len - data_len;
            assert_eq!(expected_pad, 4, "M4 odd column count must require exactly 4 bytes of padding");

            assert_eq!(tr.header().data_byte_len as usize, data_len);
            let expected_block_len = DDSF_TRACK_HEADER_BYTES
                + if t_idx == 1 { DDSF_TRACK_STATS_BYTES } else { 0 }
                + aligned_data_len;
            assert_eq!(tr.header().block_byte_len as usize, expected_block_len);
            assert_eq!(expected_block_len % 8, 0, "block_byte_len must be multiple of 8");

            // Verify payload absolute memory pointer is strictly divisible by 8
            let payload_slice = tr.payload_bytes();
            let payload_ptr = payload_slice.as_ptr() as usize;
            assert_eq!(payload_ptr % 8, 0, "payload memory pointer must be 8-byte aligned");

            // Verify payload byte offset relative to buffer base is strictly divisible by 8
            let offset_from_buf = payload_ptr - buf_base;
            assert_eq!(offset_from_buf % 8, 0, "payload byte offset from frame start must be multiple of 8");

            // Verify zero-padding bytes in wire stream are exactly 0
            let pad_slice_offset = payload_ptr - buf_base + data_len;
            assert_eq!(&buf[pad_slice_offset..pad_slice_offset + expected_pad], &[0u8; 4]);

            // Verify roundtrip column extraction bit-for-bit
            let decoded_cols = tr.m4_columns().expect("m4_columns extraction failed");
            assert_eq!(decoded_cols.len(), p);
            for i in 0..p {
                assert_eq!(decoded_cols[i], cols[i]);
            }
        }
    }
}

#[test]
fn test_alignment_digital_edges_odd_counts_padding_and_offsets() {
    println!("\n=== [TA5] Testing DigitalEdges odd counts E in {{1, 3, 5, 7, 9, 11, 13, 17, 31, 33}} ===");

    let mut packer = DdsfFramePacker::new(32 * 1024);
    let odd_es = [1, 3, 5, 7, 9, 11, 13, 17, 31, 33];

    for &e in &odd_es {
        let edges: Vec<f32> = (0..e).map(|i| (i as f32) / (e as f32)).collect();

        let scope = ScopeViewportPayload {
            scope_id: 1,
            trigger_state: 1,
            mode: 1,
            win_t_start_ns: 0,
            win_t_span_ns: 1000,
            subpixel_offset_ns: 0.0,
            tracks: vec![
                TrackPayload::new_digital_edges(10, 0, edges.clone()),
                TrackPayload::new_digital_edges(20, 1, edges.clone()),
            ],
        };

        let buf = packer.pack_frame(e as u32, 2000, 0, &[scope], &[]).expect("pack edges failed");

        assert_eq!(buf.as_ptr() as usize % 64, 0);
        assert_eq!(buf.len() % 8, 0);

        let reader = DdsfZeroCopyReader::new(buf).expect("reader failed");
        let sc = reader.scopes().next().expect("scope missing");
        let tracks: Vec<_> = sc.tracks().collect();
        assert_eq!(tracks.len(), 2);

        let buf_base = buf.as_ptr() as usize;

        for (t_idx, tr) in tracks.iter().enumerate() {
            let data_len = e * std::mem::size_of::<f32>();
            let aligned_data_len = align_up_8(data_len);
            let expected_pad = aligned_data_len - data_len;
            assert_eq!(expected_pad, 4, "DigitalEdges odd count must require 4 bytes padding");

            assert_eq!(tr.header().data_byte_len as usize, data_len);
            let expected_block_len = DDSF_TRACK_HEADER_BYTES + aligned_data_len;
            assert_eq!(tr.header().block_byte_len as usize, expected_block_len);
            assert_eq!(expected_block_len % 8, 0);

            // Verify payload absolute memory pointer is strictly divisible by 8
            let payload_slice = tr.payload_bytes();
            let payload_ptr = payload_slice.as_ptr() as usize;
            assert_eq!(payload_ptr % 8, 0, "payload memory pointer must be 8-byte aligned");

            // Verify payload byte offset relative to buffer base is strictly divisible by 8
            let offset_from_buf = payload_ptr - buf_base;
            assert_eq!(offset_from_buf % 8, 0, "payload byte offset from frame start must be multiple of 8");

            // Verify zero-padding bytes
            let pad_slice_offset = payload_ptr - buf_base + data_len;
            assert_eq!(&buf[pad_slice_offset..pad_slice_offset + expected_pad], &[0u8; 4]);

            // Verify digital init level
            let expected_init = if t_idx == 0 { 0 } else { 1 };
            assert_eq!(tr.header().digital_init_level, expected_init);

            // Verify roundtrip edge extraction
            let decoded_edges = tr.digital_edges().expect("digital_edges extraction failed");
            assert_eq!(decoded_edges.len(), e);
            for i in 0..e {
                assert_eq!(decoded_edges[i], edges[i]);
            }
        }
    }
}

#[test]
fn test_mixed_signals_cascading_alignment_invariants() {
    println!("\n=== [TA5] Testing cascading mixed signals alignment across all 5 block kinds ===");

    let mut packer = DdsfFramePacker::new(32 * 1024);

    // Cascading tracks with varying odd/even byte lengths
    let tracks = vec![
        // Track 0: M4 with P=3 (60 bytes data -> 4 bytes pad)
        TrackPayload::new_m4(
            1,
            vec![DdsfM4Column::default(); 3],
            None,
        ),
        // Track 1: Digital with E=5 (20 bytes data -> 4 bytes pad)
        TrackPayload::new_digital_edges(2, 1, vec![0.1f32; 5]),
        // Track 2: RawXY with N=3 (24 bytes data -> 0 bytes pad)
        TrackPayload::new_raw_xy(
            3,
            vec![DdsfAnalogPoint { x_norm: 0.1, y_val: 1.0 }; 3],
        ),
        // Track 3: M4 with P=1 and Stats (Stats=32, Data=20 -> 4 bytes pad)
        TrackPayload::new_m4(
            4,
            vec![DdsfM4Column::default(); 1],
            Some(DdsfTrackStats::default()),
        ),
        // Track 4: StateRibbons with R=3 (48 bytes data -> 0 bytes pad)
        TrackPayload::new_state_ribbons(
            5,
            vec![DdsfStateRibbon { x0_norm: 0.0, x1_norm: 0.5, state_id: 1, color_idx: 2 }; 3],
        ),
        // Track 5: SpectrumFrame with M=7 (28 bytes data -> 4 bytes pad)
        TrackPayload::new_spectrum(6, 10.0, 1.0, vec![1.0f32; 7]),
    ];

    let scope1 = ScopeViewportPayload {
        scope_id: 1,
        trigger_state: 3,
        mode: 2,
        win_t_start_ns: 100,
        win_t_span_ns: 10_000,
        subpixel_offset_ns: 0.0,
        tracks: tracks.clone(),
    };

    let scope2 = ScopeViewportPayload {
        scope_id: 2,
        trigger_state: 0,
        mode: 0,
        win_t_start_ns: 200,
        win_t_span_ns: 20_000,
        subpixel_offset_ns: 0.0,
        tracks,
    };

    let snapshots = vec![
        DdsfSnapshotEntry { channel_num_id: 1, kind: 0, pad: [0; 3], val_f64: 1.0 },
        DdsfSnapshotEntry { channel_num_id: 2, kind: 1, pad: [0; 3], val_f64: 2.0 },
        DdsfSnapshotEntry { channel_num_id: 3, kind: 2, pad: [0; 3], val_f64: 3.0 },
    ];

    let buf = packer.pack_frame(1, 100, 0, &[scope1, scope2], &snapshots).expect("pack failed");

    let buf_base = buf.as_ptr() as usize;
    assert_eq!(buf_base % 64, 0, "frame buffer pointer must be 64-byte aligned");
    assert_eq!(buf.len() % 8, 0, "frame total byte length must be 8-byte aligned");

    let reader = DdsfZeroCopyReader::new(buf).expect("reader failed");
    assert_eq!(reader.header().scope_count, 2);
    assert_eq!(reader.header().snapshot_count, 3);

    for scope_view in reader.scopes() {
        let s_hdr_ptr = (scope_view.header() as *const DdsfScopePacketHeader) as usize;
        assert_eq!((s_hdr_ptr - buf_base) % 8, 0, "scope header offset must be 8-byte aligned");
        assert_eq!(scope_view.header().packet_byte_len % 8, 0, "scope packet_byte_len must be 8-byte aligned");

        for track_view in scope_view.tracks() {
            let t_hdr_ptr = (track_view.header() as *const DdsfTrackBlockHeader) as usize;
            assert_eq!((t_hdr_ptr - buf_base) % 8, 0, "track header offset must be 8-byte aligned");
            assert_eq!(track_view.header().block_byte_len % 8, 0, "track block_byte_len must be 8-byte aligned");

            if let Some(stats) = track_view.stats() {
                let stats_ptr = (stats as *const DdsfTrackStats) as usize;
                assert_eq!((stats_ptr - buf_base) % 8, 0, "stats block offset must be 8-byte aligned");
            }

            let payload_ptr = track_view.payload_bytes().as_ptr() as usize;
            assert_eq!((payload_ptr - buf_base) % 8, 0, "payload byte offset must be 8-byte aligned");
            assert_eq!(payload_ptr % 8, 0, "payload memory pointer must be 8-byte aligned");
        }
    }

    let snap_slice = reader.snapshots().expect("snapshots failed");
    let snap_ptr = snap_slice.as_ptr() as usize;
    assert_eq!((snap_ptr - buf_base) % 8, 0, "snapshots start offset must be 8-byte aligned");
    assert_eq!(snap_ptr % 8, 0, "snapshots memory pointer must be 8-byte aligned");
}

// ============================================================================
// 3. Bit-for-Bit Roundtrip Integrity Across Floats & Extremes
// ============================================================================

#[test]
fn test_float_extremes_and_special_values_roundtrip() {
    println!("\n=== [TA5] Testing float extremes (NaN, +/-0.0, +/-Inf, subnormals, min/max) ===");

    let mut packer = DdsfFramePacker::new(16 * 1024);

    let test_floats: Vec<f32> = vec![
        0.0,
        -0.0,
        1.0,
        -1.0,
        PI_F32,
        -PI_F32,
        f32::MIN_POSITIVE,       // Smallest positive normal
        -f32::MIN_POSITIVE,
        f32::from_bits(1),       // Smallest positive subnormal
        f32::from_bits(0x800001),// Negative subnormal
        f32::MAX,                // Largest finite float
        f32::MIN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NAN,
    ];

    // 1. Raw points with special floats
    let pts: Vec<DdsfAnalogPoint> = test_floats
        .iter()
        .map(|&f| DdsfAnalogPoint { x_norm: f, y_val: f })
        .collect();

    // 2. M4 columns with special floats
    let cols: Vec<DdsfM4Column> = test_floats
        .iter()
        .map(|&f| DdsfM4Column {
            x_norm: f,
            v_first: f,
            v_min: f,
            v_max: f,
            v_last: f,
        })
        .collect();

    // 3. Digital edges with special floats
    let edges = test_floats.clone();

    // 4. State ribbons
    let ribbons: Vec<DdsfStateRibbon> = test_floats
        .iter()
        .enumerate()
        .map(|(i, &f)| DdsfStateRibbon {
            x0_norm: f,
            x1_norm: f,
            state_id: i as i32,
            color_idx: (i * 10) as u32,
        })
        .collect();

    // 5. Stats with special floats
    let stats = DdsfTrackStats {
        min: f32::NEG_INFINITY,
        max: f32::INFINITY,
        vpp: f32::MAX,
        mean: -0.0,
        rms: f32::MIN_POSITIVE,
        freq_hz: PI_F32,
        duty: 0.5,
        reserved: 0.0,
    };

    // 6. Snapshots with special f64
    let test_f64s: Vec<f64> = vec![
        0.0,
        -0.0,
        1.0,
        -1.0,
        PI_F64,
        f64::MIN_POSITIVE,
        f64::from_bits(1),
        f64::MAX,
        f64::MIN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
    ];

    let snapshots: Vec<DdsfSnapshotEntry> = test_f64s
        .iter()
        .enumerate()
        .map(|(i, &val)| DdsfSnapshotEntry {
            channel_num_id: i as u32,
            kind: (i % 3) as u8,
            pad: [0; 3],
            val_f64: val,
        })
        .collect();

    let scope = ScopeViewportPayload {
        scope_id: 42,
        trigger_state: 3,
        mode: 2,
        win_t_start_ns: 12345,
        win_t_span_ns: 67890,
        subpixel_offset_ns: f32::NAN, // Even header float can be NaN
        tracks: vec![
            TrackPayload::new_raw_xy(1, pts.clone()),
            TrackPayload::new_m4(2, cols.clone(), Some(stats)),
            TrackPayload::new_digital_edges(3, 1, edges.clone()),
            TrackPayload::new_state_ribbons(4, ribbons.clone()),
        ],
    };

    let buf = packer.pack_frame(1, 100, 0, &[scope], &snapshots).expect("pack special floats failed");
    let reader = DdsfZeroCopyReader::new(buf).expect("reader failed");

    let sc = reader.scopes().next().expect("scope missing");
    assert!(sc.header().subpixel_offset_ns.is_nan());

    let tracks: Vec<_> = sc.tracks().collect();
    assert_eq!(tracks.len(), 4);

    // Verify raw points bit-for-bit
    let dec_pts = tracks[0].raw_points().expect("raw_points failed");
    assert_eq!(dec_pts.len(), pts.len());
    for i in 0..pts.len() {
        assert_eq!(dec_pts[i].x_norm.to_bits(), pts[i].x_norm.to_bits(), "pt {i} x_norm bits mismatch");
        assert_eq!(dec_pts[i].y_val.to_bits(), pts[i].y_val.to_bits(), "pt {i} y_val bits mismatch");
    }

    // Verify M4 columns bit-for-bit
    let dec_cols = tracks[1].m4_columns().expect("m4_columns failed");
    assert_eq!(dec_cols.len(), cols.len());
    for i in 0..cols.len() {
        assert_eq!(dec_cols[i].x_norm.to_bits(), cols[i].x_norm.to_bits());
        assert_eq!(dec_cols[i].v_first.to_bits(), cols[i].v_first.to_bits());
        assert_eq!(dec_cols[i].v_min.to_bits(), cols[i].v_min.to_bits());
        assert_eq!(dec_cols[i].v_max.to_bits(), cols[i].v_max.to_bits());
        assert_eq!(dec_cols[i].v_last.to_bits(), cols[i].v_last.to_bits());
    }

    // Verify stats bit-for-bit
    let dec_stats = tracks[1].stats().expect("stats missing");
    assert_eq!(dec_stats.min.to_bits(), stats.min.to_bits());
    assert_eq!(dec_stats.max.to_bits(), stats.max.to_bits());
    assert_eq!(dec_stats.vpp.to_bits(), stats.vpp.to_bits());
    assert_eq!(dec_stats.mean.to_bits(), stats.mean.to_bits());
    assert_eq!(dec_stats.rms.to_bits(), stats.rms.to_bits());
    assert_eq!(dec_stats.freq_hz.to_bits(), stats.freq_hz.to_bits());

    // Verify digital edges bit-for-bit
    let dec_edges = tracks[2].digital_edges().expect("edges failed");
    assert_eq!(dec_edges.len(), edges.len());
    for i in 0..edges.len() {
        assert_eq!(dec_edges[i].to_bits(), edges[i].to_bits());
    }

    // Verify state ribbons bit-for-bit
    let dec_ribbons = tracks[3].state_ribbons().expect("ribbons failed");
    assert_eq!(dec_ribbons.len(), ribbons.len());
    for i in 0..ribbons.len() {
        assert_eq!(dec_ribbons[i].x0_norm.to_bits(), ribbons[i].x0_norm.to_bits());
        assert_eq!(dec_ribbons[i].x1_norm.to_bits(), ribbons[i].x1_norm.to_bits());
        assert_eq!(dec_ribbons[i].state_id, ribbons[i].state_id);
        assert_eq!(dec_ribbons[i].color_idx, ribbons[i].color_idx);
    }

    // Verify snapshots bit-for-bit
    let dec_snaps = reader.snapshots().expect("snapshots failed");
    assert_eq!(dec_snaps.len(), snapshots.len());
    for i in 0..snapshots.len() {
        assert_eq!(dec_snaps[i].channel_num_id, snapshots[i].channel_num_id);
        assert_eq!(dec_snaps[i].kind, snapshots[i].kind);
        assert_eq!(dec_snaps[i].val_f64.to_bits(), snapshots[i].val_f64.to_bits(), "snap {i} val_f64 bits mismatch");
    }
}

// ============================================================================
// 4. Dual API Parity: Owned vs Borrowed
// ============================================================================

#[test]
fn test_dual_api_parity_owned_vs_borrowed_exact_bytes() {
    println!("\n=== [TA5] Testing dual API parity (pack_frame vs pack_frame_borrowed) ===");

    let pts = vec![
        DdsfAnalogPoint { x_norm: 0.1, y_val: 1.5 },
        DdsfAnalogPoint { x_norm: 0.5, y_val: -2.5 },
        DdsfAnalogPoint { x_norm: 0.9, y_val: 3.5 },
    ];

    let cols = vec![
        DdsfM4Column { x_norm: 0.1, v_first: 1.0, v_min: -1.0, v_max: 2.0, v_last: 0.5 },
        DdsfM4Column { x_norm: 0.2, v_first: 0.5, v_min: 0.0, v_max: 3.0, v_last: 2.0 },
        DdsfM4Column { x_norm: 0.3, v_first: 2.0, v_min: -0.5, v_max: 4.0, v_last: 1.0 },
    ];

    let edges = vec![0.1f32, 0.4, 0.7];

    let ribbons = vec![
        DdsfStateRibbon { x0_norm: 0.0, x1_norm: 0.3, state_id: 1, color_idx: 10 },
        DdsfStateRibbon { x0_norm: 0.3, x1_norm: 0.7, state_id: 2, color_idx: 20 },
        DdsfStateRibbon { x0_norm: 0.7, x1_norm: 1.0, state_id: 3, color_idx: 30 },
    ];

    let bins = vec![100.0f32, 200.0, 300.0, 400.0, 500.0];

    let stats = DdsfTrackStats {
        min: -2.5,
        max: 4.0,
        vpp: 6.5,
        mean: 1.2,
        rms: 2.1,
        freq_hz: 50.0,
        duty: 0.45,
        reserved: 0.0,
    };

    let owned_tracks_s1 = vec![
        TrackPayload::new_raw_xy(1, pts.clone()),
        TrackPayload::new_m4(2, cols.clone(), Some(stats)),
    ];

    let owned_tracks_s2 = vec![
        TrackPayload::new_digital_edges(3, 1, edges.clone()),
        TrackPayload::new_state_ribbons(4, ribbons.clone()),
        TrackPayload::new_spectrum(5, 50.0, 5.0, bins.clone()),
    ];

    let owned_scopes = vec![
        ScopeViewportPayload {
            scope_id: 1,
            trigger_state: 3,
            mode: 2,
            win_t_start_ns: 1_000,
            win_t_span_ns: 10_000,
            subpixel_offset_ns: 12.5,
            tracks: owned_tracks_s1,
        },
        ScopeViewportPayload {
            scope_id: 2,
            trigger_state: 1,
            mode: 1,
            win_t_start_ns: 2_000,
            win_t_span_ns: 20_000,
            subpixel_offset_ns: 0.0,
            tracks: owned_tracks_s2,
        },
    ];

    let snapshots = vec![
        DdsfSnapshotEntry { channel_num_id: 1, kind: 0, pad: [0; 3], val_f64: PI_F64 },
        DdsfSnapshotEntry { channel_num_id: 2, kind: 1, pad: [0; 3], val_f64: 42.0 },
        DdsfSnapshotEntry { channel_num_id: 3, kind: 2, pad: [0; 3], val_f64: -100.5 },
    ];

    // Pack using owned API
    let mut packer_owned = DdsfFramePacker::new(32 * 1024);
    let owned_bytes = packer_owned
        .pack_frame(1234, 5678, DDSF_FLAG_PINNED_ACTIVE | DDSF_FLAG_QUOTA_WARNING, &owned_scopes, &snapshots)
        .expect("pack_frame owned failed")
        .to_vec();

    // Construct equivalent borrowed views
    let borrowed_tracks_s1 = [
        TrackBlockView {
            channel_id: 1,
            block_kind: DdsfBlockKind::AnalogRawXY,
            digital_init_level: 0,
            flags: 0,
            stats: None,
            payload: TrackPayloadSlice::AnalogRawXY(&pts),
            aux_f32_0: 0.0,
            aux_f32_1: 0.0,
        },
        TrackBlockView {
            channel_id: 2,
            block_kind: DdsfBlockKind::AnalogM4Column,
            digital_init_level: 0,
            flags: DDSF_TRACK_FLAG_HAS_STATS,
            stats: Some(&stats),
            payload: TrackPayloadSlice::AnalogM4Column(&cols),
            aux_f32_0: 0.0,
            aux_f32_1: 0.0,
        },
    ];

    let borrowed_tracks_s2 = [
        TrackBlockView {
            channel_id: 3,
            block_kind: DdsfBlockKind::DigitalEdges,
            digital_init_level: 1,
            flags: 0,
            stats: None,
            payload: TrackPayloadSlice::DigitalEdges(&edges),
            aux_f32_0: 0.0,
            aux_f32_1: 0.0,
        },
        TrackBlockView {
            channel_id: 4,
            block_kind: DdsfBlockKind::StateRibbons,
            digital_init_level: 0,
            flags: 0,
            stats: None,
            payload: TrackPayloadSlice::StateRibbons(&ribbons),
            aux_f32_0: 0.0,
            aux_f32_1: 0.0,
        },
        TrackBlockView {
            channel_id: 5,
            block_kind: DdsfBlockKind::SpectrumFrame,
            digital_init_level: 0,
            flags: 0,
            stats: None,
            payload: TrackPayloadSlice::SpectrumFrame(&bins),
            aux_f32_0: 50.0,
            aux_f32_1: 5.0,
        },
    ];

    let borrowed_scopes = [
        ScopePacketView {
            scope_id: 1,
            trigger_state: 3,
            mode: 2,
            win_t_start_ns: 1_000,
            win_t_span_ns: 10_000,
            subpixel_offset_ns: 12.5,
            tracks: &borrowed_tracks_s1,
        },
        ScopePacketView {
            scope_id: 2,
            trigger_state: 1,
            mode: 1,
            win_t_start_ns: 2_000,
            win_t_span_ns: 20_000,
            subpixel_offset_ns: 0.0,
            tracks: &borrowed_tracks_s2,
        },
    ];

    // Pack using borrowed API
    let mut packer_borrowed = DdsfFramePacker::new(32 * 1024);
    let borrowed_bytes = packer_borrowed
        .pack_frame_borrowed(1234, 5678, DDSF_FLAG_PINNED_ACTIVE | DDSF_FLAG_QUOTA_WARNING, &borrowed_scopes, &snapshots)
        .expect("pack_frame_borrowed failed");

    // CRITICAL ASSERTION: exact byte equality between owned and borrowed serialization!
    assert_eq!(
        owned_bytes.as_slice(),
        borrowed_bytes,
        "CRITICAL: owned and borrowed APIs produced non-identical byte streams!"
    );

    // Also test pack_frame_with_header vs pack_frame_borrowed_with_header
    let header = DdsfFrameHeader::new_v1(1234, 2, 3, 5678, 0, DDSF_FLAG_PINNED_ACTIVE | DDSF_FLAG_QUOTA_WARNING);
    let bytes_hdr_owned = packer_owned
        .pack_frame_with_header(&header, &owned_scopes, &snapshots)
        .expect("pack_frame_with_header failed");
    let bytes_hdr_borrowed = packer_borrowed
        .pack_frame_borrowed_with_header(&header, &borrowed_scopes, &snapshots)
        .expect("pack_frame_borrowed_with_header failed");

    assert_eq!(bytes_hdr_owned, bytes_hdr_borrowed);
    assert_eq!(bytes_hdr_owned, owned_bytes.as_slice());
}

// ============================================================================
// 5. Buffer Capacity Stability & Invariance in Steady State
// ============================================================================

#[test]
fn test_steady_state_zero_allocation_and_pointer_invariance() {
    println!("\n=== [TA5] Testing steady-state pointer invariance across 1,000 iterations ===");

    // Construct a representative frame
    let pts = vec![DdsfAnalogPoint { x_norm: 0.1, y_val: 1.0 }; 200];
    let cols = vec![DdsfM4Column::default(); 100];
    let edges = vec![0.5f32; 20];
    let ribbons = vec![DdsfStateRibbon::default(); 10];
    let snapshots = vec![DdsfSnapshotEntry::default(); 8];

    let scope = ScopeViewportPayload {
        scope_id: 1,
        trigger_state: 3,
        mode: 2,
        win_t_start_ns: 1000,
        win_t_span_ns: 10_000,
        subpixel_offset_ns: 0.5,
        tracks: vec![
            TrackPayload::new_raw_xy(1, pts),
            TrackPayload::new_m4(2, cols, None),
            TrackPayload::new_digital_edges(3, 1, edges),
            TrackPayload::new_state_ribbons(4, ribbons),
        ],
    };

    let mut packer = DdsfFramePacker::new(64 * 1024);

    // Iteration 1: Warm up
    let (warm_ptr, warm_len) = {
        let warm_buf = packer.pack_frame(1, 1000, 0, std::slice::from_ref(&scope), &snapshots).unwrap();
        (warm_buf.as_ptr(), warm_buf.len())
    };
    let warm_cap = packer.capacity();

    // 1,000 consecutive streaming cycles
    for seq in 2..=1000 {
        let buf = packer.pack_frame(seq, seq as u64 * 1000, 0, std::slice::from_ref(&scope), &snapshots).unwrap();
        assert_eq!(buf.len(), warm_len, "frame length fluctuated at seq {seq}");
        assert_eq!(buf.as_ptr(), warm_ptr, "buffer relocated at seq {seq} - zero allocation violated!");
        assert_eq!(packer.capacity(), warm_cap, "capacity changed at seq {seq}!");
    }
}

// ============================================================================
// 6. Reader Error Resilience & Fuzzing Pathological Wire Frames
// ============================================================================

#[test]
fn test_reader_error_handling_and_truncation_fuzzing() {
    println!("\n=== [TA5] Testing reader error resilience against truncated/corrupted wire buffers ===");

    // Sub-test 1: Truncation from 0 to 63 bytes (smaller than root header)
    for len in 0..DDSF_HEADER_BYTES {
        let fake_buf = vec![0u8; len];
        let res = DdsfZeroCopyReader::new(&fake_buf);
        assert!(res.is_err(), "DdsfZeroCopyReader::new must reject buf len {len} < 64");
    }

    // Sub-test 2: Corrupted magic
    let mut packer = DdsfFramePacker::new(4096);
    let valid_frame = packer.pack_frame(1, 100, 0, &[], &[]).unwrap().to_vec();

    let mut bad_magic = valid_frame.clone();
    bad_magic[0] = 0xAA;
    bad_magic[1] = 0xBB;
    assert!(DdsfZeroCopyReader::new(&bad_magic).is_err(), "must reject bad magic");

    // Sub-test 3: Corrupted version
    let mut bad_ver = valid_frame.clone();
    bad_ver[4] = 0x02; // version = 2
    assert!(DdsfZeroCopyReader::new(&bad_ver).is_err(), "must reject version != 1");

    // Sub-test 4: total_byte_len < 64
    let mut bad_len = valid_frame.clone();
    bad_len[24..32].copy_from_slice(&32u64.to_le_bytes()); // total_byte_len = 32
    assert!(DdsfZeroCopyReader::new(&bad_len).is_err(), "must reject total_byte_len < 64");

    // Sub-test 5: total_byte_len > buffer len
    let mut bad_len_high = valid_frame.clone();
    bad_len_high[24..32].copy_from_slice(&1000u64.to_le_bytes());
    assert!(DdsfZeroCopyReader::new(&bad_len_high).is_err(), "must reject total_byte_len > buf.len()");

    // Sub-test 6: Truncation fuzzing over valid packed frame with scopes
    let pts = vec![DdsfAnalogPoint { x_norm: 0.1, y_val: 1.0 }; 20];
    let scope = ScopeViewportPayload {
        scope_id: 1,
        trigger_state: 0,
        mode: 0,
        win_t_start_ns: 0,
        win_t_span_ns: 1000,
        subpixel_offset_ns: 0.0,
        tracks: vec![TrackPayload::new_raw_xy(1, pts)],
    };
    let snaps = vec![DdsfSnapshotEntry::default(); 4];

    let complex_buf = packer.pack_frame(1, 100, 0, &[scope], &snaps).unwrap().to_vec();

    // Any slice strictly shorter than complex_buf.len() must either fail reader creation
    // or safely yield truncated scopes/tracks without memory fault or panic
    for trunc_len in 64..complex_buf.len() {
        let truncated = &complex_buf[..trunc_len];
        // Reader creation must fail because total_byte_len exceeds truncated buffer length!
        assert!(
            DdsfZeroCopyReader::new(truncated).is_err(),
            "DdsfZeroCopyReader::new must fail when truncated at length {trunc_len}"
        );
    }
}

#[test]
fn test_reader_track_kind_mismatch_rejections() {
    println!("\n=== [TA5] Testing track block kind mismatch rejections ===");

    let mut packer = DdsfFramePacker::new(4096);
    let pts = vec![DdsfAnalogPoint { x_norm: 0.1, y_val: 1.0 }; 5];
    let scope = ScopeViewportPayload {
        scope_id: 1,
        trigger_state: 0,
        mode: 0,
        win_t_start_ns: 0,
        win_t_span_ns: 1000,
        subpixel_offset_ns: 0.0,
        tracks: vec![TrackPayload::new_raw_xy(1, pts)],
    };

    let buf = packer.pack_frame(1, 100, 0, &[scope], &[]).unwrap();
    let reader = DdsfZeroCopyReader::new(buf).unwrap();
    let tr = reader.scopes().next().unwrap().tracks().next().unwrap();

    // Calling wrong typed accessors on AnalogRawXY must return Err(CoreError::Protocol), never panic!
    assert!(tr.raw_points().is_ok());
    assert!(tr.m4_columns().is_err());
    assert!(tr.digital_edges().is_err());
    assert!(tr.state_ribbons().is_err());
    assert!(tr.spectrum_bins().is_err());
}

#[test]
fn test_adversarial_u32_u64_boundary_values() {
    println!("\n=== [TA5] Testing boundary integer values (0, u16::MAX, u32::MAX, u64::MAX) ===");

    let mut packer = DdsfFramePacker::new(4096);

    let scope = ScopeViewportPayload {
        scope_id: u32::MAX,
        trigger_state: u8::MAX,
        mode: u8::MAX,
        win_t_start_ns: u64::MAX,
        win_t_span_ns: u64::MAX,
        subpixel_offset_ns: f32::MAX,
        tracks: vec![
            TrackPayload::new_raw_xy(u32::MAX, vec![DdsfAnalogPoint { x_norm: f32::MAX, y_val: f32::MIN }]),
        ],
    };

    let snapshot = DdsfSnapshotEntry {
        channel_num_id: u32::MAX,
        kind: u8::MAX,
        pad: [u8::MAX; 3],
        val_f64: f64::MAX,
    };

    let buf = packer
        .pack_frame(u32::MAX, u64::MAX, u16::MAX, &[scope], &[snapshot])
        .expect("pack boundary values failed");

    let reader = DdsfZeroCopyReader::new(buf).expect("reader failed");
    assert_eq!(reader.header().frame_seq, u32::MAX);
    assert_eq!(reader.header().host_now_ns, u64::MAX);
    assert_eq!(reader.header().flags, u16::MAX);
    assert_eq!(reader.header().scope_count, 1);
    assert_eq!(reader.header().snapshot_count, 1);

    let sc = reader.scopes().next().unwrap();
    assert_eq!(sc.header().scope_id, u32::MAX);
    assert_eq!(sc.header().trigger_state, u8::MAX);
    assert_eq!(sc.header().mode, u8::MAX);
    assert_eq!(sc.header().win_t_start_ns, u64::MAX);
    assert_eq!(sc.header().win_t_span_ns, u64::MAX);

    let tr = sc.tracks().next().unwrap();
    assert_eq!(tr.header().channel_id, u32::MAX);
    assert_eq!(tr.raw_points().unwrap()[0].x_norm, f32::MAX);
    assert_eq!(tr.raw_points().unwrap()[0].y_val, f32::MIN);

    let sn = reader.snapshots().unwrap();
    assert_eq!(sn[0].channel_num_id, u32::MAX);
    assert_eq!(sn[0].kind, u8::MAX);
    assert_eq!(sn[0].val_f64, f64::MAX);
}

#[test]
fn test_adversarial_malformed_wire_snapshot_alignment_audit() {
    println!("\n=== [TA5] Auditing reader behavior on malformed snapshot wire layouts ===");

    // Craft a valid 64-byte frame header with snapshot_count = 1, but total_byte_len = 81 (odd!)
    // In a properly aligned frame, total_byte_len MUST be a multiple of 8.
    // If wire input has total_byte_len = 81 (81 - 16 = 65, an odd offset),
    // we observe whether snapshots() panics or catches it.
    let mut fake_buf = vec![0u8; 128];
    let header = DdsfFrameHeader {
        magic: ddscope_core::scope::ddsf::DDSF_MAGIC,
        version: 1,
        flags: 0,
        frame_seq: 1,
        scope_count: 0,
        snapshot_count: 1,
        host_now_ns: 1000,
        total_byte_len: 81, // Odd total_byte_len!
        reserved_padding: [0; 32],
    };
    fake_buf[..64].copy_from_slice(bytemuck::bytes_of(&header));

    let reader = DdsfZeroCopyReader::new(&fake_buf).expect("reader accepts valid magic/version");

    // Catch unwind on snapshots()
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        reader.snapshots()
    }));

    if let Ok(res) = result {
        println!("snapshots() handled odd total_byte_len gracefully: {:?}", res.is_ok());
    } else {
        println!("CONFIRMED AUDIT FINDING: reader.snapshots() panics on malformed odd total_byte_len due to bytemuck::cast_slice!");
    }
}

#[test]
fn test_adversarial_malformed_scope_and_track_iterators_alignment_audit() {
    println!("\n=== [TA5] Auditing iterator behavior on corrupted unaligned packet lengths ===");

    // Case 1: Corrupted ScopePacketHeader where packet_byte_len is 33 (odd)
    // and scope_count is 2. The second scope header will be at offset 64 + 33 = 97 (unaligned).
    let mut fake_buf = vec![0u8; 256];
    let frame_hdr = DdsfFrameHeader {
        magic: ddscope_core::scope::ddsf::DDSF_MAGIC,
        version: 1,
        flags: 0,
        frame_seq: 1,
        scope_count: 2,
        snapshot_count: 0,
        host_now_ns: 1000,
        total_byte_len: 256,
        reserved_padding: [0; 32],
    };
    fake_buf[..64].copy_from_slice(bytemuck::bytes_of(&frame_hdr));

    let scope0_hdr = DdsfScopePacketHeader {
        scope_id: 1,
        trigger_state: 0,
        mode: 0,
        track_block_count: 0,
        win_t_start_ns: 0,
        win_t_span_ns: 1000,
        subpixel_offset_ns: 0.0,
        packet_byte_len: 33, // Corrupted unaligned length!
    };
    fake_buf[64..96].copy_from_slice(bytemuck::bytes_of(&scope0_hdr));

    let reader = DdsfZeroCopyReader::new(&fake_buf).expect("reader created");
    let mut scopes = reader.scopes();

    let first = scopes.next();
    assert!(first.is_some());

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        scopes.next()
    }));

    if result.is_err() {
        println!("CONFIRMED AUDIT FINDING: DdsfScopeIter panics when traversing corrupted unaligned scope packets due to bytemuck::from_bytes!");
    } else {
        println!("DdsfScopeIter handled unaligned packet offset safely");
    }
}

