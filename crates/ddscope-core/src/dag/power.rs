//! Cycle-synchronized IEEE 1459 single-phase power analyzer state machine in `f64`.

use std::f64::consts::TAU;

use crate::types::TimestampNs;

const MAX_CYCLE_BINS: usize = 1024;
const HOLD_EMIT_INTERVAL_NS: u64 = 2_000_000;

/// Full 12-metric IEEE 1459 single-phase power measurement snapshot.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct SinglePhasePowerSample {
    pub ts_ns: TimestampNs,
    pub freq: f64,
    pub u_rms: f64,
    pub i_rms: f64,
    pub p: f64,
    pub q1: f64,
    pub q_tot: f64,
    pub s: f64,
    pub pf: f64,
    pub phi_deg: f64,
    pub u_pk: f64,
    pub i_pk: f64,
    pub energy_wh: f64,
}

#[derive(Copy, Clone, Debug, Default)]
struct CycleBin {
    dt_sec: f64,
    u_dt: f64,
    i_dt: f64,
}

#[derive(Clone, Debug)]
struct CycleAccumulator {
    duration_sec: f64,
    u2_dt: f64,
    i2_dt: f64,
    p_dt: f64,
    u_pk: f64,
    i_pk: f64,
    bins: Vec<CycleBin>,
    merge_stride: usize,
    pending_bin: CycleBin,
    pending_count: usize,
}

impl Default for CycleAccumulator {
    fn default() -> Self {
        Self {
            duration_sec: 0.0,
            u2_dt: 0.0,
            i2_dt: 0.0,
            p_dt: 0.0,
            u_pk: 0.0,
            i_pk: 0.0,
            bins: Vec::with_capacity(MAX_CYCLE_BINS),
            merge_stride: 1,
            pending_bin: CycleBin::default(),
            pending_count: 0,
        }
    }
}

impl CycleAccumulator {
    fn reset(&mut self) {
        self.duration_sec = 0.0;
        self.u2_dt = 0.0;
        self.i2_dt = 0.0;
        self.p_dt = 0.0;
        self.u_pk = 0.0;
        self.i_pk = 0.0;
        self.bins.clear();
        self.merge_stride = 1;
        self.pending_bin = CycleBin::default();
        self.pending_count = 0;
    }

    #[inline]
    fn push_segment(&mut self, u0: f64, u1: f64, i0: f64, i1: f64, dt_sec: f64, record_bins: bool) {
        if dt_sec <= 0.0 || !dt_sec.is_finite() {
            return;
        }
        self.duration_sec += dt_sec;
        // Trapezoidal integration yields O(dt^2) accuracy across periodic waveforms
        self.u2_dt += 0.5 * (u0 * u0 + u1 * u1) * dt_sec;
        self.i2_dt += 0.5 * (i0 * i0 + i1 * i1) * dt_sec;
        self.p_dt += 0.5 * (u0 * i0 + u1 * i1) * dt_sec;
        self.u_pk = self.u_pk.max(u0.abs()).max(u1.abs());
        self.i_pk = self.i_pk.max(i0.abs()).max(i1.abs());

        if !record_bins {
            return;
        }

        let u_mid = 0.5 * (u0 + u1);
        let i_mid = 0.5 * (i0 + i1);
        self.pending_bin.dt_sec += dt_sec;
        self.pending_bin.u_dt += u_mid * dt_sec;
        self.pending_bin.i_dt += i_mid * dt_sec;
        self.pending_count += 1;

        if self.pending_count >= self.merge_stride {
            let finished = self.pending_bin;
            self.pending_bin = CycleBin::default();
            self.pending_count = 0;
            self.bins.push(finished);
            if self.bins.len() >= MAX_CYCLE_BINS {
                self.coalesce_bins_by_2();
            }
        }
    }

