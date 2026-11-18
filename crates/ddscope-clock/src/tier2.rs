//! Tier 2 second-order software PLL for bursty packet stream reconstruction.

use ddscope_types::{CoreError, CoreResult, TimestampNs};

/// Second-order proportional-integral software PLL reconstructing uniform sample grids
/// across bursty USB/Ethernet packet arrivals.
#[derive(Clone, Debug)]
pub struct SecondOrderPll {
    nominal_sample_rate_hz: Option<f64>,
    kp: f64,
    ki: f64,
    max_freq_deviation_ratio: f64,
    adaptive_window_ns: u64,
    gap_reset_ns: u64,

    dt_hat: f64,
    dt_nominal: Option<f64>,
    dt_min: f64,
    dt_max: f64,
    t_next_hat: f64,
    last_emitted_ns: Option<TimestampNs>,
    last_pkt_recv_ns: Option<TimestampNs>,
    last_phase_error_ns: f64,
    packet_count: u64,
    gap_resets: u64,

    // Adaptive frequency estimation state when `nominal_sample_rate_hz` is `None`.
    adaptive_done: bool,
    adaptive_start_recv_ns: TimestampNs,
    adaptive_cum_samples: u64,
    adaptive_anchors: Vec<(f64, f64)>,
}

impl SecondOrderPll {
    /// Creates a new [`SecondOrderPll`] with validated loop gains and window bounds.
    pub fn new(
        nominal_sample_rate_hz: Option<f64>,
        kp: f64,
        ki: f64,
        max_freq_deviation_ratio: f64,
        adaptive_window_ns: u64,
        gap_reset_ns: u64,
    ) -> CoreResult<Self> {
        if let Some(fs) = nominal_sample_rate_hz {
            if !fs.is_finite() || fs <= 0.0 {
                return Err(CoreError::InvalidConfig(format!(
                    "Tier2SoftwarePll nominal_sample_rate_hz must be finite and > 0, got {fs}"
                )));
            }
        }
        if !kp.is_finite() || !(0.0..1.0).contains(&kp) {
            return Err(CoreError::InvalidConfig(format!(
                "pll_kp must be finite and in [0.0, 1.0), got {kp}"
            )));
        }
        if !ki.is_finite() || !(0.0..1.0).contains(&ki) {
            return Err(CoreError::InvalidConfig(format!(
                "pll_ki must be finite and in [0.0, 1.0), got {ki}"
            )));
        }
        if !max_freq_deviation_ratio.is_finite() || !(0.0..=0.99).contains(&max_freq_deviation_ratio)
        {
            return Err(CoreError::InvalidConfig(format!(
                "pll_max_freq_deviation_ratio must be finite and in [0.0, 0.99], got {max_freq_deviation_ratio}"
            )));
        }
        if adaptive_window_ns == 0 || gap_reset_ns == 0 {
            return Err(CoreError::InvalidConfig(
                "adaptive_window_ns and gap_reset_ns must be > 0".to_owned(),
            ));
        }

        let (dt_hat, dt_nominal, dt_min, dt_max, adaptive_done) = match nominal_sample_rate_hz {
            Some(fs) => {
                let dt = 1e9 / fs;
                let min = (dt * (1.0 - max_freq_deviation_ratio)).max(1e-6);
                let max = dt * (1.0 + max_freq_deviation_ratio);
                (dt, Some(dt), min, max, true)
            }
            None => (0.0, None, 0.1, 1e9, false),
        };

        Ok(Self {
            nominal_sample_rate_hz,
            kp,
            ki,
            max_freq_deviation_ratio,
            adaptive_window_ns,
            gap_reset_ns,
            dt_hat,
            dt_nominal,
            dt_min,
            dt_max,
            t_next_hat: 0.0,
            last_emitted_ns: None,
            last_pkt_recv_ns: None,
            last_phase_error_ns: 0.0,
            packet_count: 0,
            gap_resets: 0,
            adaptive_done,
            adaptive_start_recv_ns: 0,
            adaptive_cum_samples: 0,
            adaptive_anchors: Vec::with_capacity(64),
        })
    }

