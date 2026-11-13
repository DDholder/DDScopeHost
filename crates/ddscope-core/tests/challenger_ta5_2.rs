//! Empirical Challenger 2 Adversarial Stress Suite for Milestone TA5 (Zero-Copy DDSF v1 Frame Packer).
//!
//! Validates:
//! 1. Zero Secondary Allocation Invariant:
//!    - Steady-state packing across 10,000 continuous frames with fluctuating payload sizes performs EXACTLY 0 heap allocations.
//!    - Steady-state packing with borrowed views across 10,000 frames performs EXACTLY 0 heap allocations.
//! 2. Pointer Stability:
//!    - Buffer base pointer `buffer().as_ptr()` remains byte-for-byte identical across all 10,000 iterations.
//! 3. Memory Leak and Unbounded Growth Absence:
//!    - Streaming up to 50,000 continuous frames maintains strictly fixed buffer capacity (no unbounded growth).
//! 4. Capacity Expansion and Re-stabilization:
//!    - Exceeding buffer capacity safely expands the buffer, updates high watermark, successfully packs,
//!      and immediately resumes 0 secondary allocations and stable base pointer on subsequent frames.
//! 5. Odd and Boundary Alignment Padding:
//!    - Unaligned element payloads (e.g. 1 float, odd byte lengths) are padded to 8-byte boundaries with zero secondary allocations.
//! 6. Zero-Copy Deserialization Reader:
//!    - Reading, iterating scopes, tracks, and casting slices performs EXACTLY 0 heap allocations and matches wire specification.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Mutex;

use ddscope_core::scope::ddsf::{
    DdsfAnalogPoint, DdsfBlockKind, DdsfFramePacker, DdsfM4Column, DdsfSnapshotEntry,
    DdsfSnapshotKind, DdsfStateRibbon, DdsfTrackStats, DdsfZeroCopyReader, ScopePacketView,
    ScopeViewportPayload, TrackBlockView, TrackPayload, TrackPayloadSlice, DDSF_FLAG_PINNED_ACTIVE,
    DDSF_FLAG_QUOTA_WARNING, DDSF_HEADER_BYTES, DDSF_MAGIC, DDSF_TRACK_FLAG_HAS_STATS,
    DDSF_VERSION_V1,
};

thread_local! {
    static THREAD_TRACKING_ENABLED: Cell<bool> = const { Cell::new(false) };
    static THREAD_ALLOC_COUNT: Cell<usize> = const { Cell::new(0) };
    static THREAD_DEALLOC_COUNT: Cell<usize> = const { Cell::new(0) };
    static THREAD_ALLOC_BYTES: Cell<usize> = const { Cell::new(0) };
}

/// Thread-isolated counting tracking allocator for empirical allocation invariant testing.
struct TrackingAllocator;

impl TrackingAllocator {
    const fn new() -> Self {
        Self
    }

    #[inline]
    fn enable(&self) {
        let _ = THREAD_TRACKING_ENABLED.try_with(|c| c.set(true));
    }

    #[inline]
    fn disable(&self) {
        let _ = THREAD_TRACKING_ENABLED.try_with(|c| c.set(false));
    }

    #[inline]
    fn reset(&self) {
        let _ = THREAD_ALLOC_COUNT.try_with(|c| c.set(0));
        let _ = THREAD_DEALLOC_COUNT.try_with(|c| c.set(0));
        let _ = THREAD_ALLOC_BYTES.try_with(|c| c.set(0));
    }

    #[inline]
    fn allocs(&self) -> usize {
        THREAD_ALLOC_COUNT.try_with(|c| c.get()).unwrap_or(0)
    }

    #[inline]
    fn deallocs(&self) -> usize {
        THREAD_DEALLOC_COUNT.try_with(|c| c.get()).unwrap_or(0)
    }

    #[inline]
    fn bytes(&self) -> usize {
        THREAD_ALLOC_BYTES.try_with(|c| c.get()).unwrap_or(0)
    }
}

unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = THREAD_TRACKING_ENABLED.try_with(|enabled| {
            if enabled.get() {
                let _ = THREAD_ALLOC_COUNT.try_with(|c| c.set(c.get() + 1));
                let _ = THREAD_ALLOC_BYTES.try_with(|c| c.set(c.get() + layout.size()));
            }
        });
        // SAFETY: Delegating directly to the host system allocator.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let _ = THREAD_TRACKING_ENABLED.try_with(|enabled| {
            if enabled.get() {
                let _ = THREAD_DEALLOC_COUNT.try_with(|c| c.set(c.get() + 1));
            }
        });
        // SAFETY: Delegating directly to the host system allocator.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let _ = THREAD_TRACKING_ENABLED.try_with(|enabled| {
            if enabled.get() {
                let _ = THREAD_ALLOC_COUNT.try_with(|c| c.set(c.get() + 1));
                let _ = THREAD_ALLOC_BYTES.try_with(|c| c.set(c.get() + new_size));
            }
        });
        // SAFETY: Delegating directly to the host system allocator.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL_TRACKER: TrackingAllocator = TrackingAllocator::new();

