//! Tier 1 hardware counter unwrapping and sliding-window linear drift anchoring.

use std::collections::VecDeque;

use crate::types::{CoreError, CoreResult, TimestampNs};

/// Minimum allowed linear drift slope (`-5000 ppm`).
pub const DRIFT_SLOPE_MIN: f64 = 0.995;

/// Maximum allowed linear drift slope (`+5000 ppm`).
pub const DRIFT_SLOPE_MAX: f64 = 1.005;

/// Unwraps fixed-width wrapping hardware counters across rollover boundaries.
#[derive(Clone, Debug)]
pub struct CounterUnwrapper {
    counter_bits: u8,
    mask: u64,
    ns_per_tick: f64,
    prev_raw: Option<u64>,
    unwrapped_ticks: u64,
}

impl CounterUnwrapper {
    /// Creates a new unwrapper for an unsigned `counter_bits`-wide counter ticking at `tick_hz`.
    pub fn new(tick_hz: f64, counter_bits: u8) -> CoreResult<Self> {
        if !tick_hz.is_finite() || tick_hz <= 0.0 {
            return Err(CoreError::InvalidConfig(format!(
                "Tier1HardwareCounter tick_hz must be finite and > 0, got {tick_hz}"
            )));
        }
        if !(8..=64).contains(&counter_bits) {
            return Err(CoreError::InvalidConfig(format!(
                "Tier1HardwareCounter counter_bits must be in 8..=64, got {counter_bits}"
            )));
        }
        let mask = u64::MAX >> (64 - u32::from(counter_bits));
        Ok(Self {
            counter_bits,
            mask,
            ns_per_tick: 1e9 / tick_hz,
            prev_raw: None,
            unwrapped_ticks: 0,
        })
    }

    /// Returns the configured counter bit width.
    pub fn counter_bits(&self) -> u8 {
        self.counter_bits
    }

    /// Returns the cumulative unwrapped ticks since initialization or last reset.
    pub fn unwrapped_ticks(&self) -> u64 {
        self.unwrapped_ticks
    }

    /// Advances the unwrapper with the next raw hardware counter reading and returns device nanoseconds.
    pub fn push_raw(&mut self, raw: u64) -> f64 {
        let masked = raw & self.mask;
        match self.prev_raw {
            None => {
                self.prev_raw = Some(masked);
                self.unwrapped_ticks = 0;
            }
            Some(prev) => {
                let delta = (masked.wrapping_sub(prev)) & self.mask;
                self.prev_raw = Some(masked);
                self.unwrapped_ticks = self.unwrapped_ticks.saturating_add(delta);
            }
        }
        (self.unwrapped_ticks as f64) * self.ns_per_tick
    }

    /// Evaluates device nanoseconds for a raw tick without mutating state (only valid after first tick).
    pub fn peek_next_dev_ns(&self, raw: u64) -> f64 {
        let masked = raw & self.mask;
        let ticks = match self.prev_raw {
            None => 0,
            Some(prev) => {
                let delta = (masked.wrapping_sub(prev)) & self.mask;
                self.unwrapped_ticks.saturating_add(delta)
            }
        };
        (ticks as f64) * self.ns_per_tick
    }

    /// Re-anchors the unwrapper to `raw` as a new origin after a stream gap.
    pub fn reanchor_to(&mut self, raw: u64) -> f64 {
        self.prev_raw = Some(raw & self.mask);
        self.unwrapped_ticks = 0;
        0.0
    }

    /// Resets unwrapper state completely.
    pub fn reset(&mut self) {
        self.prev_raw = None;
        self.unwrapped_ticks = 0;
    }
}

/// Sliding-window ordinary least squares drift estimator mapping `t_dev_ns -> t_host_ns`.
#[derive(Clone, Debug)]
pub struct LinearDriftTracker {
    window_size: usize,
    ewma_alpha: f64,
    offset_ewma_alpha: f64,
    anchors: VecDeque<(f64, f64)>,
    origin_dev_ns: f64,
    origin_host_ns: TimestampNs,
    initialized: bool,
    alpha_hat: f64,
    mean_x: f64,
    mean_y: f64,
    state_dev_x: f64,
    state_host_y: f64,
}

impl LinearDriftTracker {
    /// Creates a new drift tracker with sliding window `window_size` and EWMA smoothing factor.
    pub fn new(window_size: usize, ewma_alpha: f64) -> CoreResult<Self> {
        if window_size < 2 {
            return Err(CoreError::InvalidConfig(format!(
                "drift_window_size must be >= 2, got {window_size}"
            )));
        }
        if !ewma_alpha.is_finite() || ewma_alpha <= 0.0 || ewma_alpha > 1.0 {
            return Err(CoreError::InvalidConfig(format!(
                "drift_ewma_alpha must be in (0.0, 1.0], got {ewma_alpha}"
            )));
        }
        Ok(Self {
            window_size,
            ewma_alpha,
            offset_ewma_alpha: (ewma_alpha * 0.25).min(0.05),
            anchors: VecDeque::with_capacity(window_size),
            origin_dev_ns: 0.0,
            origin_host_ns: 0,
            initialized: false,
            alpha_hat: 1.0,
            mean_x: 0.0,
            mean_y: 0.0,
            state_dev_x: 0.0,
            state_host_y: 0.0,
        })
    }

