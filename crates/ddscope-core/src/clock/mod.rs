//! Three-tier hybrid clock synchronization and second-order software PLL contracts.

pub mod tier1;
pub mod tier2;

pub use tier1::{CounterUnwrapper, LinearDriftTracker, DRIFT_SLOPE_MAX, DRIFT_SLOPE_MIN};
pub use tier2::SecondOrderPll;

use serde::{Deserialize, Serialize};

use crate::types::{CoreError, CoreResult, DecodedBatch, TimestampNs};

/// Clock reconstruction strategy configured per data source.
#[derive(Copy, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum ClockMode {
    /// Tier 1: Hardware counter embedded in protocol frames, anchored to `host_mono_ns`.
    Tier1HardwareCounter { tick_hz: f64, counter_bits: u8 },
    /// Tier 2: Second-order software PLL reconstructing uniform sample spacing across bursty packets.
    Tier2SoftwarePll { nominal_sample_rate_hz: Option<f64> },
    /// Tier 3: Direct monotonic host arrival timestamps for asynchronous events.
    #[default]
    Tier3HostArrival,
}

/// Tunable parameters for [`StreamClockSynchronizer`].
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClockSyncConfig {
    /// Number of packet anchors retained in the Tier 1 sliding OLS window.
    pub drift_window_size: usize,
    /// First-order EWMA smoothing factor in `(0.0, 1.0]` applied to the Tier 1 drift slope.
    pub drift_ewma_alpha: f64,
    /// Proportional phase loop gain for the Tier 2 2nd-order software PLL.
    pub pll_kp: f64,
    /// Integral frequency loop gain for the Tier 2 2nd-order software PLL.
    pub pll_ki: f64,
    /// Maximum fractional sample-period deviation `±ratio` clamped around nominal `dt` in Tier 2.
    pub pll_max_freq_deviation_ratio: f64,
    /// Duration in nanoseconds of the initial adaptive rate-estimation window when `nominal_sample_rate_hz == None`.
    pub adaptive_window_ns: u64,
    /// Stream discontinuity threshold in nanoseconds triggering clean phase re-anchoring.
    pub gap_reset_ns: u64,
}

impl Default for ClockSyncConfig {
    fn default() -> Self {
        Self {
            drift_window_size: 64,
            drift_ewma_alpha: 0.2,
            pll_kp: 0.0015,
            pll_ki: 1.5e-6,
            pll_max_freq_deviation_ratio: 0.05,
            adaptive_window_ns: 50_000_000,
            gap_reset_ns: 250_000_000,
        }
    }
}

/// Live telemetry snapshot exposed by [`StreamClockSynchronizer::diagnostics`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ClockSyncDiagnostics {
    /// Reconstructed sample-to-sample interval in nanoseconds (`0.0` if undetermined or Tier 3).
    pub estimated_dt_ns: f64,
    /// Reconstructed effective sample rate in Hz (`1e9 / estimated_dt_ns`, or `0.0`).
    pub estimated_rate_hz: f64,
    /// Estimated clock drift slope (`alpha_hat` in Tier 1, `dt_nominal / dt_hat` in Tier 2, `1.0` in Tier 3).
    pub drift_slope: f64,
    /// Most recent packet phase error `t_recv - t_predicted` in nanoseconds.
    pub phase_error_ns: f64,
    /// Cumulative number of stream-gap re-anchors performed.
    pub gap_resets: u64,
    /// Whether the synchronizer has acquired lock (window warmed up / adaptive rate finalized).
    pub locked: bool,
}

#[derive(Clone, Debug)]
enum SyncEngine {
    Tier1(tier1::Tier1State),
    Tier2(SecondOrderPll),
    Tier3 { last_emitted_ns: Option<TimestampNs> },
}

/// Three-tier hybrid clock synchronizer assigning strictly monotonic nanosecond timestamps
/// to decoded sample batches.
#[derive(Clone, Debug)]
pub struct StreamClockSynchronizer {
    mode: ClockMode,
    engine: SyncEngine,
    recv_offset_ns: TimestampNs,
    reanchor_pending: bool,
}

impl StreamClockSynchronizer {
    /// Creates a synchronizer for `mode` using [`ClockSyncConfig::default()`].
    pub fn new(mode: ClockMode) -> CoreResult<Self> {
        Self::with_config(mode, ClockSyncConfig::default())
    }

    /// Creates a synchronizer for `mode` with explicit `config` parameters.
    pub fn with_config(mode: ClockMode, config: ClockSyncConfig) -> CoreResult<Self> {
        let engine = match mode {
            ClockMode::Tier1HardwareCounter {
                tick_hz,
                counter_bits,
            } => {
                if config.gap_reset_ns == 0 {
                    return Err(CoreError::InvalidConfig(
                        "gap_reset_ns must be > 0".to_owned(),
                    ));
                }
                SyncEngine::Tier1(tier1::Tier1State::new(
                    tick_hz,
                    counter_bits,
                    config.drift_window_size,
                    config.drift_ewma_alpha,
                    config.gap_reset_ns,
                )?)
            }
            ClockMode::Tier2SoftwarePll {
                nominal_sample_rate_hz,
            } => SyncEngine::Tier2(SecondOrderPll::new(
                nominal_sample_rate_hz,
                config.pll_kp,
                config.pll_ki,
                config.pll_max_freq_deviation_ratio,
                config.adaptive_window_ns,
                config.gap_reset_ns,
            )?),
            ClockMode::Tier3HostArrival => SyncEngine::Tier3 {
                last_emitted_ns: None,
            },
        };

        Ok(Self {
            mode,
            engine,
            recv_offset_ns: 0,
            reanchor_pending: false,
        })
    }