/// Mutex ensuring serialized execution across test cases so tracking allocator measurements never collide.
static SERIAL_TEST_LOCK: Mutex<()> = Mutex::new(());

/// Lightweight deterministic XorShift64 PRNG for repeatable fuzz testing.
struct XorShift64 {
    state: u64,
}

impl XorShift64 {
    fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 { 0x5a17_d00d_beef_cafe } else { seed },
        }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    fn next_range(&mut self, min: usize, max: usize) -> usize {
        if min >= max {
            return min;
        }
        min + (self.next_u64() as usize % (max - min + 1))
    }
}

/// Helper to build a representative mixed-signal scope with specified counts.
fn make_test_scope(
    scope_id: u32,
    raw_len: usize,
    m4_len: usize,
    edges_len: usize,
    ribbons_len: usize,
) -> ScopeViewportPayload {
    let raw_points: Vec<DdsfAnalogPoint> = (0..raw_len)
        .map(|i| DdsfAnalogPoint {
            x_norm: (i as f32) / (raw_len.max(1) as f32),
            y_val: ((i as f32) * 0.05).sin(),
        })
        .collect();

    let m4_columns: Vec<DdsfM4Column> = (0..m4_len)
        .map(|i| DdsfM4Column {
            x_norm: (i as f32) / (m4_len.max(1) as f32),
            v_first: 1.0,
            v_min: -2.5,
            v_max: 3.5,
            v_last: 0.8,
        })
        .collect();

    let stats = DdsfTrackStats {
        min: -2.5,
        max: 3.5,
        vpp: 6.0,
        mean: 0.12,
        rms: 1.85,
        freq_hz: 1000.0,
        duty: 0.5,
        reserved: 0.0,
    };

    let digital_edges: Vec<f32> = (0..edges_len)
        .map(|i| (i as f32) / (edges_len.max(1) as f32))
        .collect();

    let state_ribbons: Vec<DdsfStateRibbon> = (0..ribbons_len)
        .map(|i| DdsfStateRibbon {
            x0_norm: (i as f32) / (ribbons_len.max(1) as f32),
            x1_norm: ((i + 1) as f32) / (ribbons_len.max(1) as f32),
            state_id: (i % 4) as i32,
            color_idx: (i % 8) as u32,
        })
        .collect();

    ScopeViewportPayload {
        scope_id,
        trigger_state: 3,
        mode: 2,
        win_t_start_ns: 1_000_000_000,
        win_t_span_ns: 10_000_000,
        subpixel_offset_ns: 12.5,
        tracks: vec![
            TrackPayload::new_raw_xy(1, raw_points),
            TrackPayload::new_m4(2, m4_columns, Some(stats)),
            TrackPayload::new_digital_edges(3, 1, digital_edges),
            TrackPayload::new_state_ribbons(4, state_ribbons),
        ],
    }
}

/// Helper to generate N test snapshot entries.
fn make_test_snapshots(count: usize) -> Vec<DdsfSnapshotEntry> {
    (0..count)
        .map(|i| DdsfSnapshotEntry {
            channel_num_id: i as u32,
            kind: (i % 3) as u8,
            pad: [0; 3],
            val_f64: (i as f64) * std::f64::consts::PI,
        })
        .collect()
}

// ============================================================================
// Test 1: 10,000 Frames Zero Secondary Heap Allocation & Pointer Stability
// ============================================================================