    /// Assigns reconstructed monotonic timestamps for a burst of `count` samples arriving at `pkt_recv_ns`.
    pub(crate) fn assign(
        &mut self,
        pkt_recv_ns: TimestampNs,
        count: usize,
        out_ns: &mut [TimestampNs],
    ) {
        if count == 0 {
            return;
        }

        if !self.adaptive_done {
            self.assign_adaptive(pkt_recv_ns, count, out_ns);
            return;
        }

        if self.packet_count == 0 {
            let min_base = self.last_emitted_ns.map_or(0.0, |p| (p as f64) + 1.0);
            let t_base_hat = (pkt_recv_ns as f64).max(min_base);
            self.last_phase_error_ns = 0.0;
            self.emit_grid(t_base_hat, self.dt_hat, count, out_ns);
            self.t_next_hat = t_base_hat + (count as f64) * self.dt_hat;
            self.last_pkt_recv_ns = Some(pkt_recv_ns);
            self.packet_count = 1;
            return;
        }

        let e = (pkt_recv_ns as f64) - self.t_next_hat;
        self.last_phase_error_ns = e;
        let gap_threshold = (self.gap_reset_ns as f64).max(64.0 * (count as f64) * self.dt_hat);

        if e.abs() > gap_threshold {
            self.gap_resets += 1;
            let min_base = self.last_emitted_ns.map_or(0.0, |p| (p as f64) + 1.0);
            let t_base_hat = (pkt_recv_ns as f64).max(min_base);
            self.emit_grid(t_base_hat, self.dt_hat, count, out_ns);
            self.t_next_hat = t_base_hat + (count as f64) * self.dt_hat;
        } else {
            let t_base_hat = self.t_next_hat + self.kp * e;
            self.dt_hat =
                (self.dt_hat + self.ki * (e / (count as f64))).clamp(self.dt_min, self.dt_max);
            self.emit_grid(t_base_hat, self.dt_hat, count, out_ns);
            self.t_next_hat = t_base_hat + (count as f64) * self.dt_hat;
        }

        self.last_pkt_recv_ns = Some(pkt_recv_ns);
        self.packet_count += 1;
    }

    fn assign_adaptive(
        &mut self,
        pkt_recv_ns: TimestampNs,
        count: usize,
        out_ns: &mut [TimestampNs],
    ) {
        let min_base = self.last_emitted_ns.map_or(0.0, |p| (p as f64) + 1.0);

        if self.packet_count == 0 {
            self.start_adaptive_window(pkt_recv_ns, count, min_base, out_ns);
            self.packet_count = 1;
            return;
        }

        if self
            .last_pkt_recv_ns
            .is_some_and(|prev| pkt_recv_ns.saturating_sub(prev) > self.gap_reset_ns)
        {
            self.gap_resets += 1;
            self.start_adaptive_window(pkt_recv_ns, count, min_base, out_ns);
            self.packet_count += 1;
            return;
        }

        let x_curr = self.adaptive_cum_samples as f64;
        let rel_recv = (pkt_recv_ns as i128 - self.adaptive_start_recv_ns as i128) as f64;
        self.adaptive_anchors.push((x_curr, rel_recv));
        self.adaptive_cum_samples = self.adaptive_cum_samples.saturating_add(count as u64);
        self.last_pkt_recv_ns = Some(pkt_recv_ns);

        let (dt_est, beta_ols) = Self::fit_ols_dt(&self.adaptive_anchors);
        self.dt_hat = dt_est.clamp(0.1, 1e9);

        let window_complete = rel_recv >= self.adaptive_window_ns as f64
            && self.adaptive_anchors.len() >= 2;

        let t_base_hat = if window_complete {
            self.adaptive_done = true;
            self.dt_nominal = Some(self.dt_hat);
            self.dt_min = (self.dt_hat * (1.0 - self.max_freq_deviation_ratio)).max(1e-6);
            self.dt_max = self.dt_hat * (1.0 + self.max_freq_deviation_ratio);
            self.last_phase_error_ns =
                rel_recv - (beta_ols + self.dt_hat * x_curr);
            ((self.adaptive_start_recv_ns as f64) + beta_ols + self.dt_hat * x_curr).max(min_base)
        } else if self.adaptive_anchors.len() == 2 {
            self.last_phase_error_ns = 0.0;
            (pkt_recv_ns as f64).max(min_base)
        } else {
            let e = (pkt_recv_ns as f64) - self.t_next_hat;
            self.last_phase_error_ns = e;
            let kp_warmup = self.kp.max(0.05);
            (self.t_next_hat + kp_warmup * e).max(min_base)
        };

        self.emit_grid(t_base_hat, self.dt_hat, count, out_ns);
        self.t_next_hat = t_base_hat + (count as f64) * self.dt_hat;
        self.packet_count += 1;
    }

    fn start_adaptive_window(
        &mut self,
        pkt_recv_ns: TimestampNs,
        count: usize,
        min_base: f64,
        out_ns: &mut [TimestampNs],
    ) {
        self.adaptive_start_recv_ns = pkt_recv_ns;
        self.adaptive_anchors.clear();
        self.adaptive_anchors.push((0.0, 0.0));
        self.adaptive_cum_samples = count as u64;
        self.last_pkt_recv_ns = Some(pkt_recv_ns);
        self.last_phase_error_ns = 0.0;

        let t_base = (pkt_recv_ns as f64).max(min_base);
        // Use 1 ns spacing on the very first unknown-rate packet so timestamps remain strictly
        // monotonic without overshooting packet 2 even at very high sample rates.
        self.emit_grid(t_base, 1.0, count, out_ns);
        self.t_next_hat = t_base + (count as f64);
    }