    fn coalesce_bins_by_2(&mut self) {
        let mut write = 0usize;
        let len = self.bins.len();
        let mut read = 0usize;
        while read + 1 < len {
            let a = self.bins[read];
            let b = self.bins[read + 1];
            self.bins[write] = CycleBin {
                dt_sec: a.dt_sec + b.dt_sec,
                u_dt: a.u_dt + b.u_dt,
                i_dt: a.i_dt + b.i_dt,
            };
            write += 1;
            read += 2;
        }
        if read < len {
            self.bins[write] = self.bins[read];
            write += 1;
        }
        self.bins.truncate(write);
        self.merge_stride = self.merge_stride.saturating_mul(2);
    }

    fn finalize_ac(&mut self, freq_hz: f64, energy_wh: f64, ts_ns: TimestampNs) -> SinglePhasePowerSample {
        if self.pending_count > 0 && self.pending_bin.dt_sec > 0.0 {
            self.bins.push(self.pending_bin);
            self.pending_bin = CycleBin::default();
            self.pending_count = 0;
        }

        let t_total = self.duration_sec.max(1e-12);
        let u_rms = (self.u2_dt / t_total).max(0.0).sqrt();
        let i_rms = (self.i2_dt / t_total).max(0.0).sqrt();
        let p = self.p_dt / t_total;
        let s = u_rms * i_rms;
        let q_tot = (s * s - p * p).max(0.0).sqrt();
        let pf = if s > 1e-12 {
            (p / s).clamp(-1.0, 1.0)
        } else {
            0.0
        };

        // Single-bin DFT at fundamental omega_1 = 2 * pi / T over exact cycle duration
        let omega1 = TAU / t_total;
        let mut tau_acc = 0.0;
        let mut u1_c = 0.0;
        let mut u1_s = 0.0;
        let mut i1_c = 0.0;
        let mut i1_s = 0.0;
        for bin in &self.bins {
            let tau_mid = tau_acc + 0.5 * bin.dt_sec;
            tau_acc += bin.dt_sec;
            let (sin_w, cos_w) = (omega1 * tau_mid).sin_cos();
            u1_c += bin.u_dt * cos_w;
            u1_s += bin.u_dt * sin_w;
            i1_c += bin.i_dt * cos_w;
            i1_s += bin.i_dt * sin_w;
        }
        let scale = 2.0 / t_total;
        u1_c *= scale;
        u1_s *= scale;
        i1_c *= scale;
        i1_s *= scale;

        let p1 = 0.5 * (u1_c * i1_c + u1_s * i1_s);
        // Inductive (current lags voltage) => positive Q1 and positive phi_deg
        let q1 = 0.5 * (u1_c * i1_s - u1_s * i1_c);
        let u1_mag = u1_c.hypot(u1_s);
        let i1_mag = i1_c.hypot(i1_s);
        let phi_deg = if u1_mag * i1_mag > 1e-12 {
            q1.atan2(p1).to_degrees()
        } else {
            0.0
        };

        SinglePhasePowerSample {
            ts_ns,
            freq: freq_hz,
            u_rms,
            i_rms,
            p,
            q1,
            q_tot,
            s,
            pf,
            phi_deg,
            u_pk: self.u_pk,
            i_pk: self.i_pk,
            energy_wh,
        }
    }

    fn finalize_dc(&self, energy_wh: f64, ts_ns: TimestampNs) -> SinglePhasePowerSample {
        let t_total = self.duration_sec.max(1e-12);
        let u_rms = (self.u2_dt / t_total).max(0.0).sqrt();
        let i_rms = (self.i2_dt / t_total).max(0.0).sqrt();
        let p = self.p_dt / t_total;
        let s = u_rms * i_rms;
        let q_tot = (s * s - p * p).max(0.0).sqrt();
        let pf = if s > 1e-12 {
            (p / s).clamp(-1.0, 1.0)
        } else {
            0.0
        };

        SinglePhasePowerSample {
            ts_ns,
            freq: 0.0,
            u_rms,
            i_rms,
            p,
            q1: 0.0,
            q_tot,
            s,
            pf,
            phi_deg: 0.0,
            u_pk: self.u_pk,
            i_pk: self.i_pk,
            energy_wh,
        }
    }
}

