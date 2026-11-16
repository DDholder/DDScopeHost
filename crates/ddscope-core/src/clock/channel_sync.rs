//! Channel-value clock synchronization with explicit and automatic rollover unwrapping.

use crate::clock::tier1::LinearDriftTracker;
use crate::types::{
    ChannelDescriptor, ChannelKind, CoreError, CoreResult, DecodedBatch, DecodedColumnBuf,
    ScalarType, TimestampNs,
};

const MAX_EXACT_F64_INT: f64 = 9_007_199_254_740_992.0; // 2^53

#[derive(Copy, Clone, Debug)]
enum CounterReading {
    Float(f64),
    U64(u64),
    I64(i64),
}

impl CounterReading {
    fn from_column(col: &DecodedColumnBuf, row: usize) -> CoreResult<Self> {
        let len = col.len();
        if row >= len {
            return Err(CoreError::Clock(format!(
                "sync channel row index {row} out of bounds for column len {len}"
            )));
        }
        match col {
            DecodedColumnBuf::U8(v) => Ok(Self::U64(u64::from(v[row]))),
            DecodedColumnBuf::U16(v) => Ok(Self::U64(u64::from(v[row]))),
            DecodedColumnBuf::U32(v) => Ok(Self::U64(u64::from(v[row]))),
            DecodedColumnBuf::U64(v) => Ok(Self::U64(v[row])),
            DecodedColumnBuf::I8(v) => Ok(Self::I64(i64::from(v[row]))),
            DecodedColumnBuf::I16(v) => Ok(Self::I64(i64::from(v[row]))),
            DecodedColumnBuf::I32(v) => Ok(Self::I64(i64::from(v[row]))),
            DecodedColumnBuf::I64(v) => Ok(Self::I64(v[row])),
            DecodedColumnBuf::F32(v) => Ok(Self::Float(f64::from(v[row]))),
            DecodedColumnBuf::F64(v) => Ok(Self::Float(v[row])),
            DecodedColumnBuf::Bool(_) | DecodedColumnBuf::StateEnum(_) => Err(CoreError::Clock(
                "ChannelCounter sync channel must be a numeric scalar column".to_owned(),
            )),
        }
    }

    #[inline]
    fn as_f64(self) -> f64 {
        match self {
            Self::Float(v) => v,
            Self::U64(u) => u as f64,
            Self::I64(i) => i as f64,
        }
    }
}

#[derive(Copy, Clone, Debug)]
struct StepResult {
    delta_counts: f64,
    wrapped: bool,
    new_prev_scalar: f64,
    new_prev_raw: f64,
    new_prev_u64: Option<u64>,
    new_prev_i64: Option<i64>,
    new_min_seen: f64,
    new_peak_seen: f64,
    new_smoothed_step: f64,
    keep_initialized: bool,
}

/// Unwraps monotonic counter values read from a numeric telemetry channel across integer,
/// signed two's-complement, or arbitrary periodic (`ARR + 1`) rollover boundaries.
#[derive(Clone, Debug)]
pub struct ChannelCounterUnwrapper {
    ns_per_count: f64,
    rollover_modulus: Option<f64>,
    int_modulus: Option<u64>,
    initialized: bool,
    prev_scalar: f64,
    prev_raw: f64,
    prev_u64: Option<u64>,
    prev_i64: Option<i64>,
    min_seen: f64,
    peak_seen: f64,
    smoothed_step: f64,
    unwrapped_counts: f64,
    rollover_count: u64,
}

impl ChannelCounterUnwrapper {
    /// Creates a new unwrapper scaling each counter step by `ns_per_count` nanoseconds.
    ///
    /// When `rollover_modulus` is `None`, overflow wrap-around boundaries are automatically
    /// recognized from the observed counter range and standard bit-width ceilings.
    pub fn new(ns_per_count: f64, rollover_modulus: Option<f64>) -> CoreResult<Self> {
        if !ns_per_count.is_finite() || ns_per_count <= 0.0 {
            return Err(CoreError::InvalidConfig(format!(
                "ChannelCounter ns_per_count must be finite and > 0, got {ns_per_count}"
            )));
        }
        let int_modulus = match rollover_modulus {
            None => None,
            Some(m) => {
                if !m.is_finite() || m <= 0.0 {
                    return Err(CoreError::InvalidConfig(format!(
                        "ChannelCounter rollover_modulus must be finite and > 0, got {m}"
                    )));
                }
                if m.fract() == 0.0 && (1.0..=MAX_EXACT_F64_INT).contains(&m) {
                    Some(m as u64)
                } else {
                    None
                }
            }
        };

        Ok(Self {
            ns_per_count,
            rollover_modulus,
            int_modulus,
            initialized: false,
            prev_scalar: 0.0,
            prev_raw: 0.0,
            prev_u64: None,
            prev_i64: None,
            min_seen: 0.0,
            peak_seen: 0.0,
            smoothed_step: 0.0,
            unwrapped_counts: 0.0,
            rollover_count: 0,
        })
    }

    /// Configured nanoseconds represented by a single counter increment.
    pub fn ns_per_count(&self) -> f64 {
        self.ns_per_count
    }