#[test]
fn test_adversarial_10k_frames_zero_alloc_and_pointer_stability() {
    let _guard = SERIAL_TEST_LOCK.lock().unwrap();

    // 1. Pre-generate 32 frames with fluctuating payload sizes (ranging from ~1 KB up to ~150 KB)
    let mut rng = XorShift64::new(0x1234_5678_9abc);
    let mut test_frames = Vec::with_capacity(32);
    for _ in 0..32 {
        let num_scopes = rng.next_range(1, 4);
        let mut scopes = Vec::with_capacity(num_scopes);
        for s in 0..num_scopes {
            let raw_len = rng.next_range(100, 2500);
            let m4_len = rng.next_range(50, 1000);
            let edges_len = rng.next_range(10, 200);
            let ribbons_len = rng.next_range(5, 50);
            scopes.push(make_test_scope(
                (s + 1) as u32,
                raw_len,
                m4_len,
                edges_len,
                ribbons_len,
            ));
        }
        let snap_count = rng.next_range(0, 32);
        let snapshots = make_test_snapshots(snap_count);
        test_frames.push((scopes, snapshots));
    }

    // 2. Initialize packer with 512 KB capacity (comfortably accommodates max ~200 KB frame)
    let initial_cap = 512 * 1024;
    let mut packer = DdsfFramePacker::new(initial_cap);
    let expected_cap = packer.capacity();
    assert!(
        expected_cap >= initial_cap,
        "Capacity must be at least requested initial_cap"
    );

    // Warm-up iteration
    let (ref w_scopes, ref w_snaps) = test_frames[0];
    packer
        .pack_frame(1, 1_000_000_000, DDSF_FLAG_PINNED_ACTIVE, w_scopes, w_snaps)
        .expect("warm-up pack ok");
    let baseline_ptr = packer.buffer().as_ptr();
    assert!(!baseline_ptr.is_null());

    // 3. Begin 10,000 iterations under Global Tracking Allocator
    GLOBAL_TRACKER.reset();
    GLOBAL_TRACKER.enable();

    for i in 0..10_000 {
        let (ref scopes, ref snaps) = test_frames[i % 32];
        let seq = (i + 2) as u32;
        let ts = 1_000_000_000 + (i as u64) * 1_000;
        let flags = if i % 2 == 0 {
            DDSF_FLAG_PINNED_ACTIVE
        } else {
            DDSF_FLAG_QUOTA_WARNING
        };

        let res = packer.pack_frame(seq, ts, flags, scopes, snaps);
        assert!(res.is_ok(), "pack_frame failed at iteration {i}");

        // Pointer Stability Invariant: buffer().as_ptr() MUST remain byte-identical
        let current_ptr = packer.buffer().as_ptr();
        assert_eq!(
            current_ptr, baseline_ptr,
            "CRITICAL: Pointer changed at iteration {i}! Expected {baseline_ptr:?}, got {current_ptr:?}"
        );

        // Capacity Invariant: capacity must NOT grow when fitting within bounds
        assert_eq!(
            packer.capacity(),
            expected_cap,
            "CRITICAL: Capacity mutated from {expected_cap} to {} at iteration {i}",
            packer.capacity()
        );
    }

    GLOBAL_TRACKER.disable();

    // 4. Invariant Assertion: EXACTLY ZERO secondary heap allocations
    let total_allocs = GLOBAL_TRACKER.allocs();
    let total_deallocs = GLOBAL_TRACKER.deallocs();
    let total_bytes = GLOBAL_TRACKER.bytes();

    assert_eq!(
        total_allocs, 0,
        "CRITICAL VIOLATION: 10,000 continuous frames caused {total_allocs} heap allocations ({total_bytes} bytes)!"
    );
    assert_eq!(
        total_deallocs, 0,
        "CRITICAL VIOLATION: 10,000 continuous frames caused {total_deallocs} deallocations!"
    );

    // 5. Final wire verification of the last frame
    let last_frame_bytes = packer.buffer();
    let reader = DdsfZeroCopyReader::new(last_frame_bytes)
        .expect("ZeroCopyReader failed to parse final frame");
    assert_eq!(reader.header().frame_seq, 10_001);
    assert_eq!(reader.header().total_byte_len, last_frame_bytes.len() as u64);
}

// ============================================================================
// Test 2: 10,000 Frames Zero Secondary Allocations with Borrowed Views
// ============================================================================