/// Parameters governing [`SinglePhasePowerState`].
#[derive(Copy, Clone, Debug)]
pub struct SinglePhasePowerParams {
    pub voltage_scale: f64,
    pub voltage_offset: f64,
    pub current_scale: f64,
    pub current_offset: f64,
    pub nominal_freq_hz: f64,
    pub zero_cross_hysteresis_v: f64,
    pub min_freq_hz: f64,
    pub fallback_window_ms: f64,
}

/// Stateful IEEE 1459 cycle-synchronized single-phase power analyzer running in `f64`.
#[derive(Clone, Debug)]
pub struct SinglePhasePowerState {
    params: SinglePhasePowerParams,
    tau_filt_sec: f64,
    min_period_ns: u64,
    max_cycle_ns: u64,
    fallback_window_ns: u64,

    initialized: bool,
    prev_ts_ns: TimestampNs,
    prev_u: f64,
    prev_i: f64,
    prev_u_filt: f64,
    tracked_u_pk: f64,
    armed_neg: bool,

    window_start_ns: TimestampNs,
    last_zc_ns: Option<TimestampNs>,
    last_ac_emit_ns: Option<TimestampNs>,
    fallback_start_ns: TimestampNs,
    last_emitted_ts_ns: Option<TimestampNs>,
    last_sample: Option<SinglePhasePowerSample>,

    energy_wh: f64,
    cycle_acc: CycleAccumulator,
    fallback_acc: CycleAccumulator,
}

impl SinglePhasePowerState {
    /// Creates a new [`SinglePhasePowerState`] from validated parameters.
    pub fn new(params: SinglePhasePowerParams) -> Self {
        let nom = params.nominal_freq_hz.max(0.1);
        let min_f = params.min_freq_hz.max(0.01);
        let fc = 4.0 * nom;
        let tau_filt_sec = 1.0 / (TAU * fc);
        let min_period_ns = (1e9 / (2000.0_f64.max(20.0 * nom))).round().max(1.0) as u64;
        let max_cycle_ns = (1e9 / min_f).round().max(1.0) as u64;
        let fallback_window_ns = (params.fallback_window_ms.max(0.1) * 1e6).round().max(1.0) as u64;

        Self {
            params,
            tau_filt_sec,
            min_period_ns,
            max_cycle_ns,
            fallback_window_ns,
            initialized: false,
            prev_ts_ns: 0,
            prev_u: 0.0,
            prev_i: 0.0,
            prev_u_filt: 0.0,
            tracked_u_pk: 0.0,
            armed_neg: false,
            window_start_ns: 0,
            last_zc_ns: None,
            last_ac_emit_ns: None,
            fallback_start_ns: 0,
            last_emitted_ts_ns: None,
            last_sample: None,
            energy_wh: 0.0,
            cycle_acc: CycleAccumulator::default(),
            fallback_acc: CycleAccumulator::default(),
        }
    }

    /// Resets all accumulated state (e.g. before retroactive backfill).
    pub fn reset(&mut self) {
        self.initialized = false;
        self.prev_ts_ns = 0;
        self.prev_u = 0.0;
        self.prev_i = 0.0;
        self.prev_u_filt = 0.0;
        self.tracked_u_pk = 0.0;
        self.armed_neg = false;
        self.window_start_ns = 0;
        self.last_zc_ns = None;
        self.last_ac_emit_ns = None;
        self.fallback_start_ns = 0;
        self.last_emitted_ts_ns = None;
        self.last_sample = None;
        self.energy_wh = 0.0;
        self.cycle_acc.reset();
        self.fallback_acc.reset();
    }