    /// Configured explicit wrap-around modulus (`None` = auto-detect).
    pub fn rollover_modulus(&self) -> Option<f64> {
        self.rollover_modulus
    }

    /// Cumulative unwrapped counter progression since initialization or last re-anchor.
    pub fn unwrapped_counts(&self) -> f64 {
        self.unwrapped_counts
    }

    /// Cumulative unwrapped nanoseconds (`unwrapped_counts * ns_per_count`).
    pub fn unwrapped_dev_ns(&self) -> f64 {
        self.unwrapped_counts * self.ns_per_count
    }

    /// Number of overflow wrap-around transitions detected.
    pub fn rollover_count(&self) -> u64 {
        self.rollover_count
    }

    /// Evaluates the step for `reading` without mutating internal state.
    fn compute_step(&self, reading: CounterReading) -> StepResult {
        if let CounterReading::Float(v) = reading {
            if !v.is_finite() {
                return StepResult {
                    delta_counts: 0.0,
                    wrapped: false,
                    new_prev_scalar: self.prev_scalar,
                    new_prev_raw: self.prev_raw,
                    new_prev_u64: self.prev_u64,
                    new_prev_i64: self.prev_i64,
                    new_min_seen: self.min_seen,
                    new_peak_seen: self.peak_seen,
                    new_smoothed_step: self.smoothed_step,
                    keep_initialized: self.initialized,
                };
            }
        }

        let raw_f = reading.as_f64();

        if !self.initialized {
            let (prev_scalar, prev_u64, prev_i64) = match (self.rollover_modulus, self.int_modulus, reading) {
                (Some(_), Some(im), CounterReading::U64(u)) => {
                    let cur_m = u % im;
                    (cur_m as f64, Some(cur_m), None)
                }
                (Some(m), _, _) => (raw_f.rem_euclid(m), None, None),
                (None, _, CounterReading::U64(u)) => (u as f64, Some(u), None),
                (None, _, CounterReading::I64(i)) => (i as f64, None, Some(i)),
                (None, _, CounterReading::Float(v)) => (v, None, None),
            };
            return StepResult {
                delta_counts: 0.0,
                wrapped: false,
                new_prev_scalar: prev_scalar,
                new_prev_raw: raw_f,
                new_prev_u64: prev_u64,
                new_prev_i64: prev_i64,
                new_min_seen: prev_scalar,
                new_peak_seen: prev_scalar,
                new_smoothed_step: self.smoothed_step,
                keep_initialized: true,
            };
        }

        // Case 1: Explicit rollover modulus configured
        if let Some(m) = self.rollover_modulus {
            if let (Some(im), CounterReading::U64(u), Some(prev_u)) =
                (self.int_modulus, reading, self.prev_u64)
            {
                let cur_m = u % im;
                let (delta, wrapped) = if cur_m >= prev_u {
                    ((cur_m - prev_u) as f64, false)
                } else {
                    (((im - prev_u) + cur_m) as f64, true)
                };
                return StepResult {
                    delta_counts: delta,
                    wrapped,
                    new_prev_scalar: cur_m as f64,
                    new_prev_raw: cur_m as f64,
                    new_prev_u64: Some(cur_m),
                    new_prev_i64: None,
                    new_min_seen: self.min_seen.min(cur_m as f64),
                    new_peak_seen: self.peak_seen.max(cur_m as f64),
                    new_smoothed_step: update_smoothed_step(self.smoothed_step, delta),
                    keep_initialized: true,
                };
            }

            let cur_m = raw_f.rem_euclid(m);
            let prev = self.prev_scalar;
            let delta = if cur_m >= prev {
                cur_m - prev
            } else {
                cur_m + m - prev
            };
            let wrapped = if raw_f < 0.0 || self.prev_raw < 0.0 {
                raw_f < 0.0 && self.prev_raw >= 0.0 && (self.prev_raw - raw_f) > m * 0.25
            } else {
                cur_m < prev
            };
            return StepResult {
                delta_counts: delta,
                wrapped,
                new_prev_scalar: cur_m,
                new_prev_raw: raw_f,
                new_prev_u64: None,
                new_prev_i64: None,
                new_min_seen: self.min_seen.min(cur_m),
                new_peak_seen: self.peak_seen.max(cur_m),
                new_smoothed_step: update_smoothed_step(self.smoothed_step, delta),
                keep_initialized: true,
            };
        }

        // Case 2: Automatic rollover detection
        // First preserve full 64-bit integer precision across `u64::MAX -> 0` / `i64::MAX -> i64::MIN`
        if let (CounterReading::U64(u), Some(prev_u)) = (reading, self.prev_u64) {
            if u >= prev_u {
                let delta = (u - prev_u) as f64;
                let cur_f = u as f64;
                return StepResult {
                    delta_counts: delta,
                    wrapped: false,
                    new_prev_scalar: cur_f,
                    new_prev_raw: cur_f,
                    new_prev_u64: Some(u),
                    new_prev_i64: None,
                    new_min_seen: self.min_seen.min(cur_f),
                    new_peak_seen: self.peak_seen.max(cur_f),
                    new_smoothed_step: update_smoothed_step(self.smoothed_step, delta),
                    keep_initialized: true,
                };
            }
            if prev_u > (1u64 << 62) && u < (1u64 << 62) {
                let delta = u.wrapping_sub(prev_u) as f64;
                let cur_f = u as f64;
                return StepResult {
                    delta_counts: delta,
                    wrapped: true,
                    new_prev_scalar: cur_f,
                    new_prev_raw: cur_f,
                    new_prev_u64: Some(u),
                    new_prev_i64: None,
                    new_min_seen: self.min_seen.min(cur_f),
                    new_peak_seen: self.peak_seen.max(cur_f),
                    new_smoothed_step: update_smoothed_step(self.smoothed_step, delta),
                    keep_initialized: true,
                };
            }
        } else if let (CounterReading::I64(v), Some(prev_i)) = (reading, self.prev_i64) {
            if v >= prev_i {
                let delta = (v - prev_i) as f64;
                let cur_f = v as f64;
                return StepResult {
                    delta_counts: delta,
                    wrapped: false,
                    new_prev_scalar: cur_f,
                    new_prev_raw: cur_f,
                    new_prev_u64: None,
                    new_prev_i64: Some(v),
                    new_min_seen: self.min_seen.min(cur_f),
                    new_peak_seen: self.peak_seen.max(cur_f),
                    new_smoothed_step: update_smoothed_step(self.smoothed_step, delta),
                    keep_initialized: true,
                };
            }
            if prev_i > (1i64 << 61) && v < -(1i64 << 61) {
                let delta = (v as u64).wrapping_sub(prev_i as u64) as f64;
                let cur_f = v as f64;
                return StepResult {
                    delta_counts: delta,
                    wrapped: true,
                    new_prev_scalar: cur_f,
                    new_prev_raw: cur_f,
                    new_prev_u64: None,
                    new_prev_i64: Some(v),
                    new_min_seen: self.min_seen.min(cur_f),
                    new_peak_seen: self.peak_seen.max(cur_f),
                    new_smoothed_step: update_smoothed_step(self.smoothed_step, delta),
                    keep_initialized: true,
                };
            }
        }

        let cur = raw_f;
        let prev = self.prev_scalar;
        let next_u64 = match reading {
            CounterReading::U64(u) => Some(u),
            _ => None,
        };
        let next_i64 = match reading {
            CounterReading::I64(i) => Some(i),
            _ => None,
        };

        if cur >= prev {
            let delta = cur - prev;
            return StepResult {
                delta_counts: delta,
                wrapped: false,
                new_prev_scalar: cur,
                new_prev_raw: raw_f,
                new_prev_u64: next_u64,
                new_prev_i64: next_i64,
                new_min_seen: self.min_seen.min(cur),
                new_peak_seen: self.peak_seen.max(cur),
                new_smoothed_step: update_smoothed_step(self.smoothed_step, delta),
                keep_initialized: true,
            };
        }

        // Counter dropped (`cur < prev`): distinguish genuine rollover plunge vs minor backward jitter
        let eff_min = self.min_seen.min(cur);
        let span = (self.peak_seen - eff_min)
            .max(prev - eff_min)
            .max(prev.abs());
        let drop = prev - cur;

        if span > 0.0 && drop >= span * 0.5 {
            let step_ref = if self.smoothed_step > 0.0 {
                self.smoothed_step
            } else {
                1.0
            };
            let modulus = infer_auto_rollover_modulus(eff_min, self.peak_seen, cur, step_ref, drop);
            let delta = (cur + modulus - prev).max(0.0);
            return StepResult {
                delta_counts: delta,
                wrapped: true,
                new_prev_scalar: cur,
                new_prev_raw: raw_f,
                new_prev_u64: next_u64,
                new_prev_i64: next_i64,
                new_min_seen: eff_min,
                new_peak_seen: self.peak_seen.max(cur),
                new_smoothed_step: update_smoothed_step(self.smoothed_step, delta),
                keep_initialized: true,
            };
        }

        // Small backward dip without rollover magnitude: clamp forward progress to 0 and hold high-water mark
        StepResult {
            delta_counts: 0.0,
            wrapped: false,
            new_prev_scalar: prev,
            new_prev_raw: self.prev_raw,
            new_prev_u64: self.prev_u64,
            new_prev_i64: self.prev_i64,
            new_min_seen: self.min_seen,
            new_peak_seen: self.peak_seen,
            new_smoothed_step: self.smoothed_step,
            keep_initialized: true,
        }
    }