#[test]
fn test_adversarial_10k_frames_borrowed_views_zero_alloc() {
    let _guard = SERIAL_TEST_LOCK.lock().unwrap();

    // 1. Pre-generate backing buffers for borrowed views
    let points: Vec<DdsfAnalogPoint> = (0..5000)
        .map(|i| DdsfAnalogPoint {
            x_norm: i as f32 / 5000.0,
            y_val: (i as f32 * 0.1).sin(),
        })
        .collect();

    let m4_cols: Vec<DdsfM4Column> = (0..2000)
        .map(|i| DdsfM4Column {
            x_norm: i as f32 / 2000.0,
            v_first: 1.0,
            v_min: -1.0,
            v_max: 2.0,
            v_last: 0.5,
        })
        .collect();

    let edges: Vec<f32> = (0..500).map(|i| i as f32 / 500.0).collect();
    let ribbons: Vec<DdsfStateRibbon> = (0..100)
        .map(|i| DdsfStateRibbon {
            x0_norm: i as f32 / 100.0,
            x1_norm: (i + 1) as f32 / 100.0,
            state_id: i % 5,
            color_idx: (i % 10) as u32,
        })
        .collect();
    let snapshots = make_test_snapshots(16);

    let stats = DdsfTrackStats {
        min: -1.0,
        max: 2.0,
        vpp: 3.0,
        mean: 0.5,
        rms: 0.8,
        freq_hz: 50.0,
        duty: 0.5,
        reserved: 0.0,
    };

    let mut packer = DdsfFramePacker::new(256 * 1024);
    let expected_cap = packer.capacity();

    // Pre-warm with a dummy borrowed view
    let dummy_tracks = [TrackBlockView {
        channel_id: 1,
        block_kind: DdsfBlockKind::AnalogRawXY,
        digital_init_level: 0,
        flags: 0,
        stats: None,
        payload: TrackPayloadSlice::AnalogRawXY(&points[..100]),
        aux_f32_0: 0.0,
        aux_f32_1: 0.0,
    }];
    let dummy_scopes = [ScopePacketView {
        scope_id: 1,
        trigger_state: 2,
        mode: 1,
        win_t_start_ns: 1000,
        win_t_span_ns: 5000,
        subpixel_offset_ns: 0.0,
        tracks: &dummy_tracks,
    }];
    packer
        .pack_frame_borrowed(1, 1000, 0, &dummy_scopes, &snapshots[..4])
        .expect("prewarm ok");
    let baseline_ptr = packer.buffer().as_ptr();

    // 2. Run 10,000 iterations of pack_frame_borrowed under tracking allocator
    GLOBAL_TRACKER.reset();
    GLOBAL_TRACKER.enable();

    for i in 0..10_000 {
        // Vary slice windows deterministically on the stack without any heap allocations
        let pt_len = 100 + (i * 37) % 4000;
        let m4_len = 50 + (i * 17) % 1500;
        let ed_len = 10 + (i * 7) % 400;
        let rb_len = 5 + (i * 3) % 80;

        let tracks = [
            TrackBlockView {
                channel_id: 101,
                block_kind: DdsfBlockKind::AnalogRawXY,
                digital_init_level: 0,
                flags: 0,
                stats: None,
                payload: TrackPayloadSlice::AnalogRawXY(&points[..pt_len]),
                aux_f32_0: 0.0,
                aux_f32_1: 0.0,
            },
            TrackBlockView {
                channel_id: 102,
                block_kind: DdsfBlockKind::AnalogM4Column,
                digital_init_level: 0,
                flags: DDSF_TRACK_FLAG_HAS_STATS,
                stats: Some(&stats),
                payload: TrackPayloadSlice::AnalogM4Column(&m4_cols[..m4_len]),
                aux_f32_0: 0.0,
                aux_f32_1: 0.0,
            },
            TrackBlockView {
                channel_id: 103,
                block_kind: DdsfBlockKind::DigitalEdges,
                digital_init_level: 1,
                flags: 0,
                stats: None,
                payload: TrackPayloadSlice::DigitalEdges(&edges[..ed_len]),
                aux_f32_0: 0.0,
                aux_f32_1: 0.0,
            },
            TrackBlockView {
                channel_id: 104,
                block_kind: DdsfBlockKind::StateRibbons,
                digital_init_level: 0,
                flags: 0,
                stats: None,
                payload: TrackPayloadSlice::StateRibbons(&ribbons[..rb_len]),
                aux_f32_0: 0.0,
                aux_f32_1: 0.0,
            },
        ];

        let scopes = [ScopePacketView {
            scope_id: 1,
            trigger_state: 3,
            mode: 2,
            win_t_start_ns: 1_000_000_000 + i as u64,
            win_t_span_ns: 10_000_000,
            subpixel_offset_ns: 2.5,
            tracks: &tracks,
        }];

        let snap_slice = &snapshots[..(i % 16)];

        let res = packer.pack_frame_borrowed(i as u32, 2_000_000_000, 0, &scopes, snap_slice);
        assert!(res.is_ok());

        // Verify pointer stability
        assert_eq!(
            packer.buffer().as_ptr(),
            baseline_ptr,
            "Pointer shifted during borrowed 10k test at iteration {i}"
        );
        assert_eq!(packer.capacity(), expected_cap);
    }

    GLOBAL_TRACKER.disable();

    assert_eq!(
        GLOBAL_TRACKER.allocs(),
        0,
        "CRITICAL: pack_frame_borrowed performed {} heap allocations across 10,000 frames!",
        GLOBAL_TRACKER.allocs()
    );
}

// ============================================================================
// Test 3: Capacity Expansion and High-Watermark Re-stabilization
// ============================================================================

