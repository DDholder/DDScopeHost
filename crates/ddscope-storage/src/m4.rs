//! 64-ary Multi-Level M4 Envelope Pyramid & Viewport Decimation Extraction.
//!
//! Provides SIMD-accelerated M4 envelope construction across all 10 native scalar types
//! (`i8, i16, i32, i64, u8, u16, u32, u64, f32, f64`), multi-level pyramid indexing (L1 through L4),
//! and sub-0.1ms viewport extraction queries with 100% single-sample peak and valley retention.

use ddscope_scope::ddsf::{DdsfAnalogPoint, DdsfM4Column, DdsfTrackStats};
use ddscope_types::{AlignedVec, M4Bucket, TimestampNs, M4_BRANCH_FACTOR};

// Re-export intra-chunk types and reduction functions from `ddscope_types::chunk`
pub use ddscope_types::chunk::{
    reduce_64_buckets, reduce_bucket_slice, reduce_column_slice, reduce_raw_bool_slice,
    reduce_raw_f32_slice, reduce_raw_f64_slice, reduce_raw_i16_slice, reduce_raw_i32_slice,
    reduce_raw_i64_slice, reduce_raw_i8_slice, reduce_raw_u16_slice, reduce_raw_u32_slice,
    reduce_raw_u64_slice, reduce_raw_u8_slice, ChunkM4Levels,
};

/// Result of a pixel-aligned viewport downsampling query.
#[derive(Clone, Debug, PartialEq)]
pub enum M4QueryResult {
    /// Exact normalized sample points when sample count N <= 2 * W_px.
    Raw(Vec<DdsfAnalogPoint>),
    /// Decimated M4 columns when sample count N > 2 * W_px.
    Columns(Vec<DdsfM4Column>),
}

/// Type alias for render data compatibility.
pub type ViewportRenderData = M4QueryResult;

/// Viewport extraction bundle combining render data and track statistics.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewportExtract {
    pub result: M4QueryResult,
    pub stats: DdsfTrackStats,
}

/// Series-level M4 envelope pyramid managing inter-chunk levels (L3, L4).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MultiLevelM4Pyramid {
    /// Level 3 buckets: step 262,144 samples (4 sealed chunks = 64 L2 buckets).
    pub l3: AlignedVec<M4Bucket>,
    /// Level 4 buckets: step 16,777,216 samples (256 sealed chunks = 64 L3 buckets).
    pub l4: AlignedVec<M4Bucket>,
    /// Temporary accumulator for up to 64 L2 buckets awaiting aggregation into an L3 bucket.
    pub accum_l2: Vec<M4Bucket>,
    /// Temporary accumulator for up to 64 L3 buckets awaiting aggregation into an L4 bucket.
    pub accum_l3: Vec<M4Bucket>,
}

impl MultiLevelM4Pyramid {
    /// Creates a new empty inter-chunk pyramid.
    pub fn new() -> Self {
        Self {
            l3: AlignedVec::with_capacity(64),
            l4: AlignedVec::with_capacity(16),
            accum_l2: Vec::with_capacity(M4_BRANCH_FACTOR),
            accum_l3: Vec::with_capacity(M4_BRANCH_FACTOR),
        }
    }

    /// Ingests L2 buckets from a newly sealed chunk into the pyramid hierarchy.
    pub fn append_chunk_l2(&mut self, chunk_l2: &[M4Bucket]) {
        for &bucket in chunk_l2 {
            self.accum_l2.push(bucket);
            if self.accum_l2.len() == M4_BRANCH_FACTOR {
                let child_array: &[M4Bucket; 64] = self.accum_l2[..64]
                    .try_into()
                    .expect("exact 64-element slice");
                let l3_bucket = reduce_64_buckets(child_array);
                self.l3.push(l3_bucket);
                self.accum_l2.clear();

                // Propagate to Level 4
                self.accum_l3.push(l3_bucket);
                if self.accum_l3.len() == M4_BRANCH_FACTOR {
                    let child_array_l3: &[M4Bucket; 64] = self.accum_l3[..64]
                        .try_into()
                        .expect("exact 64-element slice");
                    let l4_bucket = reduce_64_buckets(child_array_l3);
                    self.l4.push(l4_bucket);
                    self.accum_l3.clear();
                }
            }
        }
    }