    fn apply_step(&mut self, step: StepResult) -> f64 {
        self.initialized = step.keep_initialized;
        self.prev_scalar = step.new_prev_scalar;
        self.prev_raw = step.new_prev_raw;
        self.prev_u64 = step.new_prev_u64;
        self.prev_i64 = step.new_prev_i64;
        self.min_seen = step.new_min_seen;
        self.peak_seen = step.new_peak_seen;
        self.smoothed_step = step.new_smoothed_step;
        self.unwrapped_counts += step.delta_counts;
        if step.wrapped {
            self.rollover_count = self.rollover_count.saturating_add(1);
        }
        self.unwrapped_dev_ns()
    }

    /// Pushes a scalar floating-point counter sample and returns unwrapped device nanoseconds.
    pub fn push_value(&mut self, value: f64) -> f64 {
        let step = self.compute_step(CounterReading::Float(value));
        self.apply_step(step)
    }

    /// Evaluates unwrapped device nanoseconds for `value` without mutating state.
    pub fn peek_value_dev_ns(&self, value: f64) -> f64 {
        if !self.initialized {
            return 0.0;
        }
        let step = self.compute_step(CounterReading::Float(value));
        (self.unwrapped_counts + step.delta_counts) * self.ns_per_count
    }

    /// Evaluates unwrapped device nanoseconds for `(col, row)` without mutating state.
    pub(crate) fn peek_column_dev_ns(&self, col: &DecodedColumnBuf, row: usize) -> CoreResult<f64> {
        let reading = CounterReading::from_column(col, row)?;
        if !self.initialized {
            return Ok(0.0);
        }
        let step = self.compute_step(reading);
        Ok((self.unwrapped_counts + step.delta_counts) * self.ns_per_count)
    }