#[test]
fn test_adversarial_capacity_expansion_and_restabilization() {
    let _guard = SERIAL_TEST_LOCK.lock().unwrap();

    // 1. Initialize with minimal capacity (4 KB)
    let mut packer = DdsfFramePacker::new(4096);
    let initial_cap = packer.capacity();
    assert!(initial_cap >= 4096);

    // Frame 1: Small frame (~1.5 KB), fits within 4 KB
    let small_scope = vec![make_test_scope(1, 50, 20, 10, 5)];
    let small_snaps = make_test_snapshots(4);

    let len0 = packer
        .pack_frame(1, 1000, 0, &small_scope, &small_snaps)
        .expect("pack small ok")
        .len();
    let ptr0 = packer.buffer().as_ptr();
    assert!(len0 < 4096);
    assert_eq!(packer.capacity(), initial_cap);

    // Verify small frame in steady state has 0 allocations
    GLOBAL_TRACKER.reset();
    GLOBAL_TRACKER.enable();
    for _ in 0..100 {
        packer
            .pack_frame(2, 2000, 0, &small_scope, &small_snaps)
            .expect("pack small steady ok");
        assert_eq!(packer.buffer().as_ptr(), ptr0);
    }
    GLOBAL_TRACKER.disable();
    assert_eq!(GLOBAL_TRACKER.allocs(), 0, "Steady small frames must have 0 allocs");

    // 2. Stage 1 Expansion: Exceed capacity with an ~18 KB frame
    let med_scope = vec![
        make_test_scope(1, 600, 300, 50, 20),
        make_test_scope(2, 400, 200, 30, 10),
    ];
    let med_snaps = make_test_snapshots(16);

    let len1 = packer
        .pack_frame(10, 3000, DDSF_FLAG_PINNED_ACTIVE, &med_scope, &med_snaps)
        .expect("pack medium expand ok")
        .len();
    let ptr1 = packer.buffer().as_ptr();
    let cap1 = packer.capacity();

    assert!(len1 > initial_cap, "Medium frame len ({len1}) must exceed initial cap ({initial_cap})");
    assert!(cap1 >= len1, "New capacity ({cap1}) must accommodate frame ({len1})");
    assert!(cap1 > initial_cap, "Capacity must have expanded");

    // Verify wire validity after expansion
    let reader1 = DdsfZeroCopyReader::new(packer.buffer()).expect("reader1 ok");
    assert_eq!(reader1.header().frame_seq, 10);
    assert_eq!(reader1.header().scope_count, 2);

    // 3. Stage 1 Re-stabilization: 1,000 continuous frames fluctuating UNDER cap1
    // Must perform EXACTLY ZERO allocations and pointer MUST remain ptr1
    GLOBAL_TRACKER.reset();
    GLOBAL_TRACKER.enable();

    for i in 0..1000 {
        // Vary between small and medium payloads (all <= len1)
        if i % 2 == 0 {
            packer
                .pack_frame(100 + i as u32, 4000, 0, &small_scope, &small_snaps)
                .expect("pack sub ok");
        } else {
            packer
                .pack_frame(100 + i as u32, 4000, 0, &med_scope, &med_snaps)
                .expect("pack med ok");
        }
        assert_eq!(
            packer.buffer().as_ptr(),
            ptr1,
            "Pointer must remain stable after first expansion at iteration {i}"
        );
        assert_eq!(packer.capacity(), cap1, "Capacity must remain fixed at cap1");
    }

    GLOBAL_TRACKER.disable();
    assert_eq!(
        GLOBAL_TRACKER.allocs(),
        0,
        "CRITICAL: Re-stabilized frames after expansion 1 caused {} allocations!",
        GLOBAL_TRACKER.allocs()
    );

    // 4. Stage 2 Expansion: Exceed cap1 with a ~120 KB frame
    let large_scope: Vec<ScopeViewportPayload> = (1..=4)
        .map(|id| make_test_scope(id, 2000, 1000, 100, 50))
        .collect();
    let large_snaps = make_test_snapshots(32);

    let len2 = packer
        .pack_frame(2000, 5000, DDSF_FLAG_QUOTA_WARNING, &large_scope, &large_snaps)
        .expect("pack large expand ok")
        .len();
    let ptr2 = packer.buffer().as_ptr();
    let cap2 = packer.capacity();

    assert!(len2 > cap1, "Large frame len ({len2}) must exceed cap1 ({cap1})");
    assert!(cap2 >= len2, "New capacity ({cap2}) must accommodate frame ({len2})");
    assert!(cap2 > cap1, "Capacity must have expanded a second time");

    let reader2 = DdsfZeroCopyReader::new(packer.buffer()).expect("reader2 ok");
    assert_eq!(reader2.header().frame_seq, 2000);
    assert_eq!(reader2.header().scope_count, 4);

    // 5. Stage 2 Re-stabilization: 1,000 continuous frames fluctuating UNDER cap2
    GLOBAL_TRACKER.reset();
    GLOBAL_TRACKER.enable();

    for i in 0..1000 {
        if i % 3 == 0 {
            packer
                .pack_frame(3000 + i as u32, 6000, 0, &small_scope, &small_snaps)
                .expect("sub ok");
        } else if i % 3 == 1 {
            packer
                .pack_frame(3000 + i as u32, 6000, 0, &med_scope, &med_snaps)
                .expect("med ok");
        } else {
            packer
                .pack_frame(3000 + i as u32, 6000, 0, &large_scope, &large_snaps)
                .expect("large ok");
        }
        assert_eq!(
            packer.buffer().as_ptr(),
            ptr2,
            "Pointer must remain stable after second expansion at iteration {i}"
        );
        assert_eq!(packer.capacity(), cap2, "Capacity must remain fixed at cap2");
    }

    GLOBAL_TRACKER.disable();
    assert_eq!(
        GLOBAL_TRACKER.allocs(),
        0,
        "CRITICAL: Re-stabilized frames after expansion 2 caused {} allocations!",
        GLOBAL_TRACKER.allocs()
    );
}