    /// Clears the pyramid and all accumulators.
    pub fn clear(&mut self) {
        self.l3.clear();
        self.l4.clear();
        self.accum_l2.clear();
        self.accum_l3.clear();
    }

    /// Prunes buckets that are fully older than `oldest_retained_ts` (i.e. `t_last_ns < oldest_retained_ts`).
    pub fn prune_older_than(&mut self, oldest_retained_ts: TimestampNs) {
        let drop_l3 = self
            .l3
            .as_slice()
            .partition_point(|b| b.t_last_ns < oldest_retained_ts);
        self.l3.drain_prefix(drop_l3);

        let drop_l4 = self
            .l4
            .as_slice()
            .partition_point(|b| b.t_last_ns < oldest_retained_ts);
        self.l4.drain_prefix(drop_l4);

        self.accum_l3.retain(|b| b.t_last_ns >= oldest_retained_ts);
        self.accum_l2.retain(|b| b.t_last_ns >= oldest_retained_ts);
    }
}

// ---------------------------------------------------------------------------
// Viewport Pixel Column Aggregation
// ---------------------------------------------------------------------------

/// Lightweight single-pixel accumulator slot for zero-allocation decimation.
#[derive(Clone, Copy, Debug)]
pub struct PixelSlot {
    pub v_first: f32,
    pub v_min: f32,
    pub v_max: f32,
    pub v_last: f32,
    pub has_data: bool,
}

impl PixelSlot {
    pub const EMPTY: Self = Self {
        v_first: 0.0,
        v_min: f32::INFINITY,
        v_max: f32::NEG_INFINITY,
        v_last: 0.0,
        has_data: false,
    };
}

/// Accumulates a single [`M4Bucket`] directly into a pixel column slot.
#[inline(always)]
pub fn feed_bucket_into_slots(
    slots: &mut [PixelSlot],
    b: &M4Bucket,
    t_min: TimestampNs,
    t_max: TimestampNs,
    inv_dt_pixel: f64,
    w_px: usize,
) {
    if b.t_last_ns < t_min || b.t_first_ns > t_max {
        return;
    }

    let t_mid = b.t_first_ns + (b.t_last_ns.saturating_sub(b.t_first_ns)) / 2;
    let p = (((t_mid.saturating_sub(t_min)) as f64 * inv_dt_pixel) as usize).min(w_px - 1);
    let slot = &mut slots[p];

    if !slot.has_data {
        slot.v_first = b.v_first;
        slot.v_min = b.v_min;
        slot.v_max = b.v_max;
        slot.v_last = b.v_last;
        slot.has_data = true;
    } else {
        slot.v_min = slot.v_min.min(b.v_min);
        slot.v_max = slot.v_max.max(b.v_max);
        slot.v_last = b.v_last;
    }
}

/// Resolves pixel column continuity via zero-order-hold (ZOH) carry-forward and produces [`DdsfM4Column`] output.
pub fn finish_pixel_slots(slots: &mut [PixelSlot], w_px: usize) -> Vec<DdsfM4Column> {
    if w_px == 0 {
        return Vec::new();
    }

    // Forward carry-over (ZOH) for empty intervals
    let mut last_valid = 0.0_f32;
    let mut found_first = false;
    for slot in slots.iter_mut().take(w_px) {
        if slot.has_data {
            last_valid = slot.v_last;
            found_first = true;
        } else if found_first {
            slot.v_first = last_valid;
            slot.v_min = last_valid;
            slot.v_max = last_valid;
            slot.v_last = last_valid;
        }
    }

    // Backfill leading empty columns
    if found_first {
        if let Some(first_slot) = slots.iter().take(w_px).find(|s| s.has_data) {
            let first_val = first_slot.v_first;
            for slot in slots.iter_mut().take(w_px) {
                if slot.has_data {
                    break;
                }
                slot.v_first = first_val;
                slot.v_min = first_val;
                slot.v_max = first_val;
                slot.v_last = first_val;
            }
        }
    }

    let mut columns = Vec::with_capacity(w_px);
    let inv_w = 1.0 / (w_px as f32);
    for (p, slot) in slots.iter().enumerate().take(w_px) {
        let x_norm = (p as f32 + 0.5) * inv_w;
        columns.push(DdsfM4Column {
            x_norm,
            v_first: slot.v_first,
            v_min: if slot.v_min.is_infinite() {
                0.0
            } else {
                slot.v_min
            },
            v_max: if slot.v_max.is_infinite() {
                0.0
            } else {
                slot.v_max
            },
            v_last: slot.v_last,
        });
    }

    columns
}