    /// Advances the power analyzer by one `(ts_ns, raw_u, raw_i)` sample, pushing any emitted
    /// [`SinglePhasePowerSample`] snapshots into `out`.
    pub fn process_sample(
        &mut self,
        ts_ns: TimestampNs,
        raw_u: f64,
        raw_i: f64,
        out: &mut Vec<SinglePhasePowerSample>,
    ) {
        let safe_u = if raw_u.is_finite() { raw_u } else { 0.0 };
        let safe_i = if raw_i.is_finite() { raw_i } else { 0.0 };
        let u = safe_u * self.params.voltage_scale + self.params.voltage_offset;
        let i = safe_i * self.params.current_scale + self.params.current_offset;

        if !self.initialized {
            self.initialized = true;
            self.prev_ts_ns = ts_ns;
            self.prev_u = u;
            self.prev_i = i;
            self.prev_u_filt = u;
            self.tracked_u_pk = u.abs();
            self.armed_neg = u < 0.0;
            self.window_start_ns = ts_ns;
            self.fallback_start_ns = ts_ns;
            return;
        }

        let dt_ns = ts_ns.saturating_sub(self.prev_ts_ns);
        if dt_ns == 0 {
            self.prev_u = u;
            self.prev_i = i;
            return;
        }

        let dt_sec = dt_ns as f64 * 1e-9;
        self.tracked_u_pk =
            (self.tracked_u_pk * (-dt_sec * self.params.nominal_freq_hz * 0.5).exp()).max(u.abs());

        let p_inst = 0.5 * (self.prev_u * self.prev_i + u * i);
        self.energy_wh += (p_inst * dt_sec) / 3600.0;

        let alpha = dt_sec / (self.tau_filt_sec + dt_sec);
        let u_filt = self.prev_u_filt + alpha * (u - self.prev_u_filt);
        let hyst = if self.params.zero_cross_hysteresis_v > 0.0 {
            self.params.zero_cross_hysteresis_v
        } else {
            (0.05 * self.tracked_u_pk).max(1e-3)
        };

        if !self.armed_neg && u_filt <= -hyst {
            self.armed_neg = true;
        }

        let mut cycle_completed = false;
        if self.armed_neg && u_filt >= hyst {
            let denom = u_filt - self.prev_u_filt;
            let frac = if denom.abs() > 1e-12 {
                ((hyst - self.prev_u_filt) / denom).clamp(0.0, 1.0)
            } else {
                0.5
            };
            let t_zc_ns = self.prev_ts_ns + ((dt_ns as f64) * frac).round() as u64;

            if self
                .last_zc_ns
                .is_none_or(|prev| t_zc_ns.saturating_sub(prev) >= self.min_period_ns)
            {
                self.armed_neg = false;
                let u_zc = self.prev_u + (u - self.prev_u) * frac;
                let i_zc = self.prev_i + (i - self.prev_i) * frac;
                let dt_first = t_zc_ns.saturating_sub(self.prev_ts_ns) as f64 * 1e-9;
                let dt_second = ts_ns.saturating_sub(t_zc_ns) as f64 * 1e-9;

                self.cycle_acc
                    .push_segment(self.prev_u, u_zc, self.prev_i, i_zc, dt_first, true);
                self.fallback_acc
                    .push_segment(self.prev_u, u_zc, self.prev_i, i_zc, dt_first, false);

                if let Some(prev_zc) = self.last_zc_ns {
                    let period_ns = t_zc_ns.saturating_sub(prev_zc);
                    if period_ns > 0 {
                        let freq_hz = 1e9 / (period_ns as f64);
                        if freq_hz >= self.params.min_freq_hz {
                            let emit_ts = self
                                .last_emitted_ts_ns
                                .map_or(t_zc_ns, |prev| t_zc_ns.max(prev));
                            // Energy up to t_zc_ns excludes the tail segment energy for this instant
                            let tail_energy = (0.5 * (u_zc * i_zc + u * i) * dt_second) / 3600.0;
                            let sample = self.cycle_acc.finalize_ac(
                                freq_hz,
                                self.energy_wh - tail_energy,
                                emit_ts,
                            );
                            self.last_emitted_ts_ns = Some(emit_ts);
                            self.last_ac_emit_ns = Some(t_zc_ns);
                            self.last_sample = Some(sample);
                            out.push(sample);
                            cycle_completed = true;
                        }
                    }
                }

                self.last_zc_ns = Some(t_zc_ns);
                self.cycle_acc.reset();
                self.cycle_acc
                    .push_segment(u_zc, u, i_zc, i, dt_second, true);

                if cycle_completed {
                    self.fallback_acc.reset();
                    self.fallback_start_ns = t_zc_ns;
                }
                self.fallback_acc
                    .push_segment(u_zc, u, i_zc, i, dt_second, false);
            }
        }

        if !cycle_completed && self.last_zc_ns.is_none_or(|zc| zc < self.prev_ts_ns) {
            // Normal segment when no zero crossing occurred in this step
            self.cycle_acc
                .push_segment(self.prev_u, u, self.prev_i, i, dt_sec, true);
            self.fallback_acc
                .push_segment(self.prev_u, u, self.prev_i, i, dt_sec, false);
        }

        let no_recent_ac = match self.last_ac_emit_ns {
            Some(last_ac) => ts_ns.saturating_sub(last_ac) >= self.max_cycle_ns,
            None => ts_ns.saturating_sub(self.window_start_ns) >= self.max_cycle_ns,
        };

        if no_recent_ac && ts_ns.saturating_sub(self.fallback_start_ns) >= self.fallback_window_ns
        {
            let emit_ts = self
                .last_emitted_ts_ns
                .map_or(ts_ns, |prev| ts_ns.max(prev));
            let sample = self.fallback_acc.finalize_dc(self.energy_wh, emit_ts);
            self.last_emitted_ts_ns = Some(emit_ts);
            self.last_sample = Some(sample);
            self.fallback_acc.reset();
            self.fallback_start_ns = ts_ns;
            out.push(sample);
        } else if !cycle_completed {
            if let Some(mut held) = self.last_sample {
                if self
                    .last_emitted_ts_ns
                    .is_some_and(|prev| ts_ns.saturating_sub(prev) >= HOLD_EMIT_INTERVAL_NS)
                {
                    held.ts_ns = ts_ns;
                    held.energy_wh = self.energy_wh;
                    self.last_emitted_ts_ns = Some(ts_ns);
                    out.push(held);
                }
            }
        }

        self.prev_ts_ns = ts_ns;
        self.prev_u = u;
        self.prev_i = i;
        self.prev_u_filt = u_filt;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::{PI, SQRT_2};

    fn default_params() -> SinglePhasePowerParams {
        SinglePhasePowerParams {
            voltage_scale: 1.0,
            voltage_offset: 0.0,
            current_scale: 1.0,
            current_offset: 0.0,
            nominal_freq_hz: 50.0,
            zero_cross_hysteresis_v: 0.0,
            min_freq_hz: 5.0,
            fallback_window_ms: 100.0,
        }
    }

    #[test]
    fn power_analyzer_inductive_and_capacitive_ac_accuracy() {
        for &(phi_deg_expected, sign_label) in &[(30.0_f64, "inductive"), (-45.0_f64, "capacitive")]
        {
            let mut st = SinglePhasePowerState::new(default_params());
            let mut out = Vec::new();

            let u_rms_exp = 220.0;
            let i_rms_exp = 10.0;
            let u_amp = u_rms_exp * SQRT_2;
            let i_amp = i_rms_exp * SQRT_2;
            let phi_rad = phi_deg_expected.to_radians();
            let fs = 20_000.0; // 20 kHz
            let dt_ns = 50_000_u64;

            for k in 0..4_000 {
                // 200ms = 10 cycles @ 50Hz
                let t = (k as f64) / fs;
                let u = u_amp * (TAU * 50.0 * t).sin();
                let i = i_amp * (TAU * 50.0 * t - phi_rad).sin();
                st.process_sample(k * dt_ns, u, i, &mut out);
            }

            let last = out.last().expect("expected AC cycle samples");
            let p_exp = u_rms_exp * i_rms_exp * phi_rad.cos();
            let q_exp = u_rms_exp * i_rms_exp * phi_rad.sin();
            let s_exp = u_rms_exp * i_rms_exp;
            let pf_exp = phi_rad.cos();

            assert!(
                (last.freq - 50.0).abs() < 0.05,
                "{sign_label} freq: got {}",
                last.freq
            );
            assert!(
                (last.u_rms - u_rms_exp).abs() < 0.3,
                "{sign_label} u_rms: got {}",
                last.u_rms
            );
            assert!(
                (last.i_rms - i_rms_exp).abs() < 0.03,
                "{sign_label} i_rms: got {}",
                last.i_rms
            );
            assert!(
                (last.p - p_exp).abs() < 5.0,
                "{sign_label} p: exp {p_exp}, got {}",
                last.p
            );
            assert!(
                (last.q1 - q_exp).abs() < 5.0,
                "{sign_label} q1: exp {q_exp}, got {}",
                last.q1
            );
            assert!(
                (last.s - s_exp).abs() < 5.0,
                "{sign_label} s: exp {s_exp}, got {}",
                last.s
            );
            assert!(
                (last.pf - pf_exp).abs() < 0.005,
                "{sign_label} pf: exp {pf_exp}, got {}",
                last.pf
            );
            assert!(
                (last.phi_deg - phi_deg_expected).abs() < 0.5,
                "{sign_label} phi_deg: exp {phi_deg_expected}, got {}",
                last.phi_deg
            );
            assert!((last.u_pk - u_amp).abs() < 0.5);
            assert!((last.i_pk - i_amp).abs() < 0.05);
            let expected_wh = p_exp * (0.2 / 3600.0);
            assert!((last.energy_wh - expected_wh).abs() < expected_wh * 0.02);
        }
    }

    #[test]
    fn power_analyzer_distinguishes_harmonic_distortion_reactive_power() {
        let mut st = SinglePhasePowerState::new(default_params());
        let mut out = Vec::new();
        let fs = 20_000.0;
        let dt_ns = 50_000_u64;
        let u_amp = 220.0 * SQRT_2;
        let i1_amp = 10.0 * SQRT_2;
        let i3_amp = 4.0 * SQRT_2;

        for k in 0..4_000 {
            let t = (k as f64) / fs;
            let u = u_amp * (TAU * 50.0 * t).sin();
            // Fundamental current in phase (phi1 = 0 => Q1 = 0), plus 3rd harmonic current
            let i = i1_amp * (TAU * 50.0 * t).sin() + i3_amp * (TAU * 150.0 * t + PI / 4.0).sin();
            st.process_sample(k * dt_ns, u, i, &mut out);
        }

        let last = out.last().unwrap();
        assert!(last.q1.abs() < 5.0, "expected Q1 ~ 0, got {}", last.q1);
        assert!(
            last.q_tot > 800.0,
            "expected Q_tot > 800 from 3rd harmonic, got {}",
            last.q_tot
        );
        assert!(last.pf < 0.95);
    }

    #[test]
    fn power_analyzer_dc_fallback_and_energy() {
        let mut st = SinglePhasePowerState::new(default_params());
        let mut out = Vec::new();
        for k in 0..=500 {
            st.process_sample(k * 1_000_000, 48.0, 5.0, &mut out);
        }
        let last = out.last().expect("expected DC fallback samples");
        assert_eq!(last.freq, 0.0);
        assert_eq!(last.q1, 0.0);
        assert_eq!(last.phi_deg, 0.0);
        assert!((last.u_rms - 48.0).abs() < 1e-6);
        assert!((last.i_rms - 5.0).abs() < 1e-6);
        assert!((last.p - 240.0).abs() < 1e-6);
        assert!((last.s - 240.0).abs() < 1e-6);
        assert!((last.pf - 1.0).abs() < 1e-6);
        assert!((last.energy_wh - 240.0 * (0.5 / 3600.0)).abs() < 1e-6);
    }
}