    /// Pushes `(col, row)` from a decoded column and returns unwrapped device nanoseconds.
    pub(crate) fn push_column_row(&mut self, col: &DecodedColumnBuf, row: usize) -> CoreResult<f64> {
        let reading = CounterReading::from_column(col, row)?;
        let step = self.compute_step(reading);
        Ok(self.apply_step(step))
    }

    /// Re-anchors the unwrapper to `(col, row)` as relative `0.0 ns` origin while preserving
    /// learned counter span heuristics across a stream gap.
    pub(crate) fn reanchor_column_row(
        &mut self,
        col: &DecodedColumnBuf,
        row: usize,
    ) -> CoreResult<f64> {
        let reading = CounterReading::from_column(col, row)?;
        let prev_rollovers = self.rollover_count;
        let prev_min = if self.initialized {
            Some(self.min_seen)
        } else {
            None
        };
        let prev_peak = if self.initialized {
            Some(self.peak_seen)
        } else {
            None
        };
        let prev_step = self.smoothed_step;

        self.initialized = false;
        self.unwrapped_counts = 0.0;
        let step = self.compute_step(reading);
        self.apply_step(step);
        self.rollover_count = prev_rollovers;
        if let Some(pm) = prev_min {
            self.min_seen = self.min_seen.min(pm);
        }
        if let Some(pp) = prev_peak {
            self.peak_seen = self.peak_seen.max(pp);
        }
        if prev_step > 0.0 {
            self.smoothed_step = prev_step;
        }
        Ok(0.0)
    }

    /// Resets all unwrapper state completely.
    pub fn reset(&mut self) {
        self.initialized = false;
        self.prev_scalar = 0.0;
        self.prev_raw = 0.0;
        self.prev_u64 = None;
        self.prev_i64 = None;
        self.min_seen = 0.0;
        self.peak_seen = 0.0;
        self.smoothed_step = 0.0;
        self.unwrapped_counts = 0.0;
        self.rollover_count = 0;
    }
}

#[inline]
fn update_smoothed_step(prev_step: f64, delta: f64) -> f64 {
    if delta <= 0.0 || !delta.is_finite() {
        return prev_step;
    }
    if prev_step <= 0.0 {
        delta
    } else {
        prev_step * 0.8 + delta * 0.2
    }
}

#[inline]
fn infer_auto_rollover_modulus(
    eff_min: f64,
    peak_seen: f64,
    cur: f64,
    step_ref: f64,
    drop: f64,
) -> f64 {
    if eff_min >= 0.0 {
        const UNSIGNED_MODULI: [f64; 4] = [256.0, 65_536.0, 16_777_216.0, 4_294_967_296.0];
        for std_m in UNSIGNED_MODULI {
            if peak_seen < std_m {
                let slack = (step_ref * 4.0).max(std_m * 0.02);
                if (std_m - peak_seen) <= slack && cur < std_m * 0.5 {
                    return std_m;
                }
                break;
            }
        }
    } else {
        const SIGNED_MODULI: [(f64, f64, f64); 3] = [
            (-128.0, 128.0, 256.0),
            (-32_768.0, 32_768.0, 65_536.0),
            (-2_147_483_648.0, 2_147_483_648.0, 4_294_967_296.0),
        ];
        for (lo, hi, std_m) in SIGNED_MODULI {
            if eff_min >= lo && peak_seen < hi {
                let slack = (step_ref * 4.0).max(std_m * 0.02);
                if (hi - peak_seen) <= slack && (cur - lo) <= std_m * 0.5 {
                    return std_m;
                }
                break;
            }
        }
    }

    ((peak_seen - eff_min) + step_ref).max(drop + step_ref)
}