    /// Records a new `(t_dev_ns, t_host_ns)` packet anchor and updates `alpha_hat` and smoothed phase.
    pub fn push_anchor(&mut self, t_dev_ns: f64, t_host_ns: TimestampNs) {
        if !self.initialized {
            self.initialized = true;
            self.origin_dev_ns = t_dev_ns;
            self.origin_host_ns = t_host_ns;
            self.anchors.push_back((0.0, 0.0));
            self.mean_x = 0.0;
            self.mean_y = 0.0;
            self.state_dev_x = 0.0;
            self.state_host_y = 0.0;
            return;
        }

        // Shift coordinates relative to the anchor origin so f64 preserves sub-ns precision
        // even when host timestamps are large Unix-epoch nanoseconds.
        let x = t_dev_ns - self.origin_dev_ns;
        let y = (t_host_ns as i128 - self.origin_host_ns as i128) as f64;

        if self.anchors.len() == self.window_size {
            self.anchors.pop_front();
        }
        self.anchors.push_back((x, y));

        let n = self.anchors.len() as f64;
        let (sum_x, sum_y) = self
            .anchors
            .iter()
            .fold((0.0, 0.0), |(sx, sy), &(ax, ay)| (sx + ax, sy + ay));
        self.mean_x = sum_x / n;
        self.mean_y = sum_y / n;

        if self.anchors.len() >= 2 {
            let (s_xx, s_xy) = self.anchors.iter().fold((0.0, 0.0), |(xx, xy), &(ax, ay)| {
                let dx = ax - self.mean_x;
                let dy = ay - self.mean_y;
                (xx + dx * dx, xy + dx * dy)
            });

            if s_xx > 0.0 {
                let alpha_raw = s_xy / s_xx;
                let alpha_clamped = alpha_raw.clamp(DRIFT_SLOPE_MIN, DRIFT_SLOPE_MAX);
                self.alpha_hat = (self.alpha_hat
                    + self.ewma_alpha * (alpha_clamped - self.alpha_hat))
                    .clamp(DRIFT_SLOPE_MIN, DRIFT_SLOPE_MAX);
            }
        }

        // Advance smoothed trajectory along alpha_hat and apply low-pass correction toward the
        // OLS centroid line so host arrival jitter does not cause boundary backward clamps.
        let y_pred = self.state_host_y + self.alpha_hat * (x - self.state_dev_x);
        let y_ols = self.mean_y + self.alpha_hat * (x - self.mean_x);
        self.state_host_y = y_pred + self.offset_ewma_alpha * (y_ols - y_pred);
        self.state_dev_x = x;
    }

    /// Maps a device timestamp `t_dev_ns` to global host-aligned nanoseconds.
    pub fn map_to_global_ns(&self, t_dev_ns: f64) -> TimestampNs {
        if !self.initialized {
            return t_dev_ns.round().max(0.0) as u64;
        }
        let x = t_dev_ns - self.origin_dev_ns;
        let rel_ns = (self.state_host_y + self.alpha_hat * (x - self.state_dev_x)).round() as i128;
        let abs_ns = self.origin_host_ns as i128 + rel_ns;
        abs_ns.max(0) as u64
    }

    /// Re-anchors the regression window at `(t_dev_ns, t_host_ns)` after a stream gap,
    /// preserving the learned crystal drift slope `alpha_hat`.
    pub fn reanchor_on_gap(&mut self, t_dev_ns: f64, t_host_ns: TimestampNs) {
        self.anchors.clear();
        self.initialized = true;
        self.origin_dev_ns = t_dev_ns;
        self.origin_host_ns = t_host_ns;
        self.anchors.push_back((0.0, 0.0));
        self.mean_x = 0.0;
        self.mean_y = 0.0;
        self.state_dev_x = 0.0;
        self.state_host_y = 0.0;
    }

    /// Resets both the window and the learned slope back to `1.0`.
    pub fn reset(&mut self) {
        self.anchors.clear();
        self.origin_dev_ns = 0.0;
        self.origin_host_ns = 0;
        self.initialized = false;
        self.alpha_hat = 1.0;
        self.mean_x = 0.0;
        self.mean_y = 0.0;
        self.state_dev_x = 0.0;
        self.state_host_y = 0.0;
    }

    /// Returns the current clamped drift slope `alpha_hat`.
    pub fn drift_slope(&self) -> f64 {
        self.alpha_hat
    }

    /// Returns the number of anchors currently in the sliding window.
    pub fn anchor_count(&self) -> usize {
        self.anchors.len()
    }
}

/// Complete Tier 1 hardware-counter synchronization state.
#[derive(Clone, Debug)]
pub(crate) struct Tier1State {
    unwrapper: CounterUnwrapper,
    tracker: LinearDriftTracker,
    gap_reset_ns: u64,
    last_pkt_recv_ns: Option<TimestampNs>,
    last_emitted_ns: Option<TimestampNs>,
    last_dev_ns: Option<f64>,
    last_dt_ns: f64,
    last_phase_error_ns: f64,
    gap_resets: u64,
}

