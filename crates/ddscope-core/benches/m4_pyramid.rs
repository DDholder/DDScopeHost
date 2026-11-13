//! Criterion performance benchmark harness for 64-ary M4 Envelope Pyramid.
//!
//! Validates sub-0.1ms extraction latency over 100M points downsampled to 2,000 pixels.

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};

use ddscope_core::storage::NativeScalarSeries;
use ddscope_core::types::{
    ChannelId, LinearScale, ScalarSlice, ScalarType, CHUNK_LEN,
};

const NUM_CHUNKS: usize = 1_526; // 1,526 chunks * 65,536 = 100,007,936 samples (~100M)
const VIEWPORT_PX: usize = 2_000;
const SAMPLING_INTERVAL_NS: u64 = 10; // 100 MSa/s (10 ns per sample)

fn build_100m_series() -> NativeScalarSeries {
    let mut series = NativeScalarSeries::new(
        ChannelId(1),
        ScalarType::F32,
        LinearScale::new(1.0, 0.0),
    );

    let batch_size = CHUNK_LEN;
    let mut ts_buf = Vec::with_capacity(batch_size);
    let mut val_buf = Vec::with_capacity(batch_size);

    for chunk_idx in 0..NUM_CHUNKS {
        ts_buf.clear();
        val_buf.clear();
        let base_t = (chunk_idx * batch_size) as u64 * SAMPLING_INTERVAL_NS;

        for i in 0..batch_size {
            let t = base_t + (i as u64) * SAMPLING_INTERVAL_NS;
            let val = ((chunk_idx * batch_size + i) as f32 * 0.001).sin();
            ts_buf.push(t);
            val_buf.push(val);
        }

        // Insert isolated test spike in chunk 500
        if chunk_idx == 500 {
            val_buf[1234] = 99_999.0;
        }

        series
            .append(&ts_buf, ScalarSlice::F32(&val_buf))
            .expect("batch append ok");
    }

    series
}

fn bench_m4_pyramid_queries(c: &mut Criterion) {
    let mut group = c.benchmark_group("m4_pyramid_query");
    group.throughput(Throughput::Elements(VIEWPORT_PX as u64));

    println!("Setting up 100M points series for M4 extraction benchmark...");
    let series = build_100m_series();
    let (t_start, t_end) = series.time_range_ns().expect("valid time range");
    println!(
        "Ingested {} samples ({} sealed chunks). Time span: {} ns to {} ns",
        series.total_samples(),
        series.sealed_chunk_count(),
        t_start,
        t_end
    );

    // Warm-up and validation check
    let res = series
        .query_m4_viewport(t_start, t_end, VIEWPORT_PX)
        .expect("query ok");
    if let ddscope_core::storage::M4QueryResult::Columns(cols) = &res {
        assert_eq!(cols.len(), VIEWPORT_PX);
        let mut peak_found = false;
        for c in cols {
            if (c.v_max - 99_999.0).abs() < 1e-3 {
                peak_found = true;
                break;
            }
        }
        assert!(peak_found, "Single-sample peak must be retained in 100M query");
    } else {
        panic!("Expected Columns query result for 100M points");
    }

    // Benchmark 1: Full 100M view downsampled to 2,000 px (< 0.1 ms SLA target)
    group.bench_function("query_100m_points_2000px", |b| {
        b.iter(|| {
            black_box(
                series
                    .query_m4_viewport(t_start, t_end, VIEWPORT_PX)
                    .expect("query ok"),
            );
        });
    });

    // Benchmark 2: 10M view (10x zoom)
    let t_10m = t_start + 10_000_000 * SAMPLING_INTERVAL_NS;
    group.bench_function("query_10m_points_2000px", |b| {
        b.iter(|| {
            black_box(
                series
                    .query_m4_viewport(t_start, t_10m, VIEWPORT_PX)
                    .expect("query ok"),
            );
        });
    });

    // Benchmark 3: 1M view (100x zoom, L1 level)
    let t_1m = t_start + 1_000_000 * SAMPLING_INTERVAL_NS;
    group.bench_function("query_1m_points_2000px", |b| {
        b.iter(|| {
            black_box(
                series
                    .query_m4_viewport(t_start, t_1m, VIEWPORT_PX)
                    .expect("query ok"),
            );
        });
    });

    // Benchmark 4: 100k view (1,000x zoom)
    let t_100k = t_start + 100_000 * SAMPLING_INTERVAL_NS;
    group.bench_function("query_100k_points_2000px", |b| {
        b.iter(|| {
            black_box(
                series
                    .query_m4_viewport(t_start, t_100k, VIEWPORT_PX)
                    .expect("query ok"),
            );
        });
    });

    // Benchmark 5: 4k view (Deep zoom Raw points mode)
    let t_4k = t_start + 4_000 * SAMPLING_INTERVAL_NS;
    group.bench_function("query_raw_4k_points_2000px", |b| {
        b.iter(|| {
            black_box(
                series
                    .query_m4_viewport(t_start, t_4k, VIEWPORT_PX)
                    .expect("query ok"),
            );
        });
    });

    group.finish();
}

criterion_group!(benches, bench_m4_pyramid_queries);
criterion_main!(benches);