/// Aggregates an ordered slice of [`M4Bucket`] structures into exactly `w_px` horizontal pixel columns.
///
/// Implements zero-order-hold (ZOH) carry-forward to maintain waveform continuity across
/// temporal discretization gaps.
pub fn aggregate_buckets_to_columns(
    buckets: &[M4Bucket],
    t_min: TimestampNs,
    t_max: TimestampNs,
    w_px: usize,
) -> Vec<DdsfM4Column> {
    if w_px == 0 || t_max <= t_min {
        return Vec::new();
    }

    let inv_dt_pixel = (w_px as f64) / ((t_max - t_min) as f64);
    let mut slots = vec![PixelSlot::EMPTY; w_px];

    for b in buckets {
        feed_bucket_into_slots(&mut slots, b, t_min, t_max, inv_dt_pixel, w_px);
    }

    finish_pixel_slots(&mut slots, w_px)
}

/// Computes real-time 32-byte [`DdsfTrackStats`] across decimated pixel columns.
pub fn compute_track_stats_from_columns(columns: &[DdsfM4Column]) -> DdsfTrackStats {
    if columns.is_empty() {
        return DdsfTrackStats::default();
    }

    let mut min_val = f32::INFINITY;
    let mut max_val = f32::NEG_INFINITY;
    let mut sum = 0.0_f64;
    let mut sum_sq = 0.0_f64;

    for col in columns {
        min_val = min_val.min(col.v_min);
        max_val = max_val.max(col.v_max);
        let mid = (col.v_min + col.v_max) * 0.5;
        sum += mid as f64;
        sum_sq += (mid as f64) * (mid as f64);
    }

    let n = columns.len() as f64;
    let mean = (sum / n) as f32;
    let rms = ((sum_sq / n).max(0.0).sqrt()) as f32;
    let vpp = (max_val - min_val).max(0.0);

    // Approximate duty and frequency via mid-level crossings
    let mid_threshold = (min_val + max_val) * 0.5;
    let mut above_mid_count = 0;
    let mut crossings = 0;
    let mut prev_above = columns[0].v_last >= mid_threshold;

    for col in columns {
        let is_above = col.v_last >= mid_threshold;
        if is_above {
            above_mid_count += 1;
        }
        if is_above != prev_above {
            crossings += 1;
            prev_above = is_above;
        }
    }

    let duty = (above_mid_count as f32) / (columns.len() as f32);
    let cycles = (crossings as f32) * 0.5;

    DdsfTrackStats {
        min: if min_val.is_infinite() { 0.0 } else { min_val },
        max: if max_val.is_infinite() { 0.0 } else { max_val },
        vpp,
        mean,
        rms,
        freq_hz: cycles,
        duty,
        reserved: 0.0,
    }
}