    fn last_emitted_ns(&self) -> Option<TimestampNs> {
        match &self.engine {
            SyncEngine::Tier1(state) => state.last_emitted_ns(),
            SyncEngine::Tier2(pll) => pll.last_emitted_ns(),
            SyncEngine::Tier3 { last_emitted_ns } => *last_emitted_ns,
        }
    }

    /// Assigns `count` strictly non-regressing nanosecond timestamps into `out_ns[..count]`.
    pub fn assign_timestamps(
        &mut self,
        pkt_recv_ns: TimestampNs,
        hw_ticks: Option<&[u64]>,
        count: usize,
        out_ns: &mut [TimestampNs],
    ) -> CoreResult<()> {
        if out_ns.len() < count {
            return Err(CoreError::Clock(format!(
                "output timestamp slice length {} is smaller than sample count {count}",
                out_ns.len()
            )));
        }
        if count == 0 {
            return Ok(());
        }

        if self.reanchor_pending {
            self.reanchor_pending = false;
            if let Some(floor) = self.last_emitted_ns() {
                if pkt_recv_ns <= floor {
                    self.recv_offset_ns = floor.saturating_add(1).saturating_sub(pkt_recv_ns);
                } else {
                    self.recv_offset_ns = 0;
                }
            }
        }
        let effective_recv_ns = pkt_recv_ns.saturating_add(self.recv_offset_ns);

        match &mut self.engine {
            SyncEngine::Tier1(state) => {
                state.assign(effective_recv_ns, hw_ticks, count, out_ns)
            }
            SyncEngine::Tier2(pll) => {
                pll.assign(effective_recv_ns, count, out_ns);
                Ok(())
            }
            SyncEngine::Tier3 { last_emitted_ns } => {
                let ts = last_emitted_ns
                    .map_or(effective_recv_ns, |prev| effective_recv_ns.max(prev));
                out_ns[..count].fill(ts);
                *last_emitted_ns = Some(ts);
                Ok(())
            }
        }
    }

    /// Populates `batch.timestamps_ns` for `batch.sample_rows` without reallocating once warm.
    pub fn populate_batch_timestamps(&mut self, batch: &mut DecodedBatch) -> CoreResult<()> {
        let count = batch.sample_rows;
        if count == 0 {
            batch.timestamps_ns.clear();
            return Ok(());
        }

        if batch.timestamps_ns.len() != count {
            batch.timestamps_ns.resize(count, 0);
        }

        let hw_ticks = if batch.hw_ticks.is_empty() {
            None
        } else {
            Some(batch.hw_ticks.as_slice())
        };

        if let Err(err) =
            self.assign_timestamps(batch.packet_recv_ns, hw_ticks, count, &mut batch.timestamps_ns)
        {
            batch.timestamps_ns.clear();
            return Err(err);
        }
        Ok(())
    }

    /// Resets internal clock tracking, unwrapper, and PLL state while preserving mode and config.
    pub fn reset(&mut self) {
        self.recv_offset_ns = 0;
        self.reanchor_pending = false;
        match &mut self.engine {
            SyncEngine::Tier1(state) => state.reset(),
            SyncEngine::Tier2(pll) => pll.reset(),
            SyncEngine::Tier3 { last_emitted_ns } => *last_emitted_ns = None,
        }
    }

    /// Re-anchors phase and counter state (e.g. upon replay seek or stream reconnect)
    /// while preserving the monotonic floor `last_emitted_ns` so storage timestamps never regress.
    pub fn reanchor_preserve_monotonic(&mut self) {
        self.reanchor_pending = true;
        match &mut self.engine {
            SyncEngine::Tier1(state) => state.reanchor_preserve_monotonic(),
            SyncEngine::Tier2(pll) => pll.reanchor_preserve_monotonic(),
            SyncEngine::Tier3 { .. } => {}
        }
    }

    /// Returns the configured [`ClockMode`].
    pub fn mode(&self) -> ClockMode {
        self.mode
    }

    /// Returns a snapshot of clock synchronization diagnostics.
    pub fn diagnostics(&self) -> ClockSyncDiagnostics {
        match &self.engine {
            SyncEngine::Tier1(state) => {
                let dt = state.estimated_dt_ns();
                ClockSyncDiagnostics {
                    estimated_dt_ns: dt,
                    estimated_rate_hz: if dt > 0.0 { 1e9 / dt } else { 0.0 },
                    drift_slope: state.drift_slope(),
                    phase_error_ns: state.phase_error_ns(),
                    gap_resets: state.gap_resets(),
                    locked: state.locked(),
                }
            }
            SyncEngine::Tier2(pll) => {
                let dt = pll.estimated_dt_ns();
                ClockSyncDiagnostics {
                    estimated_dt_ns: dt,
                    estimated_rate_hz: if dt > 0.0 { 1e9 / dt } else { 0.0 },
                    drift_slope: pll.drift_slope(),
                    phase_error_ns: pll.phase_error_ns(),
                    gap_resets: pll.gap_resets(),
                    locked: pll.is_locked(),
                }
            }
            SyncEngine::Tier3 { last_emitted_ns } => ClockSyncDiagnostics {
                estimated_dt_ns: 0.0,
                estimated_rate_hz: 0.0,
                drift_slope: 1.0,
                phase_error_ns: 0.0,
                gap_resets: 0,
                locked: last_emitted_ns.is_some(),
            },
        }
    }
}