// ============================================================================
// Test 4: Memory Leak Absence & No Unbounded Growth across 50,000 Iterations
// ============================================================================

#[test]
fn test_adversarial_memory_leak_absence_50k_frames() {
    let _guard = SERIAL_TEST_LOCK.lock().unwrap();

    let mut packer = DdsfFramePacker::new(128 * 1024);
    let initial_cap = packer.capacity();

    let scope_a = vec![make_test_scope(1, 100, 50, 10, 5)];
    let scope_b = vec![make_test_scope(1, 800, 400, 40, 20)];
    let snaps = make_test_snapshots(8);

    // Stream 50,000 continuous frames
    for i in 0..50_000 {
        let scopes = if i % 2 == 0 { &scope_a } else { &scope_b };
        let slice_len = packer
            .pack_frame(i as u32, i as u64, 0, scopes, &snaps)
            .expect("pack ok")
            .len();

        // Verify buffer length dynamically shrinks and grows to exact frame size
        assert_eq!(packer.buffer().len(), slice_len);
        assert!(packer.buffer().len() <= initial_cap);

        // Verify capacity NEVER grows (no unbounded leakage)
        assert_eq!(
            packer.capacity(),
            initial_cap,
            "Capacity leaked/grew at iteration {i}!"
        );
    }
}

// ============================================================================
// Test 5: Odd Length Elements and 8-byte Alignment Padding
// ============================================================================

#[test]
fn test_adversarial_odd_length_alignment_padding_zero_alloc() {
    let _guard = SERIAL_TEST_LOCK.lock().unwrap();

    let mut packer = DdsfFramePacker::new(64 * 1024);

    // Test odd element lengths that are NOT multiples of 8 bytes:
    // - digital_edges: 1 float (4 bytes -> requires 4 bytes padding to 8)
    // - digital_edges: 3 floats (12 bytes -> requires 4 bytes padding to 16)
    // - digital_edges: 5 floats (20 bytes -> requires 4 bytes padding to 24)
    // - spectrum_bins: 1 float (4 bytes -> 4 bytes pad)
    let odd_edges_1 = vec![0.5f32];
    let odd_edges_3 = vec![0.1f32, 0.2, 0.3];
    let odd_edges_5 = vec![0.1f32, 0.2, 0.3, 0.4, 0.5];
    let odd_bins_1 = vec![440.0f32];

    let scope = ScopeViewportPayload {
        scope_id: 1,
        trigger_state: 1,
        mode: 1,
        win_t_start_ns: 0,
        win_t_span_ns: 1000,
        subpixel_offset_ns: 0.0,
        tracks: vec![
            TrackPayload::new_digital_edges(10, 0, odd_edges_1),
            TrackPayload::new_digital_edges(11, 1, odd_edges_3),
            TrackPayload::new_digital_edges(12, 0, odd_edges_5),
            TrackPayload::new_spectrum(13, 0.0, 10.0, odd_bins_1),
        ],
    };

    let snaps = vec![DdsfSnapshotEntry {
        channel_num_id: 1,
        kind: DdsfSnapshotKind::Scalar.as_u8(),
        pad: [0; 3],
        val_f64: 42.0,
    }];

    let scope_slice = [scope];

    // Pre-warm
    packer
        .pack_frame(1, 1000, 0, &scope_slice, &snaps)
        .expect("prewarm ok");

    // Under tracking allocator, verify 0 allocations with odd lengths
    GLOBAL_TRACKER.reset();
    GLOBAL_TRACKER.enable();

    for i in 0..1000 {
        let packed_len = packer
            .pack_frame(i as u32, 1000, 0, &scope_slice, &snaps)
            .expect("pack ok")
            .len();
        assert_eq!(packed_len % 8, 0, "DDSF frame total length must be 8-byte aligned");
    }

    GLOBAL_TRACKER.disable();
    assert_eq!(
        GLOBAL_TRACKER.allocs(),
        0,
        "Odd alignment padding must perform 0 allocations"
    );

    // Verify zero-copy reader correctly reads each odd-padded track without unaligned errors
    let packed = packer.buffer();
    let reader = DdsfZeroCopyReader::new(packed).expect("reader parse ok");
    let scope_views: Vec<_> = reader.scopes().collect();
    assert_eq!(scope_views.len(), 1);

    let track_views: Vec<_> = scope_views[0].tracks().collect();
    assert_eq!(track_views.len(), 4);

    assert_eq!(track_views[0].header().element_count, 1);
    assert_eq!(track_views[0].header().data_byte_len, 4);
    assert_eq!(track_views[0].header().block_byte_len, 32 + 8); // 32 header + 8 aligned data

    assert_eq!(track_views[1].header().element_count, 3);
    assert_eq!(track_views[1].header().data_byte_len, 12);
    assert_eq!(track_views[1].header().block_byte_len, 32 + 16);

    assert_eq!(track_views[2].header().element_count, 5);
    assert_eq!(track_views[2].header().data_byte_len, 20);
    assert_eq!(track_views[2].header().block_byte_len, 32 + 24);

    assert_eq!(track_views[3].header().element_count, 1);
    assert_eq!(track_views[3].header().data_byte_len, 4);
    assert_eq!(track_views[3].header().block_byte_len, 32 + 8);

    // Verify snapshots slice at the tail
    let parsed_snaps = reader.snapshots().expect("snapshots ok");
    assert_eq!(parsed_snaps.len(), 1);
    assert_eq!(parsed_snaps[0].val_f64, 42.0);
}