/// Computes real-time 32-byte [`DdsfTrackStats`] across raw sample points.
pub fn compute_track_stats_from_raw(points: &[DdsfAnalogPoint]) -> DdsfTrackStats {
    if points.is_empty() {
        return DdsfTrackStats::default();
    }

    let mut min_val = f32::INFINITY;
    let mut max_val = f32::NEG_INFINITY;
    let mut sum = 0.0_f64;
    let mut sum_sq = 0.0_f64;

    for pt in points {
        min_val = min_val.min(pt.y_val);
        max_val = max_val.max(pt.y_val);
        sum += pt.y_val as f64;
        sum_sq += (pt.y_val as f64) * (pt.y_val as f64);
    }

    let n = points.len() as f64;
    let mean = (sum / n) as f32;
    let rms = ((sum_sq / n).max(0.0).sqrt()) as f32;
    let vpp = (max_val - min_val).max(0.0);

    let mid_threshold = (min_val + max_val) * 0.5;
    let mut above_count = 0;
    let mut crossings = 0;
    let mut prev_above = points[0].y_val >= mid_threshold;

    for pt in points {
        let is_above = pt.y_val >= mid_threshold;
        if is_above {
            above_count += 1;
        }
        if is_above != prev_above {
            crossings += 1;
            prev_above = is_above;
        }
    }

    let duty = (above_count as f32) / (points.len() as f32);
    let cycles = (crossings as f32) * 0.5;

    DdsfTrackStats {
        min: if min_val.is_infinite() { 0.0 } else { min_val },
        max: if max_val.is_infinite() { 0.0 } else { max_val },
        vpp,
        mean,
        rms,
        freq_hz: cycles,
        duty,
        reserved: 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddscope_types::LinearScale;

    #[test]
    fn test_reduce_64_buckets_preserves_extrema_and_endpoints() {
        let mut buckets = [M4Bucket {
            t_first_ns: 0,
            t_last_ns: 0,
            v_first: 0.0,
            v_min: 0.0,
            v_max: 0.0,
            v_last: 0.0,
        }; 64];

        for (i, b) in buckets.iter_mut().enumerate() {
            b.t_first_ns = (i as u64) * 100;
            b.t_last_ns = (i as u64) * 100 + 99;
            b.v_first = (i as f32) * 1.0;
            b.v_min = (i as f32) * 0.5;
            b.v_max = (i as f32) * 1.5;
            b.v_last = (i as f32) * 1.0;
        }

        // Insert isolated single-point peak at index 17
        buckets[17].v_max = 999.0;
        // Insert isolated single-point dip at index 42
        buckets[42].v_min = -555.0;

        let reduced = reduce_64_buckets(&buckets);
        assert_eq!(reduced.t_first_ns, 0);
        assert_eq!(reduced.t_last_ns, 63 * 100 + 99);
        assert_eq!(reduced.v_first, 0.0);
        assert_eq!(reduced.v_last, 63.0);
        assert_eq!(reduced.v_max, 999.0, "peak must be retained 100%");
        assert_eq!(reduced.v_min, -555.0, "valley must be retained 100%");
    }

    #[test]
    fn test_simd_reductions_all_10_scalar_types() {
        let scale = LinearScale::new(2.0, -10.0);

        // 1. U8
        let raw_u8: Vec<u8> = (0..64).collect();
        let b_u8 = reduce_raw_u8_slice(&raw_u8, 0, 630, &scale);
        assert_eq!(b_u8.v_first, scale.apply_f32(0.0));
        assert_eq!(b_u8.v_last, scale.apply_f32(63.0));
        assert_eq!(b_u8.v_min, scale.apply_f32(0.0));
        assert_eq!(b_u8.v_max, scale.apply_f32(63.0));

        // 2. I8
        let raw_i8: Vec<i8> = (-32..32).collect();
        let b_i8 = reduce_raw_i8_slice(&raw_i8, 0, 630, &scale);
        assert_eq!(b_i8.v_min, scale.apply_f32(-32.0));
        assert_eq!(b_i8.v_max, scale.apply_f32(31.0));

        // 3. U16
        let raw_u16: Vec<u16> = (1000..1064).collect();
        let b_u16 = reduce_raw_u16_slice(&raw_u16, 0, 630, &scale);
        assert_eq!(b_u16.v_min, scale.apply_f32(1000.0));
        assert_eq!(b_u16.v_max, scale.apply_f32(1063.0));

        // 4. I16
        let raw_i16: Vec<i16> = (-500..-436).collect();
        let b_i16 = reduce_raw_i16_slice(&raw_i16, 0, 630, &scale);
        assert_eq!(b_i16.v_min, scale.apply_f32(-500.0));
        assert_eq!(b_i16.v_max, scale.apply_f32(-437.0));

        // 5. U32
        let raw_u32: Vec<u32> = (100_000..100_064).collect();
        let b_u32 = reduce_raw_u32_slice(&raw_u32, 0, 630, &scale);
        assert_eq!(b_u32.v_min, scale.apply_f32(100_000.0));
        assert_eq!(b_u32.v_max, scale.apply_f32(100_063.0));

        // 6. I32
        let raw_i32: Vec<i32> = (-50_000..-49_936).collect();
        let b_i32 = reduce_raw_i32_slice(&raw_i32, 0, 630, &scale);
        assert_eq!(b_i32.v_min, scale.apply_f32(-50_000.0));
        assert_eq!(b_i32.v_max, scale.apply_f32(-49_937.0));

        // 7. U64
        let raw_u64: Vec<u64> = (1_000_000..1_000_064).collect();
        let b_u64 = reduce_raw_u64_slice(&raw_u64, 0, 630, &scale);
        assert_eq!(b_u64.v_min, scale.apply_f32(1_000_000.0));
        assert_eq!(b_u64.v_max, scale.apply_f32(1_000_063.0));

        // 8. I64
        let raw_i64: Vec<i64> = (-1_000_000..-999_936).collect();
        let b_i64 = reduce_raw_i64_slice(&raw_i64, 0, 630, &scale);
        assert_eq!(b_i64.v_min, scale.apply_f32(-1_000_000.0));
        assert_eq!(b_i64.v_max, scale.apply_f32(-999_937.0));

        // 9. F32
        let raw_f32: Vec<f32> = (0..64).map(|x| (x as f32) * 0.1).collect();
        let b_f32 = reduce_raw_f32_slice(&raw_f32, 0, 630, &scale);
        assert!((b_f32.v_min - scale.apply_f32(0.0)).abs() < 1e-5);
        assert!((b_f32.v_max - scale.apply_f32(6.3)).abs() < 1e-5);

        // 10. F64
        let raw_f64: Vec<f64> = (0..64).map(|x| (x as f64) * 0.01).collect();
        let b_f64 = reduce_raw_f64_slice(&raw_f64, 0, 630, &scale);
        assert!((b_f64.v_min - scale.apply_f32(0.0)).abs() < 1e-5);
        assert!((b_f64.v_max - scale.apply_f32(0.63)).abs() < 1e-5);
    }

    #[test]
    fn test_inverted_scale_swaps_min_and_max() {
        let inverted_scale = LinearScale::new(-3.0, 50.0);
        let raw_i16: Vec<i16> = vec![10, 5, 20, 15];
        let bucket = reduce_raw_i16_slice(&raw_i16, 100, 400, &inverted_scale);

        // Raw min is 5, raw max is 20
        // Scaled raw_min: -3 * 5 + 50 = 35.0 (this becomes v_max!)
        // Scaled raw_max: -3 * 20 + 50 = -10.0 (this becomes v_min!)
        assert_eq!(bucket.v_min, -10.0);
        assert_eq!(bucket.v_max, 35.0);
        assert!(bucket.v_min <= bucket.v_max);
    }

    #[test]
    fn test_float_nan_resilience() {
        let scale = LinearScale::identity();
        let mut raw_f32 = vec![1.0_f32; 64];
        raw_f32[10] = f32::NAN;
        raw_f32[25] = 42.0;
        raw_f32[50] = -12.0;

        let b = reduce_raw_f32_slice(&raw_f32, 0, 630, &scale);
        assert!(!b.v_min.is_nan(), "v_min must not be poisoned by NaN");
        assert!(!b.v_max.is_nan(), "v_max must not be poisoned by NaN");
        assert_eq!(b.v_min, -12.0);
        assert_eq!(b.v_max, 42.0);
    }

    #[test]
    fn test_pixel_column_binning_with_gap_carry_forward() {
        let buckets = vec![
            M4Bucket {
                t_first_ns: 100,
                t_last_ns: 200,
                v_first: 1.0,
                v_min: 0.5,
                v_max: 2.0,
                v_last: 1.8,
            },
            M4Bucket {
                t_first_ns: 800,
                t_last_ns: 900,
                v_first: 3.0,
                v_min: 2.5,
                v_max: 4.0,
                v_last: 3.5,
            },
        ];

        let cols = aggregate_buckets_to_columns(&buckets, 0, 1000, 10);
        assert_eq!(cols.len(), 10);

        // Gap column between col 2 and col 8 should carry forward previous last value
        assert_eq!(cols[3].v_first, 1.8);
        assert_eq!(cols[3].v_last, 1.8);
    }
}