fn match_channel_descriptor(descs: &[ChannelDescriptor], raw_spec: &str) -> CoreResult<usize> {
    let trimmed = raw_spec.trim();
    if trimmed.is_empty() {
        return Err(CoreError::InvalidConfig(
            "ChannelCounter sync channel must not be empty".to_owned(),
        ));
    }

    // Support both bare `local_path` (e.g. `timestamp_cnt`) and qualified `<source_id>/<local_path>`.
    let candidate = trimmed
        .split_once('/')
        .map_or(trimmed, |(_, local)| local.trim());

    let found_idx = descs
        .iter()
        .position(|d| d.local_path == trimmed || d.local_path == candidate)
        .or_else(|| {
            descs.iter().position(|d| {
                d.local_path.eq_ignore_ascii_case(trimmed)
                    || d.local_path.eq_ignore_ascii_case(candidate)
            })
        })
        .or_else(|| {
            candidate
                .strip_prefix('#')
                .and_then(|num| num.parse::<usize>().ok())
                .filter(|&i| i < descs.len())
        });

    let Some(idx) = found_idx else {
        let available: Vec<&str> = descs.iter().map(|d| d.local_path.as_str()).collect();
        return Err(CoreError::InvalidConfig(format!(
            "ChannelCounter sync channel '{trimmed}' does not exist in protocol channels {available:?}"
        )));
    };

    match &descs[idx].kind {
        ChannelKind::Scalar { scalar_type, .. } if *scalar_type != ScalarType::Bool => Ok(idx),
        ChannelKind::Scalar { scalar_type, .. } => Err(CoreError::InvalidConfig(format!(
            "ChannelCounter sync channel '{}' has unsupported scalar_type {:?}; expected numeric counter",
            descs[idx].local_path, scalar_type
        ))),
        _ => Err(CoreError::InvalidConfig(format!(
            "ChannelCounter sync channel '{}' must be a numeric scalar channel",
            descs[idx].local_path
        ))),
    }
}

/// Complete channel-counter synchronization state integrated inside [`StreamClockSynchronizer`].
#[derive(Clone, Debug)]
pub(crate) struct ChannelSyncState {
    channel: String,
    bound_col_idx: Option<usize>,
    drift_compensation: bool,
    unwrapper: ChannelCounterUnwrapper,
    tracker: LinearDriftTracker,
    direct_origin_host_ns: Option<TimestampNs>,
    direct_origin_dev_ns: f64,
    gap_reset_ns: u64,
    last_pkt_recv_ns: Option<TimestampNs>,
    last_emitted_ns: Option<TimestampNs>,
    last_dev_ns: Option<f64>,
    last_dt_ns: f64,
    last_phase_error_ns: f64,
    gap_resets: u64,
}