impl Tier1State {
    pub(crate) fn new(
        tick_hz: f64,
        counter_bits: u8,
        window_size: usize,
        ewma_alpha: f64,
        gap_reset_ns: u64,
    ) -> CoreResult<Self> {
        Ok(Self {
            unwrapper: CounterUnwrapper::new(tick_hz, counter_bits)?,
            tracker: LinearDriftTracker::new(window_size, ewma_alpha)?,
            gap_reset_ns,
            last_pkt_recv_ns: None,
            last_emitted_ns: None,
            last_dev_ns: None,
            last_dt_ns: 0.0,
            last_phase_error_ns: 0.0,
            gap_resets: 0,
        })
    }

    pub(crate) fn assign(
        &mut self,
        pkt_recv_ns: TimestampNs,
        hw_ticks: Option<&[u64]>,
        count: usize,
        out_ns: &mut [TimestampNs],
    ) -> CoreResult<()> {
        let ticks = hw_ticks.ok_or_else(|| {
            CoreError::Clock("Tier1HardwareCounter requires hw_ticks slice".to_owned())
        })?;
        if ticks.len() != count {
            return Err(CoreError::Clock(format!(
                "Tier1HardwareCounter hw_ticks length {} does not match sample count {count}",
                ticks.len()
            )));
        }

        let host_gap = self
            .last_pkt_recv_ns
            .is_some_and(|prev| pkt_recv_ns.saturating_sub(prev) > self.gap_reset_ns);
        let dev_gap = if !host_gap && self.last_pkt_recv_ns.is_some() {
            let predicted_dev_ns = self.unwrapper.peek_next_dev_ns(ticks[0]);
            let predicted_global = self.tracker.map_to_global_ns(predicted_dev_ns);
            (pkt_recv_ns as i128 - predicted_global as i128).unsigned_abs()
                > u128::from(self.gap_reset_ns.max(1_000_000_000))
        } else {
            false
        };
        let is_gap = host_gap || dev_gap;

        let first_dev_ns = if is_gap {
            self.gap_resets += 1;
            let dev0 = self.unwrapper.reanchor_to(ticks[0]);
            let safe_host_ns = self.last_emitted_ns.map_or(pkt_recv_ns, |p| pkt_recv_ns.max(p));
            self.tracker.reanchor_on_gap(dev0, safe_host_ns);
            self.last_phase_error_ns = 0.0;
            dev0
        } else {
            let predicted_dev_ns = self.unwrapper.peek_next_dev_ns(ticks[0]);
            let predicted_global = self.tracker.map_to_global_ns(predicted_dev_ns);
            self.last_phase_error_ns = if self.last_pkt_recv_ns.is_some() {
                (pkt_recv_ns as i128 - predicted_global as i128) as f64
            } else {
                0.0
            };

            let dev0 = self.unwrapper.push_raw(ticks[0]);
            let anchor_host_ns = if self.last_pkt_recv_ns.is_none() {
                self.last_emitted_ns.map_or(pkt_recv_ns, |p| pkt_recv_ns.max(p))
            } else {
                pkt_recv_ns
            };
            self.tracker.push_anchor(dev0, anchor_host_ns);
            dev0
        };

        let mut prev_ts = self.last_emitted_ns;
        let mut last_dev_ns = first_dev_ns;

        for (i, &tick) in ticks.iter().enumerate() {
            let dev_ns = if i == 0 {
                first_dev_ns
            } else {
                self.unwrapper.push_raw(tick)
            };
            last_dev_ns = dev_ns;

            let mapped = self.tracker.map_to_global_ns(dev_ns);
            let mono = prev_ts.map_or(mapped, |p| mapped.max(p));
            out_ns[i] = mono;
            prev_ts = Some(mono);
        }

        if count > 1 {
            let span_dev = last_dev_ns - first_dev_ns;
            if span_dev > 0.0 {
                self.last_dt_ns = (span_dev * self.tracker.drift_slope()) / ((count - 1) as f64);
            }
        } else if let Some(prev_dev) = self.last_dev_ns {
            if !is_gap && first_dev_ns > prev_dev {
                self.last_dt_ns = (first_dev_ns - prev_dev) * self.tracker.drift_slope();
            }
        }

        self.last_dev_ns = Some(last_dev_ns);
        self.last_pkt_recv_ns = Some(pkt_recv_ns);
        self.last_emitted_ns = prev_ts;
        Ok(())
    }

    pub(crate) fn reset(&mut self) {
        self.unwrapper.reset();
        self.tracker.reset();
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
        self.tracker.drift_slope()
    }

    pub(crate) fn phase_error_ns(&self) -> f64 {
        self.last_phase_error_ns
    }

    pub(crate) fn gap_resets(&self) -> u64 {
        self.gap_resets
    }

    pub(crate) fn locked(&self) -> bool {
        self.tracker.anchor_count() >= 4
    }
}
