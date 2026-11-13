//! Criterion performance benchmark harness for `NativeScalarSeries::append`.
//!
//! Measures batch append throughput across representative scalar types (`f32`, `i16`, `u32`)
//! and batch sizes (`1_024`, `4_096`, `16_384`, `65_536`).
//!
//! Target throughput: > 10,000,000 samples/sec.

use criterion::{
    black_box, criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput,
};
use ddscope_core::storage::NativeScalarSeries;
use ddscope_core::types::{ChannelId, LinearScale, ScalarSlice, ScalarType, TimestampNs};

/// Representative batch sizes matching acquisition frames and chunk boundaries.
const BATCH_SIZES: [usize; 4] = [1_024, 4_096, 16_384, 65_536];

/// Nominal 100 MSa/s sampling interval (10 ns between samples).
const SAMPLING_INTERVAL_NS: u64 = 10;

/// Benchmarks batch append throughput on `NativeScalarSeries`.
fn bench_storage_append(c: &mut Criterion) {
    let mut group = c.benchmark_group("storage_append");

    for &batch_size in &BATCH_SIZES {
        group.throughput(Throughput::Elements(batch_size as u64));

        // Pre-generate monotonic timestamps outside the timing loop
        let timestamps: Vec<TimestampNs> = (0..batch_size as u64)
            .map(|i| i * SAMPLING_INTERVAL_NS)
            .collect();

        // 1. Benchmark: f32 (IEEE-754 single precision)
        {
            let values: Vec<f32> = (0..batch_size).map(|i| (i as f32) * 0.01).collect();

            group.bench_with_input(
                BenchmarkId::new("f32", batch_size),
                &batch_size,
                |b, &_size| {
                    b.iter_batched_ref(
                        || {
                            NativeScalarSeries::new(
                                ChannelId(1),
                                ScalarType::F32,
                                LinearScale::identity(),
                            )
                        },
                        |series| {
                            black_box(
                                series
                                    .append(&timestamps, ScalarSlice::F32(&values))
                                    .expect("append f32 failed"),
                            );
                        },
                        BatchSize::LargeInput,
                    );
                },
            );
        }

        // 2. Benchmark: i16 (16-bit signed integer / raw ADC data)
        {
            let values: Vec<i16> = (0..batch_size).map(|i| (i % 32767) as i16).collect();

            group.bench_with_input(
                BenchmarkId::new("i16", batch_size),
                &batch_size,
                |b, &_size| {
                    b.iter_batched_ref(
                        || {
                            NativeScalarSeries::new(
                                ChannelId(1),
                                ScalarType::I16,
                                LinearScale::identity(),
                            )
                        },
                        |series| {
                            black_box(
                                series
                                    .append(&timestamps, ScalarSlice::I16(&values))
                                    .expect("append i16 failed"),
                            );
                        },
                        BatchSize::LargeInput,
                    );
                },
            );
        }

        // 3. Benchmark: u32 (32-bit unsigned integer / counter & bus data)
        {
            let values: Vec<u32> = (0..batch_size).map(|i| i as u32).collect();

            group.bench_with_input(
                BenchmarkId::new("u32", batch_size),
                &batch_size,
                |b, &_size| {
                    b.iter_batched_ref(
                        || {
                            NativeScalarSeries::new(
                                ChannelId(1),
                                ScalarType::U32,
                                LinearScale::identity(),
                            )
                        },
                        |series| {
                            black_box(
                                series
                                    .append(&timestamps, ScalarSlice::U32(&values))
                                    .expect("append u32 failed"),
                            );
                        },
                        BatchSize::LargeInput,
                    );
                },
            );
        }
    }

    group.finish();
}

criterion_group!(benches, bench_storage_append);
criterion_main!(benches);