impl ChannelSyncState {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        channel: String,
        ns_per_count: f64,
        rollover_modulus: Option<f64>,
        drift_compensation: bool,
        window_size: usize,
        ewma_alpha: f64,
        gap_reset_ns: u64,
    ) -> CoreResult<Self> {
        let trimmed = channel.trim();
        if trimmed.is_empty() {
            return Err(CoreError::InvalidConfig(
                "ChannelCounter sync channel must not be empty".to_owned(),
            ));
        }
        if gap_reset_ns == 0 {
            return Err(CoreError::InvalidConfig(
                "gap_reset_ns must be > 0".to_owned(),
            ));
        }
        Ok(Self {
            channel: trimmed.to_owned(),
            bound_col_idx: None,
            drift_compensation,
            unwrapper: ChannelCounterUnwrapper::new(ns_per_count, rollover_modulus)?,
            tracker: LinearDriftTracker::new(window_size, ewma_alpha)?,
            direct_origin_host_ns: None,
            direct_origin_dev_ns: 0.0,
            gap_reset_ns,
            last_pkt_recv_ns: None,
            last_emitted_ns: None,
            last_dev_ns: None,
            last_dt_ns: 0.0,
            last_phase_error_ns: 0.0,
            gap_resets: 0,
        })
    }

    pub(crate) fn bind_declared_channels(
        &mut self,
        descs: &[ChannelDescriptor],
    ) -> CoreResult<usize> {
        let idx = match_channel_descriptor(descs, &self.channel)?;
        self.bound_col_idx = Some(idx);
        Ok(idx)
    }

    pub(crate) fn bind_channel_index(&mut self, col_idx: usize) {
        self.bound_col_idx = Some(col_idx);
    }

    pub(crate) fn assign_batch(
        &mut self,
        pkt_recv_ns: TimestampNs,
        batch: &mut DecodedBatch,
    ) -> CoreResult<()> {
        let count = batch.sample_rows;
        if count == 0 {
            batch.timestamps_ns.clear();
            return Ok(());
        }
        let col_idx = self.bound_col_idx.ok_or_else(|| {
            CoreError::Clock(format!(
                "ChannelCounter sync channel '{}' is not bound to any decoded column",
                self.channel
            ))
        })?;
        let col = batch.columns.get(col_idx).ok_or_else(|| {
            CoreError::Clock(format!(
                "ChannelCounter bound column index {col_idx} out of bounds ({} columns)",
                batch.columns.len()
            ))
        })?;
        if col.len() < count {
            return Err(CoreError::Clock(format!(
                "ChannelCounter column len {} is smaller than batch sample_rows {count}",
                col.len()
            )));
        }

        if batch.timestamps_ns.len() != count {
            batch.timestamps_ns.resize(count, 0);
        }

        let host_gap = self
            .last_pkt_recv_ns
            .is_some_and(|prev| pkt_recv_ns.saturating_sub(prev) > self.gap_reset_ns);
        let dev_gap = if self.drift_compensation && !host_gap && self.last_pkt_recv_ns.is_some() {
            let predicted_dev_ns = self.unwrapper.peek_column_dev_ns(col, 0)?;
            let predicted_global = self.tracker.map_to_global_ns(predicted_dev_ns);
            (pkt_recv_ns as i128 - predicted_global as i128).unsigned_abs()
                > u128::from(self.gap_reset_ns.max(1_000_000_000))
        } else {
            false
        };
        let is_gap = host_gap || dev_gap;

        let first_dev_ns = if is_gap {
            self.gap_resets += 1;
            let dev0 = self.unwrapper.reanchor_column_row(col, 0)?;
            let safe_host_ns = self
                .last_emitted_ns
                .map_or(pkt_recv_ns, |p| pkt_recv_ns.max(p));
            if self.drift_compensation {
                self.tracker.reanchor_on_gap(dev0, safe_host_ns);
            } else {
                self.direct_origin_host_ns = Some(safe_host_ns);
                self.direct_origin_dev_ns = dev0;
            }
            self.last_phase_error_ns = 0.0;
            dev0
        } else {
            let predicted_dev_ns = self.unwrapper.peek_column_dev_ns(col, 0)?;
            let predicted_global = if self.drift_compensation {
                self.tracker.map_to_global_ns(predicted_dev_ns)
            } else if let Some(origin_host) = self.direct_origin_host_ns {
                map_direct_ns(
                    origin_host,
                    self.direct_origin_dev_ns,
                    predicted_dev_ns,
                )
            } else {
                pkt_recv_ns
            };
            self.last_phase_error_ns = if self.last_pkt_recv_ns.is_some() {
                (pkt_recv_ns as i128 - predicted_global as i128) as f64
            } else {
                0.0
            };

            let dev0 = self.unwrapper.push_column_row(col, 0)?;
            let anchor_host_ns = if self.last_pkt_recv_ns.is_none() {
                self.last_emitted_ns
                    .map_or(pkt_recv_ns, |p| pkt_recv_ns.max(p))
            } else {
                pkt_recv_ns
            };
            if self.drift_compensation {
                self.tracker.push_anchor(dev0, anchor_host_ns);
            } else if self.direct_origin_host_ns.is_none() {
                self.direct_origin_host_ns = Some(anchor_host_ns);
                self.direct_origin_dev_ns = dev0;
            }
            dev0
        };

        let mut prev_ts = self.last_emitted_ns;
        let mut last_dev_ns = first_dev_ns;

        for row in 0..count {
            let dev_ns = if row == 0 {
                first_dev_ns
            } else {
                self.unwrapper.push_column_row(col, row)?
            };
            last_dev_ns = dev_ns;

            let mapped = if self.drift_compensation {
                self.tracker.map_to_global_ns(dev_ns)
            } else {
                let origin_host = self.direct_origin_host_ns.unwrap_or(pkt_recv_ns);
                map_direct_ns(origin_host, self.direct_origin_dev_ns, dev_ns)
            };
            let mono = prev_ts.map_or(mapped, |p| mapped.max(p));
            batch.timestamps_ns[row] = mono;
            prev_ts = Some(mono);
        }

        let slope = if self.drift_compensation {
            self.tracker.drift_slope()
        } else {
            1.0
        };
        if count > 1 {
            let span_dev = last_dev_ns - first_dev_ns;
            if span_dev > 0.0 {
                self.last_dt_ns = (span_dev * slope) / ((count - 1) as f64);
            }
        } else if let Some(prev_dev) = self.last_dev_ns {
            if !is_gap && first_dev_ns > prev_dev {
                self.last_dt_ns = (first_dev_ns - prev_dev) * slope;
            }
        }

        self.last_dev_ns = Some(last_dev_ns);
        self.last_pkt_recv_ns = Some(pkt_recv_ns);
        self.last_emitted_ns = prev_ts;
        Ok(())
    }

    pub(crate) fn assign_raw_slice(
        &mut self,
        pkt_recv_ns: TimestampNs,
        values: &[u64],
        count: usize,
        out_ns: &mut [TimestampNs],
    ) -> CoreResult<()> {
        if values.len() != count {
            return Err(CoreError::Clock(format!(
                "ChannelCounter values length {} does not match sample count {count}",
                values.len()
            )));
        }
        let col = DecodedColumnBuf::U64(values.to_vec());
        let mut temp_batch = DecodedBatch {
            sample_rows: count,
            packet_recv_ns: pkt_recv_ns,
            hw_ticks: Vec::new(),
            timestamps_ns: Vec::with_capacity(count),
            columns: vec![col],
            ack_events: Vec::new(),
        };
        let saved_idx = self.bound_col_idx;
        self.bound_col_idx = Some(0);
        let res = self.assign_batch(pkt_recv_ns, &mut temp_batch);
        self.bound_col_idx = saved_idx;
        res?;
        out_ns[..count].copy_from_slice(&temp_batch.timestamps_ns[..count]);
        Ok(())
    }

    pub(crate) fn reset(&mut self) {
        self.unwrapper.reset();
        self.tracker.reset();
        self.direct_origin_host_ns = None;
        self.direct_origin_dev_ns = 0.0;
        self.last_pkt_recv_ns = None;
        self.last_emitted_ns = None;
        self.last_dev_ns = None;
        self.last_dt_ns = 0.0;
        self.last_phase_error_ns = 0.0;
        self.gap_resets = 0;
    }

    pub(crate) fn reanchor_preserve_monotonic(&mut self) {
        let preserved = self.last_emitted_ns;
        let preserved_dt = self.last_dt_ns;
        let gaps = self.gap_resets.saturating_add(1);
        self.unwrapper.reset();
        self.tracker.reset();
        self.direct_origin_host_ns = None;
        self.direct_origin_dev_ns = 0.0;
        self.last_pkt_recv_ns = None;
        self.last_emitted_ns = preserved;
        self.last_dev_ns = None;
        self.last_dt_ns = preserved_dt;
        self.last_phase_error_ns = 0.0;
        self.gap_resets = gaps;
    }

    pub(crate) fn seed_monotonic_floor(&mut self, floor_ns: TimestampNs) {
        self.last_emitted_ns = Some(
            self.last_emitted_ns
                .map_or(floor_ns, |prev| prev.max(floor_ns)),
        );
        self.reanchor_preserve_monotonic();
    }

    pub(crate) fn last_emitted_ns(&self) -> Option<TimestampNs> {
        self.last_emitted_ns
    }

    pub(crate) fn estimated_dt_ns(&self) -> f64 {
        self.last_dt_ns
    }

    pub(crate) fn drift_slope(&self) -> f64 {
        if self.drift_compensation {
            self.tracker.drift_slope()
        } else {
            1.0
        }
    }

    pub(crate) fn phase_error_ns(&self) -> f64 {
        self.last_phase_error_ns
    }

    pub(crate) fn gap_resets(&self) -> u64 {
        self.gap_resets
    }

    pub(crate) fn locked(&self) -> bool {
        if self.drift_compensation {
            self.tracker.anchor_count() >= 4
        } else {
            self.direct_origin_host_ns.is_some()
        }
    }
}