// ============================================================================
// Test 6: Zero-Copy Deserialization Reader Zero Allocation Invariant
// ============================================================================

#[test]
fn test_adversarial_zero_copy_reader_zero_allocation() {
    let _guard = SERIAL_TEST_LOCK.lock().unwrap();

    let mut packer = DdsfFramePacker::new(128 * 1024);
    let scopes: Vec<ScopeViewportPayload> = (1..=3)
        .map(|id| make_test_scope(id, 1000, 500, 50, 20))
        .collect();
    let snapshots = make_test_snapshots(16);

    packer
        .pack_frame(777, 1_234_567_890, DDSF_FLAG_PINNED_ACTIVE, &scopes, &snapshots)
        .expect("pack ok");
    let packed_bytes = packer.buffer();

    // Under tracking allocator, parse reader and inspect all scopes, tracks, and slices
    GLOBAL_TRACKER.reset();
    GLOBAL_TRACKER.enable();

    for _ in 0..1000 {
        let reader = DdsfZeroCopyReader::new(packed_bytes).expect("reader new ok");
        let header = reader.header();
        assert_eq!(header.magic, DDSF_MAGIC);
        assert_eq!(header.version, DDSF_VERSION_V1);
        assert_eq!(header.frame_seq, 777);
        assert_eq!(header.scope_count, 3);
        assert_eq!(header.snapshot_count, 16);

        let mut scope_count = 0;
        for scope in reader.scopes() {
            scope_count += 1;
            let mut track_count = 0;
            for track in scope.tracks() {
                track_count += 1;
                match DdsfBlockKind::from_u8(track.header().block_kind).unwrap() {
                    DdsfBlockKind::AnalogRawXY => {
                        let pts = track.raw_points().expect("raw pts ok");
                        assert_eq!(pts.len(), 1000);
                    }
                    DdsfBlockKind::AnalogM4Column => {
                        let cols = track.m4_columns().expect("m4 ok");
                        assert_eq!(cols.len(), 500);
                        assert!(track.stats().is_some());
                    }
                    DdsfBlockKind::DigitalEdges => {
                        assert_eq!(track.header().element_count, 50);
                    }
                    DdsfBlockKind::StateRibbons => {
                        assert_eq!(track.header().element_count, 20);
                    }
                    DdsfBlockKind::SpectrumFrame => {}
                }
            }
            assert_eq!(track_count, 4);
        }
        assert_eq!(scope_count, 3);

        let snaps = reader.snapshots().expect("snaps ok");
        assert_eq!(snaps.len(), 16);
    }

    GLOBAL_TRACKER.disable();

    assert_eq!(
        GLOBAL_TRACKER.allocs(),
        0,
        "CRITICAL: DdsfZeroCopyReader performed {} heap allocations!",
        GLOBAL_TRACKER.allocs()
    );
}

// ============================================================================
// Test 7: Extreme Boundary & Degenerate Frame Handling
// ============================================================================