    fn fit_ols_dt(anchors: &[(f64, f64)]) -> (f64, f64) {
        let n = anchors.len() as f64;
        let (sum_x, sum_y) = anchors
            .iter()
            .fold((0.0, 0.0), |(sx, sy), &(x, y)| (sx + x, sy + y));
        let mean_x = sum_x / n;
        let mean_y = sum_y / n;

        let (s_xx, s_xy) = anchors.iter().fold((0.0, 0.0), |(xx, xy), &(x, y)| {
            let dx = x - mean_x;
            let dy = y - mean_y;
            (xx + dx * dx, xy + dx * dy)
        });

        let slope = if s_xx > 0.0 {
            let s = s_xy / s_xx;
            if s > 0.0 { s } else { 1.0 }
        } else {
            1.0
        };
        let intercept = mean_y - slope * mean_x;
        (slope, intercept)
    }

    fn emit_grid(&mut self, t_base_hat: f64, dt: f64, count: usize, out_ns: &mut [TimestampNs]) {
        let raw_0 = t_base_hat.round().max(0.0) as u64;
        let mut prev = match self.last_emitted_ns {
            Some(last) => raw_0.max(last.saturating_add(1)),
            None => raw_0,
        };
        out_ns[0] = prev;

        for (i, slot) in out_ns.iter_mut().enumerate().take(count).skip(1) {
            let raw_i = (t_base_hat + (i as f64) * dt).round().max(0.0) as u64;
            let mono = raw_i.max(prev);
            *slot = mono;
            prev = mono;
        }
        self.last_emitted_ns = Some(prev);
    }

    /// Resets the PLL to its initial uninitialized state.
    pub fn reset(&mut self) {
        self.t_next_hat = 0.0;
        self.last_emitted_ns = None;
        self.last_pkt_recv_ns = None;
        self.last_phase_error_ns = 0.0;
        self.packet_count = 0;
        self.gap_resets = 0;
        self.adaptive_start_recv_ns = 0;
        self.adaptive_cum_samples = 0;
        self.adaptive_anchors.clear();

        match self.nominal_sample_rate_hz {
            Some(fs) => {
                let dt = 1e9 / fs;
                self.dt_hat = dt;
                self.dt_nominal = Some(dt);
                self.dt_min = (dt * (1.0 - self.max_freq_deviation_ratio)).max(1e-6);
                self.dt_max = dt * (1.0 + self.max_freq_deviation_ratio);
                self.adaptive_done = true;
            }
            None => {
                self.dt_hat = 0.0;
                self.dt_nominal = None;
                self.dt_min = 0.1;
                self.dt_max = 1e9;
                self.adaptive_done = false;
            }
        }
    }

    /// Re-anchors PLL phase tracking (e.g. after a file replay seek or stream restart)
    /// while preserving `last_emitted_ns` so emitted timestamps remain strictly monotonic.
    pub fn reanchor_preserve_monotonic(&mut self) {
        let preserved = self.last_emitted_ns;
        let gaps = self.gap_resets.saturating_add(1);
        self.t_next_hat = 0.0;
        self.last_pkt_recv_ns = None;
        self.last_phase_error_ns = 0.0;
        self.packet_count = 0;
        self.adaptive_start_recv_ns = 0;
        self.adaptive_cum_samples = 0;
        self.adaptive_anchors.clear();
        self.last_emitted_ns = preserved;
        self.gap_resets = gaps;
        if self.nominal_sample_rate_hz.is_none() && self.dt_hat <= 0.0 {
            self.adaptive_done = false;
        }
    }

    /// Seeds the monotonic timestamp floor before starting/replacing a stream session.
    pub fn seed_monotonic_floor(&mut self, floor_ns: TimestampNs) {
        self.last_emitted_ns = Some(
            self.last_emitted_ns
                .map_or(floor_ns, |prev| prev.max(floor_ns)),
        );
        self.reanchor_preserve_monotonic();
    }

    /// Returns the most recently emitted sample timestamp, if any.
    pub fn last_emitted_ns(&self) -> Option<TimestampNs> {
        self.last_emitted_ns
    }

    /// Returns the current estimated sample period in nanoseconds.
    pub fn estimated_dt_ns(&self) -> f64 {
        self.dt_hat
    }

    /// Returns the effective frequency ratio relative to nominal (`nominal_dt / dt_hat`), or `1.0`.
    pub fn drift_slope(&self) -> f64 {
        match self.dt_nominal {
            Some(nom) if self.dt_hat > 0.0 => nom / self.dt_hat,
            _ => 1.0,
        }
    }

    /// Returns the most recent phase error `t_recv - t_next_hat` in nanoseconds.
    pub fn phase_error_ns(&self) -> f64 {
        self.last_phase_error_ns
    }

    /// Returns the number of stream discontinuity resets observed.
    pub fn gap_resets(&self) -> u64 {
        self.gap_resets
    }

    /// Returns `true` when the PLL has completed any adaptive rate estimation and is tracking.
    pub fn is_locked(&self) -> bool {
        self.adaptive_done && self.packet_count >= 2
    }
}