#[inline]
fn map_direct_ns(origin_host_ns: TimestampNs, origin_dev_ns: f64, dev_ns: f64) -> TimestampNs {
    let rel = (dev_ns - origin_dev_ns).round() as i128;
    let abs = origin_host_ns as i128 + rel;
    abs.max(0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::SourceId;

    #[test]
    fn explicit_rollover_handles_u8_u16_signed_i16_custom_arr_and_u64() {
        // 8-bit wrapping: 254 -> 255 -> 0 -> 1 with 100 ns/count
        let mut u8_unwrap = ChannelCounterUnwrapper::new(100.0, Some(256.0)).unwrap();
        assert_eq!(u8_unwrap.push_value(254.0), 0.0);
        assert_eq!(u8_unwrap.push_value(255.0), 100.0);
        assert_eq!(u8_unwrap.push_value(0.0), 200.0);
        assert_eq!(u8_unwrap.push_value(1.0), 300.0);
        assert_eq!(u8_unwrap.rollover_count(), 1);

        // 16-bit signed two's-complement wrap: 32766 -> 32767 -> -32768 -> -32767
        let mut i16_unwrap = ChannelCounterUnwrapper::new(1000.0, Some(65_536.0)).unwrap();
        assert_eq!(i16_unwrap.push_value(32_766.0), 0.0);
        assert_eq!(i16_unwrap.push_value(32_767.0), 1_000.0);
        assert_eq!(i16_unwrap.push_value(-32_768.0), 2_000.0);
        assert_eq!(i16_unwrap.push_value(-32_767.0), 3_000.0);
        assert_eq!(i16_unwrap.rollover_count(), 1);

        // Custom STM32 ARR+1 = 10_000 (0..9999 -> 0) in steps of 100
        let mut arr_unwrap = ChannelCounterUnwrapper::new(1_000.0, Some(10_000.0)).unwrap();
        assert_eq!(arr_unwrap.push_value(9_800.0), 0.0);
        assert_eq!(arr_unwrap.push_value(9_900.0), 100_000.0);
        assert_eq!(arr_unwrap.push_value(0.0), 200_000.0);
        assert_eq!(arr_unwrap.push_value(250.0), 450_000.0);
        assert_eq!(arr_unwrap.rollover_count(), 1);

        // Full 64-bit U64 precision (> 2^53) and wrap across u64::MAX -> 0
        let mut u64_unwrap = ChannelCounterUnwrapper::new(10.0, None).unwrap();
        let col = DecodedColumnBuf::U64(vec![u64::MAX - 2, u64::MAX - 1, u64::MAX, 0, 4]);
        assert_eq!(u64_unwrap.push_column_row(&col, 0).unwrap(), 0.0);
        assert_eq!(u64_unwrap.push_column_row(&col, 1).unwrap(), 10.0);
        assert_eq!(u64_unwrap.push_column_row(&col, 2).unwrap(), 20.0);
        assert_eq!(u64_unwrap.push_column_row(&col, 3).unwrap(), 30.0);
        assert_eq!(u64_unwrap.push_column_row(&col, 4).unwrap(), 70.0);
        assert_eq!(u64_unwrap.rollover_count(), 1);
    }

    #[test]
    fn auto_rollover_recognizes_u16_signed_i16_custom_arr_and_ignores_minor_jitter() {
        // 1. Mid-cycle start unsigned 16-bit counter: 65500..65535 -> 0..10
        let mut u16_auto = ChannelCounterUnwrapper::new(1_000.0, None).unwrap();
        assert_eq!(u16_auto.push_value(65_500.0), 0.0);
        for c in 65_501..=65_535 {
            u16_auto.push_value(f64::from(c));
        }
        let after_wrap = u16_auto.push_value(0.0);
        assert!((after_wrap - 36_000.0).abs() < 1e-6);
        assert_eq!(u16_auto.rollover_count(), 1);

        // 2. Signed i16 auto detection across 32767 -> -32768 -> -32767
        let mut i16_auto = ChannelCounterUnwrapper::new(500.0, None).unwrap();
        let mut v = -32_768i32;
        assert_eq!(i16_auto.push_value(v as f64), 0.0);
        while v < 32_767 {
            v += 256;
            if v > 32_767 {
                v = 32_767;
            }
            i16_auto.push_value(v as f64);
        }
        let wrapped_i16 = i16_auto.push_value(-32_768.0);
        assert!((wrapped_i16 - 65_536.0 * 500.0).abs() < 1.0);
        assert_eq!(i16_auto.rollover_count(), 1);

        // 3. Custom ARR=9999 (mod 10000) auto detection even starting mid-cycle at 5000 in steps of 100
        let mut arr_auto = ChannelCounterUnwrapper::new(100.0, None).unwrap();
        assert_eq!(arr_auto.push_value(5_000.0), 0.0);
        for c in (5_100..=9_900).step_by(100) {
            arr_auto.push_value(c as f64);
        }
        let expected_before = (9_900.0 - 5_000.0) * 100.0;
        let wrapped_arr = arr_auto.push_value(0.0);
        assert!((wrapped_arr - (expected_before + 10_000.0)).abs() < 1e-6);
        assert_eq!(arr_auto.rollover_count(), 1);

        // 4. Small backward jitter on large counter is clamped, not misinterpreted as rollover
        let mut jitter = ChannelCounterUnwrapper::new(100.0, None).unwrap();
        assert_eq!(jitter.push_value(0.0), 0.0);
        assert_eq!(jitter.push_value(5_000.0), 500_000.0);
        assert_eq!(jitter.push_value(4_990.0), 500_000.0);
        assert_eq!(jitter.rollover_count(), 0);
        assert_eq!(jitter.push_value(5_020.0), 502_000.0);
    }

    #[test]
    fn channel_sync_state_binds_rejects_bool_and_preserves_spacing_with_or_without_drift_comp() {
        let src = SourceId::new("u0");
        let descs = vec![
            ChannelDescriptor::new_scalar(src.clone(), "va", ScalarType::F32),
            ChannelDescriptor::new_bool(src.clone(), "pwm_a"),
            ChannelDescriptor::new_scalar(src.clone(), "tick_cnt", ScalarType::F32),
        ];

        // Reject missing or boolean channels
        let mut bad =
            ChannelSyncState::new("pwm_a".into(), 1_000.0, None, false, 64, 0.2, 250_000_000)
                .unwrap();
        assert!(matches!(
            bad.bind_declared_channels(&descs),
            Err(CoreError::InvalidConfig(_))
        ));
        let mut missing =
            ChannelSyncState::new("no_such".into(), 1_000.0, None, false, 64, 0.2, 250_000_000)
                .unwrap();
        assert!(matches!(
            missing.bind_declared_channels(&descs),
            Err(CoreError::InvalidConfig(_))
        ));

        // Direct scaling mode (drift_compensation = false) with 10_000 ns/count & wrap at 100
        let mut direct = ChannelSyncState::new(
            "u0/tick_cnt".into(),
            10_000.0,
            Some(100.0),
            false,
            64,
            0.2,
            250_000_000,
        )
        .unwrap();
        assert_eq!(direct.bind_declared_channels(&descs).unwrap(), 2);

        let mut batch = DecodedBatch::from_descriptors(&descs, 4);
        batch.sample_rows = 4;
        batch.columns[2] = DecodedColumnBuf::F32(vec![98.0, 99.0, 0.0, 2.0]);
        direct.assign_batch(1_000_000, &mut batch).unwrap();
        assert_eq!(
            batch.timestamps_ns,
            vec![1_000_000, 1_010_000, 1_020_000, 1_040_000]
        );
        assert_eq!(direct.drift_slope(), 1.0);
        assert!(direct.locked());

        // Drift-compensated mode across multiple packets with floor seeding
        let mut ols =
            ChannelSyncState::new("tick_cnt".into(), 1_000.0, Some(100.0), true, 16, 0.2, 250_000_000)
                .unwrap();
        assert_eq!(ols.bind_declared_channels(&descs).unwrap(), 2);
        ols.seed_monotonic_floor(5_000_000);
        batch.columns[2] = DecodedColumnBuf::F32(vec![99.0, 0.0, 1.0, 2.0]);
        ols.assign_batch(4_000_000, &mut batch).unwrap();
        assert!(batch.timestamps_ns[0] >= 5_000_000);
        assert!(batch.timestamps_ns.windows(2).all(|w| w[1] > w[0]));
    }
}