#[test]
fn test_adversarial_degenerate_and_empty_frames() {
    let _guard = SERIAL_TEST_LOCK.lock().unwrap();

    let mut packer = DdsfFramePacker::new(4096);

    // 1. Completely empty frame: 0 scopes, 0 snapshots
    let empty_len = packer
        .pack_frame(0, 0, 0, &[], &[])
        .expect("empty pack ok")
        .len();
    assert_eq!(empty_len, DDSF_HEADER_BYTES); // Exactly 64 bytes
    let reader = DdsfZeroCopyReader::new(packer.buffer()).expect("reader ok");
    assert_eq!(reader.header().scope_count, 0);
    assert_eq!(reader.header().snapshot_count, 0);
    assert_eq!(reader.scopes().count(), 0);
    assert_eq!(reader.snapshots().expect("snaps ok").len(), 0);

    // 2. Scope with 0 tracks
    let empty_scope = ScopeViewportPayload {
        scope_id: 1,
        trigger_state: 0,
        mode: 0,
        win_t_start_ns: 0,
        win_t_span_ns: 0,
        subpixel_offset_ns: 0.0,
        tracks: vec![],
    };
    let packed_empty_scope_len = packer
        .pack_frame(1, 0, 0, &[empty_scope], &[])
        .expect("empty scope pack ok")
        .len();
    assert_eq!(
        packed_empty_scope_len,
        DDSF_HEADER_BYTES + 32 // 64 header + 32 scope header
    );
    let reader_es = DdsfZeroCopyReader::new(packer.buffer()).expect("reader ok");
    let mut scopes_iter = reader_es.scopes();
    let sc1 = scopes_iter.next().unwrap();
    assert_eq!(sc1.tracks().count(), 0);
    assert!(scopes_iter.next().is_none());

    // 3. 50 empty scopes
    let fifty_scopes: Vec<ScopeViewportPayload> = (0..50)
        .map(|id| ScopeViewportPayload {
            scope_id: id,
            trigger_state: 0,
            mode: 0,
            win_t_start_ns: 0,
            win_t_span_ns: 0,
            subpixel_offset_ns: 0.0,
            tracks: vec![],
        })
        .collect();
    let packed_50_len = packer
        .pack_frame(2, 0, 0, &fifty_scopes, &[])
        .expect("pack 50 ok")
        .len();
    assert_eq!(
        packed_50_len,
        DDSF_HEADER_BYTES + 50 * 32
    );
    let reader_50 = DdsfZeroCopyReader::new(packer.buffer()).expect("reader ok");
    assert_eq!(reader_50.header().scope_count, 50);
    assert_eq!(reader_50.scopes().count(), 50);

    // 4. Extreme floating point snapshot values (NaN, +Inf, -Inf, subnormal, MAX, MIN)
    let extreme_snaps = vec![
        DdsfSnapshotEntry { channel_num_id: 0, kind: 0, pad: [0; 3], val_f64: f64::NAN },
        DdsfSnapshotEntry { channel_num_id: 1, kind: 0, pad: [0; 3], val_f64: f64::INFINITY },
        DdsfSnapshotEntry { channel_num_id: 2, kind: 0, pad: [0; 3], val_f64: f64::NEG_INFINITY },
        DdsfSnapshotEntry { channel_num_id: 3, kind: 0, pad: [0; 3], val_f64: 0.0 },
        DdsfSnapshotEntry { channel_num_id: 4, kind: 0, pad: [0; 3], val_f64: -0.0 },
        DdsfSnapshotEntry { channel_num_id: 5, kind: 0, pad: [0; 3], val_f64: f64::MIN_POSITIVE },
        DdsfSnapshotEntry { channel_num_id: 6, kind: 0, pad: [0; 3], val_f64: f64::MAX },
        DdsfSnapshotEntry { channel_num_id: 7, kind: 0, pad: [0; 3], val_f64: f64::MIN },
    ];
    packer
        .pack_frame(3, 0, 0, &[], &extreme_snaps)
        .expect("extreme snaps pack ok");
    let reader_ex = DdsfZeroCopyReader::new(packer.buffer()).expect("reader ok");
    let parsed_ex = reader_ex.snapshots().expect("snaps ok");
    assert_eq!(parsed_ex.len(), 8);
    assert!(parsed_ex[0].val_f64.is_nan());
    assert_eq!(parsed_ex[1].val_f64, f64::INFINITY);
    assert_eq!(parsed_ex[2].val_f64, f64::NEG_INFINITY);
    assert_eq!(parsed_ex[3].val_f64, 0.0);
    assert_eq!(parsed_ex[4].val_f64, -0.0);
    assert_eq!(parsed_ex[5].val_f64, f64::MIN_POSITIVE);
    assert_eq!(parsed_ex[6].val_f64, f64::MAX);
    assert_eq!(parsed_ex[7].val_f64, f64::MIN);
}
