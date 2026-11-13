//! Criterion performance and zero-allocation benchmark harness for DDSF v1 Frame Packer.
//!
//! Measures packing throughput (frames/sec and GB/s) across multi-scope configurations
//! (1, 2, 4 scopes) with mixed signal tracks and channel latest snapshots.
//!
//! Invariant: ZERO secondary heap allocations in steady-state streaming.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use ddscope_core::scope::ddsf::{
    DdsfAnalogPoint, DdsfFramePacker, DdsfM4Column, DdsfSnapshotEntry, DdsfStateRibbon,
    DdsfTrackStats, ScopeViewportPayload, TrackPayload, DDSF_FLAG_PINNED_ACTIVE,
};

/// Global counting allocator hook for validating zero allocations in benchmarks.
struct BenchTrackingAllocator {
    enabled: AtomicBool,
    alloc_count: AtomicUsize,
}

unsafe impl GlobalAlloc for BenchTrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if self.enabled.load(Ordering::Acquire) {
            self.alloc_count.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: Delegating directly to the host system allocator.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: Delegating directly to the host system allocator.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if self.enabled.load(Ordering::Acquire) {
            self.alloc_count.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: Delegating directly to the host system allocator.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static BENCH_ALLOCATOR: BenchTrackingAllocator = BenchTrackingAllocator {
    enabled: AtomicBool::new(false),
    alloc_count: AtomicUsize::new(0),
};

/// Helper: creates a representative mixed-signal scope viewport payload.
fn build_mixed_scope(
    scope_id: u32,
    raw_pts_len: usize,
    m4_cols_len: usize,
    edges_len: usize,
    ribbons_len: usize,
) -> ScopeViewportPayload {
    let raw_points: Vec<DdsfAnalogPoint> = (0..raw_pts_len)
        .map(|i| DdsfAnalogPoint {
            x_norm: (i as f32) / (raw_pts_len as f32),
            y_val: ((i as f32) * 0.05).sin(),
        })
        .collect();

    let m4_columns: Vec<DdsfM4Column> = (0..m4_cols_len)
        .map(|i| DdsfM4Column {
            x_norm: (i as f32) / (m4_cols_len as f32),
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
        .map(|i| (i as f32) / (edges_len as f32))
        .collect();

    let state_ribbons: Vec<DdsfStateRibbon> = (0..ribbons_len)
        .map(|i| DdsfStateRibbon {
            x0_norm: (i as f32) / (ribbons_len as f32),
            x1_norm: ((i + 1) as f32) / (ribbons_len as f32),
            state_id: (i % 4) as i32,
            color_idx: (i % 8) as u32,
        })
        .collect();

    ScopeViewportPayload {
        scope_id,
        trigger_state: 3, // Triggered
        mode: 2,          // Normal
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

/// Helper: creates 16 channel latest snapshots.
fn build_snapshots(count: usize) -> Vec<DdsfSnapshotEntry> {
    (0..count)
        .map(|i| DdsfSnapshotEntry {
            channel_num_id: i as u32,
            kind: (i % 3) as u8,
            pad: [0; 3],
            val_f64: (i as f64) * std::f64::consts::PI,
        })
        .collect()
}

fn bench_ddsf_pack(c: &mut Criterion) {
    let mut group = c.benchmark_group("ddsf_pack_mixed_frames");

    let snapshots = build_snapshots(16);
    let scope_counts = [1, 2, 4];

    for &num_scopes in &scope_counts {
        let scopes: Vec<ScopeViewportPayload> = (0..num_scopes)
            .map(|id| build_mixed_scope(id + 1, 2000, 1000, 100, 50))
            .collect();

        // Pre-allocate packer with plenty of capacity (256 KB)
        let mut packer = DdsfFramePacker::new(256 * 1024);

        // Pre-warm and determine exact frame byte length
        let prewarmed = packer
            .pack_frame(1, 1_000_000_000, DDSF_FLAG_PINNED_ACTIVE, &scopes, &snapshots)
            .expect("pack failed");
        let frame_byte_len = prewarmed.len();

        // 1. Verify ZERO heap allocations in steady state
        BENCH_ALLOCATOR.alloc_count.store(0, Ordering::Release);
        BENCH_ALLOCATOR.enabled.store(true, Ordering::Release);
        let _ = packer
            .pack_frame(2, 1_000_000_000, DDSF_FLAG_PINNED_ACTIVE, &scopes, &snapshots)
            .expect("pack failed");
        let allocs = BENCH_ALLOCATOR.alloc_count.load(Ordering::Acquire);
        BENCH_ALLOCATOR.enabled.store(false, Ordering::Release);
        assert_eq!(
            allocs, 0,
            "CRITICAL: steady-state pack_frame performed {allocs} heap allocations!"
        );

        // 2. Throughput benchmark (Bytes)
        group.throughput(Throughput::Bytes(frame_byte_len as u64));

        group.bench_with_input(
            BenchmarkId::new(format!("{num_scopes}_scopes_mixed"), frame_byte_len),
            &num_scopes,
            |b, &_n| {
                b.iter(|| {
                    let bytes = packer
                        .pack_frame(
                            black_box(42),
                            black_box(1_000_000_000),
                            black_box(DDSF_FLAG_PINNED_ACTIVE),
                            black_box(&scopes),
                            black_box(&snapshots),
                        )
                        .expect("pack ok");
                    black_box(bytes);
                });
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_ddsf_pack);
criterion_main!(benches);
