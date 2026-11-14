//! Independent per-scope trigger state machine, Schmitt-trigger hysteresis debouncing,
//! sub-sample linear interpolation, and acquisition window tracking.
//!
//! Conforms to Phase 1 Track A Milestone TA4 specification in `docs/04-phase1-design-and-plan.md` §3.5.

use serde::{Deserialize, Serialize};

use crate::scope::{EdgeSlope, TriggerCondition, TriggerRuntimeState, TriggerSweepMode};
use crate::types::{ChannelId, ScopeId, SyncGroupId, TimestampNs};

/// Record of a confirmed trigger event with sub-sample linear interpolation offset.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TriggerHit {
    /// Discrete sample timestamp immediately preceding or at the trigger threshold crossing.
    pub timestamp_ns: TimestampNs,
    /// Fractional sub-sample linear interpolation offset in nanoseconds:
    /// `subpixel_offset_ns = alpha * (t_k - t_{k-1})`.
    pub subpixel_offset_ns: f32,
}

impl TriggerHit {
    /// Constructs a new [`TriggerHit`].
    #[inline]
    pub const fn new(timestamp_ns: TimestampNs, subpixel_offset_ns: f32) -> Self {
        Self {
            timestamp_ns,
            subpixel_offset_ns,
        }
    }

    /// Returns the exact sub-sample interpolated timestamp in nanoseconds as `f64`.
    #[inline]
    pub fn exact_timestamp_ns(&self) -> f64 {
        self.timestamp_ns as f64 + self.subpixel_offset_ns as f64
    }

    /// Returns the interpolated timestamp rounded to the nearest integer nanosecond.
    #[inline]
    pub fn interpolated_ts_u64(&self) -> u64 {
        self.timestamp_ns
            .saturating_add(self.subpixel_offset_ns.round() as u64)
    }
}

/// Calculates the normalized linear interpolation factor $\alpha \in [0.0, 1.0]$
/// and the sub-sample offset in nanoseconds relative to `t0`.
///
/// Formula: $\alpha = \frac{\text{level} - v_0}{v_1 - v_0}$, clamped to $[0.0, 1.0]$.
/// Subpixel offset: $\Delta t_{\text{sub}} = (\alpha \times (t_1 - t_0) \text{ as f64}) \text{ as f32}$.
///
/// Handles all IEEE 754 float edge cases (NaN/Inf, zero or tiny denominator, boundary touches).
#[inline]
pub fn calculate_subsample_offset(
    t0: TimestampNs,
    v0: f64,
    t1: TimestampNs,
    v1: f64,
    level: f64,
) -> Option<(f64, f32)> {
    if !v0.is_finite() || !v1.is_finite() || !level.is_finite() {
        return None;
    }
    if t1 < t0 {
        return None;
    }

    let dt = (t1.saturating_sub(t0)) as f64;
    let denom = v1 - v0;

    let alpha = if denom.abs() < 1e-12 {
        // Flat step or tiny division: choose nearest bound
        if (level - v0).abs() <= (level - v1).abs() {
            0.0
        } else {
            1.0
        }
    } else {
        let raw = (level - v0) / denom;
        if raw.is_nan() {
            0.0
        } else {
            raw.clamp(0.0, 1.0)
        }
    };

    let subpixel_offset_ns = (alpha * dt) as f32;
    Some((alpha, subpixel_offset_ns))
}

/// Internal Schmitt-trigger hysteresis debouncing state.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum HysteresisBandState {
    /// Initial or unprimed state (waiting to visit outside the hysteresis band).
    #[default]
    Unprimed,
    /// Signal visited $v \le \text{level} - h$ (armed for upcoming rising edge).
    ArmedLow,
    /// Signal visited $v \ge \text{level} + h$ (armed for upcoming falling edge).
    ArmedHigh,
}

/// Real-time edge and discrete state evaluation engine with Schmitt hysteresis debouncing
/// and sub-sample linear interpolation.
#[derive(Clone, Debug, PartialEq)]
pub struct TriggerEngine {
    condition: TriggerCondition,
    state: HysteresisBandState,
    last_scalar_sample: Option<(TimestampNs, f64)>,
    last_state_code: Option<i32>,
}

impl TriggerEngine {
    /// Creates a new `TriggerEngine` with the specified condition.
    pub fn new(condition: TriggerCondition) -> Self {
        Self {
            condition,
            state: HysteresisBandState::Unprimed,
            last_scalar_sample: None,
            last_state_code: None,
        }
    }

    /// Returns a reference to the active trigger condition.
    #[inline]
    pub fn condition(&self) -> &TriggerCondition {
        &self.condition
    }

    /// Updates the trigger condition and resets internal debouncing state.
    pub fn set_condition(&mut self, condition: TriggerCondition) {
        self.condition = condition;
        self.reset();
    }

    /// Returns the target channel monitored by this trigger engine.
    #[inline]
    pub fn target_channel(&self) -> ChannelId {
        match self.condition {
            TriggerCondition::Edge { channel, .. } => channel,
            TriggerCondition::StateEquals { channel, .. } => channel,
        }
    }

    /// Returns the current debouncing arm state.
    #[inline]
    pub fn state(&self) -> HysteresisBandState {
        self.state
    }

    /// Forwarding alias for [`state`](Self::state) matching the interface contract.
    #[inline]
    pub fn band_state(&self) -> HysteresisBandState {
        self.state()
    }

    /// Returns `true` once at least one scalar sample has been observed since creation/reset.
    #[inline]
    pub fn has_last_scalar_sample(&self) -> bool {
        self.last_scalar_sample.is_some()
    }

    /// Returns the most recently evaluated scalar sample `(timestamp_ns, value)`, if any.
    #[inline]
    pub fn last_scalar_sample(&self) -> Option<(TimestampNs, f64)> {
        self.last_scalar_sample
    }

    /// Resets all internal sample tracking and debouncing states to unprimed.
    pub fn reset(&mut self) {
        self.state = HysteresisBandState::Unprimed;
        self.last_scalar_sample = None;
        self.last_state_code = None;
    }

    /// Evaluates a single incoming calibrated scalar sample `(t_ns, val)`.
    ///
    /// Implements Schmitt-trigger debouncing with dual thresholds:
    /// - $V_{\text{low}} = \text{level} - h$
    /// - $V_{\text{high}} = \text{level} + h$
    ///
    /// Returns `Some(TriggerHit)` upon a qualified threshold crossing.
    pub fn evaluate_scalar(&mut self, t_ns: TimestampNs, val: f64) -> Option<TriggerHit> {
        let (level, hysteresis, slope) = match self.condition {
            TriggerCondition::Edge {
                level,
                hysteresis,
                slope,
                ..
            } => (level, hysteresis.abs(), slope),
            TriggerCondition::StateEquals { .. } => return None,
        };

        if !val.is_finite() {
            return None;
        }

        let v_low = level - hysteresis;
        let v_high = level + hysteresis;

        let prev_sample = self.last_scalar_sample;
        self.last_scalar_sample = Some((t_ns, val));

        let (t_prev, v_prev) = match prev_sample {
            Some(prev) => prev,
            None => {
                // First sample: prime hysteresis arm state if outside band
                match slope {
                    EdgeSlope::Rising => {
                        if val <= v_low {
                            self.state = HysteresisBandState::ArmedLow;
                        }
                    }
                    EdgeSlope::Falling => {
                        if val >= v_high {
                            self.state = HysteresisBandState::ArmedHigh;
                        }
                    }
                    EdgeSlope::Either => {
                        if val <= v_low {
                            self.state = HysteresisBandState::ArmedLow;
                        } else if val >= v_high {
                            self.state = HysteresisBandState::ArmedHigh;
                        }
                    }
                }
                return None;
            }
        };

        // Evaluate crossing condition
        let fired_hit = match slope {
            EdgeSlope::Rising => {
                if self.state == HysteresisBandState::ArmedLow
                    && v_prev <= level
                    && val >= level
                    && val > v_prev
                {
                    self.state = HysteresisBandState::Unprimed;
                    let (_alpha, subpixel_offset_ns) =
                        calculate_subsample_offset(t_prev, v_prev, t_ns, val, level)
                            .unwrap_or((0.0, 0.0));
                    Some(TriggerHit::new(t_prev, subpixel_offset_ns))
                } else {
                    None
                }
            }
            EdgeSlope::Falling => {
                if self.state == HysteresisBandState::ArmedHigh
                    && v_prev >= level
                    && val <= level
                    && val < v_prev
                {
                    self.state = HysteresisBandState::Unprimed;
                    let (_alpha, subpixel_offset_ns) =
                        calculate_subsample_offset(t_prev, v_prev, t_ns, val, level)
                            .unwrap_or((0.0, 0.0));
                    Some(TriggerHit::new(t_prev, subpixel_offset_ns))
                } else {
                    None
                }
            }
            EdgeSlope::Either => {
                if self.state == HysteresisBandState::ArmedLow
                    && v_prev <= level
                    && val >= level
                    && val > v_prev
                {
                    self.state = if val >= v_high {
                        HysteresisBandState::ArmedHigh
                    } else {
                        HysteresisBandState::Unprimed
                    };
                    let (_alpha, subpixel_offset_ns) =
                        calculate_subsample_offset(t_prev, v_prev, t_ns, val, level)
                            .unwrap_or((0.0, 0.0));
                    Some(TriggerHit::new(t_prev, subpixel_offset_ns))
                } else if self.state == HysteresisBandState::ArmedHigh
                    && v_prev >= level
                    && val <= level
                    && val < v_prev
                {
                    self.state = if val <= v_low {
                        HysteresisBandState::ArmedLow
                    } else {
                        HysteresisBandState::Unprimed
                    };
                    let (_alpha, subpixel_offset_ns) =
                        calculate_subsample_offset(t_prev, v_prev, t_ns, val, level)
                            .unwrap_or((0.0, 0.0));
                    Some(TriggerHit::new(t_prev, subpixel_offset_ns))
                } else {
                    None
                }
            }
        };

        if let Some(hit) = fired_hit {
            return Some(hit);
        }

        // If not fired, update arming states based on current sample
        match slope {
            EdgeSlope::Rising => {
                if val <= v_low {
                    self.state = HysteresisBandState::ArmedLow;
                }
            }
            EdgeSlope::Falling => {
                if val >= v_high {
                    self.state = HysteresisBandState::ArmedHigh;
                }
            }
            EdgeSlope::Either => {
                if val <= v_low {
                    self.state = HysteresisBandState::ArmedLow;
                } else if val >= v_high {
                    self.state = HysteresisBandState::ArmedHigh;
                }
            }
        }

        None
    }

    /// Forwarding alias for [`evaluate_scalar`](Self::evaluate_scalar) matching PROJECT.md interface contract.
    #[inline]
    pub fn evaluate(&mut self, t_ns: TimestampNs, val: f64) -> Option<TriggerHit> {
        self.evaluate_scalar(t_ns, val)
    }

    /// Evaluates a discrete state code transition `(t_ns, code)`.
    ///
    /// Triggers when the state code enters `state_code` (discrete jump; `subpixel_offset_ns = 0.0`).
    pub fn evaluate_state(&mut self, t_ns: TimestampNs, code: i32) -> Option<TriggerHit> {
        let target_code = match self.condition {
            TriggerCondition::StateEquals { state_code, .. } => state_code,
            TriggerCondition::Edge { .. } => return None,
        };

        let prev = self.last_state_code;
        self.last_state_code = Some(code);

        match prev {
            None => {
                if code == target_code {
                    Some(TriggerHit::new(t_ns, 0.0))
                } else {
                    None
                }
            }
            Some(prev_code) => {
                if prev_code != target_code && code == target_code {
                    Some(TriggerHit::new(t_ns, 0.0))
                } else {
                    None
                }
            }
        }
    }

    /// Evaluates a contiguous batch of calibrated scalar samples, returning the first trigger hit if any.
    pub fn evaluate_batch_scalar(
        &mut self,
        timestamps: &[TimestampNs],
        values: &[f64],
    ) -> Option<TriggerHit> {
        let n = timestamps.len().min(values.len());
        for i in 0..n {
            if let Some(hit) = self.evaluate_scalar(timestamps[i], values[i]) {
                return Some(hit);
            }
        }
        None
    }
}

/// Configuration parameters for an independent [`ScopeController`] instance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScopeControllerConfig {
    /// Unique identifier of this oscilloscope viewport.
    pub scope_id: ScopeId,
    /// Optional display title.
    pub title: Option<String>,
    /// Horizontal render canvas width in pixels (default 2000).
    pub canvas_width_px: u32,
    /// Total horizontal time span in nanoseconds (default 10 ms = 10,000,000 ns).
    pub time_span_ns: u64,
    /// Trigger T0 horizontal alignment ratio in $[0.0, 1.0]$ (default 0.5 = center).
    pub pre_trigger_ratio: f64,
    /// Horizontal pan offset in nanoseconds (default 0).
    pub pan_offset_ns: i64,
    /// Active sweep mode: Roll, Auto, Normal, Single (default Auto).
    pub sweep_mode: TriggerSweepMode,
    /// Auto-trigger timeout in nanoseconds (default 100 ms = 100,000,000 ns).
    pub auto_timeout_ns: u64,
    /// Optional active trigger condition.
    pub trigger: Option<TriggerCondition>,
    /// Optional synchronization group membership.
    pub sync_group_id: Option<SyncGroupId>,
    /// Acquisition paused flag.
    pub paused: bool,
}

impl Default for ScopeControllerConfig {
    fn default() -> Self {
        Self {
            scope_id: ScopeId(1),
            title: None,
            canvas_width_px: 2000,
            time_span_ns: 10_000_000, // 10 ms
            pre_trigger_ratio: 0.5,    // Centered trigger
            pan_offset_ns: 0,
            sweep_mode: TriggerSweepMode::Auto,
            auto_timeout_ns: 100_000_000, // 100 ms
            trigger: None,
            sync_group_id: None,
            paused: false,
        }
    }
}

/// Computed horizontal time window for viewport rendering, matching `DDSF v1` header fields.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScopeViewportWindow {
    /// Scope identifier.
    pub scope_id: ScopeId,
    /// Runtime state matching `DDSF v1` `trigger_state`.
    pub trigger_state: TriggerRuntimeState,
    /// Sweep mode matching `DDSF v1` `mode`.
    pub mode: TriggerSweepMode,
    /// Viewport window start timestamp in nanoseconds.
    pub win_t_start_ns: u64,
    /// Viewport window span in nanoseconds.
    pub win_t_span_ns: u64,
    /// Sub-sample linear interpolation offset in nanoseconds (`0.0` if none).
    pub subpixel_offset_ns: f32,
    /// Discrete trigger timestamp if triggered, or None for roll/freerun.
    pub trigger_timestamp_ns: Option<u64>,
}

/// Outcome emitted by [`ScopeController`] upon processing samples or periodic timer steps.
#[derive(Clone, Debug, PartialEq)]
pub enum ScopeStepOutcome {
    /// No change in viewport window or state.
    NoChange,
    /// Continuous roll window updated.
    Rolling { window: ScopeViewportWindow },
    /// Waiting for qualified trigger condition.
    WaitingForTrigger,
    /// Qualified trigger hit detected; collecting post-trigger acquisition window.
    TriggerDetected { hit: TriggerHit },
    /// Auto-trigger timeout (100ms) fired; emitted free-running viewport window.
    AutoFreerunFired { window: ScopeViewportWindow },
    /// Acquisition completed; emitted complete frame. If `latched_pinned == true`, memory is locked.
    CaptureCompleted {
        window: ScopeViewportWindow,
        latched_pinned: bool,
    },
}

/// Independent oscilloscope viewport controller managing sweep mode state machine,
/// auto-trigger timeouts, and memory pinning.
#[derive(Clone, Debug)]
pub struct ScopeController {
    config: ScopeControllerConfig,
    state: TriggerRuntimeState,
    trigger_engine: Option<TriggerEngine>,
    armed_time_ns: u64,
    latest_sample_time_ns: u64,
    active_hit: Option<TriggerHit>,
    target_win_end_ns: u64,
    current_window: Option<ScopeViewportWindow>,
    is_pinned: bool,
}

impl ScopeController {
    /// Constructs a new [`ScopeController`] configured with `config`.
    pub fn new(mut config: ScopeControllerConfig) -> Self {
        config.time_span_ns = config.time_span_ns.max(1000);
        if !config.pre_trigger_ratio.is_finite() {
            config.pre_trigger_ratio = 0.5;
        } else {
            config.pre_trigger_ratio = config.pre_trigger_ratio.clamp(0.0, 1.0);
        }

        let state = match config.sweep_mode {
            TriggerSweepMode::Roll => TriggerRuntimeState::Roll,
            TriggerSweepMode::Auto
            | TriggerSweepMode::Normal
            | TriggerSweepMode::Single => TriggerRuntimeState::Armed,
        };
        let trigger_engine = config.trigger.map(TriggerEngine::new);

        Self {
            config,
            state,
            trigger_engine,
            armed_time_ns: 0,
            latest_sample_time_ns: 0,
            active_hit: None,
            target_win_end_ns: 0,
            current_window: None,
            is_pinned: false,
        }
    }

    /// Returns a reference to the controller's configuration.
    #[inline]
    pub fn config(&self) -> &ScopeControllerConfig {
        &self.config
    }

    /// Returns the current runtime execution state.
    #[inline]
    pub fn current_state(&self) -> TriggerRuntimeState {
        self.state
    }

    /// Forwarding alias for [`current_state`](Self::current_state) matching PROJECT.md interface contract.
    #[inline]
    pub fn state(&self) -> TriggerRuntimeState {
        self.current_state()
    }

    /// Returns the configured sweep mode.
    #[inline]
    pub fn sweep_mode(&self) -> TriggerSweepMode {
        self.config.sweep_mode
    }

    /// Returns `true` if this controller is currently in `LatchedPinned` state holding memory pinned.
    #[inline]
    pub fn is_pinned(&self) -> bool {
        self.is_pinned
    }

    /// Returns the most recent computed viewport window.
    #[inline]
    pub fn current_window(&self) -> Option<ScopeViewportWindow> {
        self.current_window
    }

    /// Returns the active trigger hit if currently triggered or latched.
    #[inline]
    pub fn active_hit(&self) -> Option<TriggerHit> {
        self.active_hit
    }

    /// Returns a reference to the active trigger engine, if configured.
    #[inline]
    pub fn trigger_engine(&self) -> Option<&TriggerEngine> {
        self.trigger_engine.as_ref()
    }

    /// Returns a mutable reference to the active trigger engine, if configured.
    #[inline]
    pub fn trigger_engine_mut(&mut self) -> Option<&mut TriggerEngine> {
        self.trigger_engine.as_mut()
    }

    /// Returns the timestamp when the controller last armed.
    #[inline]
    pub fn armed_time_ns(&self) -> u64 {
        self.armed_time_ns
    }

    /// Returns the post-trigger target window end timestamp while in `Triggered` state.
    #[inline]
    pub fn target_win_end_ns(&self) -> u64 {
        self.target_win_end_ns
    }

    /// Returns the latest sample timestamp observed by this controller.
    #[inline]
    pub fn latest_sample_time_ns(&self) -> u64 {
        self.latest_sample_time_ns
    }

    /// Sets or clears the acquisition paused flag.
    pub fn set_paused(&mut self, paused: bool) {
        self.config.paused = paused;
    }

    /// Returns `true` if acquisition is paused.
    #[inline]
    pub fn is_paused(&self) -> bool {
        self.config.paused
    }

    /// Recomputes `current_window` (if present) after a zoom (`time_span_ns`),
    /// pan (`pan_offset_ns`), or `pre_trigger_ratio` update.
    ///
    /// Essential for inspecting/zooming into `LatchedPinned` (`Single`) or paused captures.
    pub fn refresh_window_for_view_change(&mut self) {
        let Some(prev) = self.current_window else {
            return;
        };
        let span = self.config.time_span_ns;
        let updated = if let Some(trig_ts) = prev.trigger_timestamp_ns {
            let pre_span = (self.pre_trigger_ratio() * span as f64).round() as u64;
            let base = trig_ts.saturating_sub(pre_span);
            let win_start =
                ((base as i128) + (self.config.pan_offset_ns as i128)).clamp(0, u64::MAX as i128)
                    as u64;
            ScopeViewportWindow {
                mode: self.config.sweep_mode,
                win_t_start_ns: win_start,
                win_t_span_ns: span,
                ..prev
            }
        } else if self.latest_sample_time_ns > 0 {
            let base = self.latest_sample_time_ns.saturating_sub(span);
            let win_start =
                ((base as i128) + (self.config.pan_offset_ns as i128)).clamp(0, u64::MAX as i128)
                    as u64;
            ScopeViewportWindow {
                mode: self.config.sweep_mode,
                win_t_start_ns: win_start,
                win_t_span_ns: span,
                ..prev
            }
        } else {
            ScopeViewportWindow {
                mode: self.config.sweep_mode,
                win_t_span_ns: span,
                ..prev
            }
        };
        self.current_window = Some(updated);
    }

    /// Updates the horizontal time span in nanoseconds.
    pub fn set_time_span_ns(&mut self, span_ns: u64) {
        self.config.time_span_ns = span_ns.max(1000);
        self.refresh_window_for_view_change();
    }

    /// Updates the horizontal pan offset in nanoseconds.
    pub fn set_pan_offset_ns(&mut self, offset_ns: i64) {
        self.config.pan_offset_ns = offset_ns;
        self.refresh_window_for_view_change();
    }

    /// Updates the full controller configuration, dynamically updating sweep modes and triggers.
    pub fn update_config(&mut self, mut config: ScopeControllerConfig) {
        config.time_span_ns = config.time_span_ns.max(1000);
        if !config.pre_trigger_ratio.is_finite() {
            config.pre_trigger_ratio = 0.5;
        } else {
            config.pre_trigger_ratio = config.pre_trigger_ratio.clamp(0.0, 1.0);
        }

        let old_mode = self.config.sweep_mode;
        let old_trigger = self.config.trigger;

        self.config = config;

        if self.config.trigger != old_trigger {
            self.trigger_engine = self.config.trigger.map(TriggerEngine::new);
        }

        if self.config.sweep_mode != old_mode {
            self.set_sweep_mode(self.config.sweep_mode);
        } else {
            self.refresh_window_for_view_change();
        }
    }

    /// Dynamically switches sweep mode between `Roll`, `Auto`, `Normal`, and `Single`.
    pub fn set_sweep_mode(&mut self, mode: TriggerSweepMode) {
        self.config.sweep_mode = mode;
        self.is_pinned = false;
        self.active_hit = None;
        self.target_win_end_ns = 0;
        if let Some(ref mut engine) = self.trigger_engine {
            engine.reset();
        }
        self.armed_time_ns = self.latest_sample_time_ns;

        match mode {
            TriggerSweepMode::Roll => {
                self.state = TriggerRuntimeState::Roll;
            }
            TriggerSweepMode::Auto
            | TriggerSweepMode::Normal
            | TriggerSweepMode::Single => {
                self.state = TriggerRuntimeState::Armed;
            }
        }
    }

    /// Forwarding alias for [`set_sweep_mode`](Self::set_sweep_mode) matching PROJECT.md interface contract.
    #[inline]
    pub fn set_mode(&mut self, mode: TriggerSweepMode) {
        self.set_sweep_mode(mode);
    }

    #[inline]
    fn pre_trigger_ratio(&self) -> f64 {
        if self.config.pre_trigger_ratio.is_finite() {
            self.config.pre_trigger_ratio.clamp(0.0, 1.0)
        } else {
            0.5
        }
    }

    /// Rearms acquisition, releasing any pinned memory chunk lock and resetting the trigger engine.
    pub fn rearm(&mut self) {
        self.is_pinned = false;
        self.active_hit = None;
        self.target_win_end_ns = 0;
        if let Some(ref mut engine) = self.trigger_engine {
            engine.reset();
        }
        self.armed_time_ns = self.latest_sample_time_ns;

        match self.config.sweep_mode {
            TriggerSweepMode::Roll => {
                self.state = TriggerRuntimeState::Roll;
            }
            TriggerSweepMode::Auto
            | TriggerSweepMode::Normal
            | TriggerSweepMode::Single => {
                self.state = TriggerRuntimeState::Armed;
            }
        }
    }

    /// Rearms acquisition anchored explicitly at `armed_at_ns`, preventing stale ring-buffer
    /// samples older than `armed_at_ns` from re-triggering or prematurely firing auto-timeout.
    pub fn rearm_at(&mut self, armed_at_ns: TimestampNs) {
        self.latest_sample_time_ns = self.latest_sample_time_ns.max(armed_at_ns);
        self.rearm();
        self.armed_time_ns = armed_at_ns;
    }

    /// Manually forces an immediate trigger event at `now_ns`.
    pub fn force_trigger(&mut self, now_ns: TimestampNs) -> ScopeStepOutcome {
        if self.config.paused || self.state == TriggerRuntimeState::LatchedPinned {
            return ScopeStepOutcome::NoChange;
        }

        self.latest_sample_time_ns = self.latest_sample_time_ns.max(now_ns);
        let hit = TriggerHit::new(now_ns, 0.0);

        if self.config.sweep_mode == TriggerSweepMode::Roll {
            let window = self.compute_rolling_window(self.latest_sample_time_ns);
            self.current_window = Some(window);
            return ScopeStepOutcome::Rolling { window };
        }

        let pre_ratio = self.pre_trigger_ratio();
        let pre_span = (pre_ratio * self.config.time_span_ns as f64).round() as u64;
        let post_span = self.config.time_span_ns.saturating_sub(pre_span);
        let target_end = hit.timestamp_ns.saturating_add(post_span);

        if self.latest_sample_time_ns >= target_end {
            let window = self.compute_triggered_window(&hit);
            self.current_window = Some(window);

            if self.config.sweep_mode == TriggerSweepMode::Single {
                self.state = TriggerRuntimeState::LatchedPinned;
                self.is_pinned = true;
                self.active_hit = None;
                ScopeStepOutcome::CaptureCompleted {
                    window,
                    latched_pinned: true,
                }
            } else {
                self.state = TriggerRuntimeState::Armed;
                self.armed_time_ns = self.latest_sample_time_ns;
                self.active_hit = None;
                ScopeStepOutcome::CaptureCompleted {
                    window,
                    latched_pinned: false,
                }
            }
        } else {
            self.state = TriggerRuntimeState::Triggered;
            self.active_hit = Some(hit);
            self.target_win_end_ns = target_end;
            ScopeStepOutcome::TriggerDetected { hit }
        }
    }

    /// Ingests a new calibrated scalar sample for a channel.
    pub fn process_scalar_sample(
        &mut self,
        channel: ChannelId,
        t_ns: TimestampNs,
        val: f64,
    ) -> ScopeStepOutcome {
        if self.config.paused || self.state == TriggerRuntimeState::LatchedPinned {
            return ScopeStepOutcome::NoChange;
        }

        if self.armed_time_ns == 0 {
            self.armed_time_ns = t_ns;
        }
        self.latest_sample_time_ns = self.latest_sample_time_ns.max(t_ns);

        if self.config.sweep_mode == TriggerSweepMode::Roll {
            let window = self.compute_rolling_window(self.latest_sample_time_ns);
            self.current_window = Some(window);
            return ScopeStepOutcome::Rolling { window };
        }

        if self.state == TriggerRuntimeState::Triggered {
            if self.latest_sample_time_ns >= self.target_win_end_ns {
                let hit = self.active_hit.take().unwrap_or(TriggerHit::new(t_ns, 0.0));
                let window = self.compute_triggered_window(&hit);
                self.current_window = Some(window);

                if self.config.sweep_mode == TriggerSweepMode::Single {
                    self.state = TriggerRuntimeState::LatchedPinned;
                    self.is_pinned = true;
                    return ScopeStepOutcome::CaptureCompleted {
                        window,
                        latched_pinned: true,
                    };
                } else {
                    self.state = TriggerRuntimeState::Armed;
                    self.armed_time_ns = self.latest_sample_time_ns;
                    return ScopeStepOutcome::CaptureCompleted {
                        window,
                        latched_pinned: false,
                    };
                }
            }
            return ScopeStepOutcome::NoChange;
        }

        // We are in Armed or AutoFreerun
        let mut hit_opt = None;
        if let Some(ref mut engine) = self.trigger_engine {
            if engine.target_channel() == channel {
                hit_opt = engine.evaluate_scalar(t_ns, val);
            }
        }

        if let Some(hit) = hit_opt {
            let pre_ratio = self.pre_trigger_ratio();
            let pre_span = (pre_ratio * self.config.time_span_ns as f64).round() as u64;
            let post_span = self.config.time_span_ns.saturating_sub(pre_span);
            let target_end = hit.timestamp_ns.saturating_add(post_span);

            if self.latest_sample_time_ns >= target_end {
                let window = self.compute_triggered_window(&hit);
                self.current_window = Some(window);

                if self.config.sweep_mode == TriggerSweepMode::Single {
                    self.state = TriggerRuntimeState::LatchedPinned;
                    self.is_pinned = true;
                    return ScopeStepOutcome::CaptureCompleted {
                        window,
                        latched_pinned: true,
                    };
                } else {
                    self.state = TriggerRuntimeState::Armed;
                    self.armed_time_ns = self.latest_sample_time_ns;
                    return ScopeStepOutcome::CaptureCompleted {
                        window,
                        latched_pinned: false,
                    };
                }
            } else {
                self.state = TriggerRuntimeState::Triggered;
                self.active_hit = Some(hit);
                self.target_win_end_ns = target_end;
                return ScopeStepOutcome::TriggerDetected { hit };
            }
        }

        // Check auto-timeout in Auto mode
        if self.config.sweep_mode == TriggerSweepMode::Auto {
            let elapsed = self.latest_sample_time_ns.saturating_sub(self.armed_time_ns);
            if elapsed >= self.config.auto_timeout_ns {
                let window = self.compute_freerun_window(self.latest_sample_time_ns);
                self.current_window = Some(window);
                self.state = TriggerRuntimeState::Armed;
                self.armed_time_ns = self.latest_sample_time_ns;
                return ScopeStepOutcome::AutoFreerunFired { window };
            }
        }

        ScopeStepOutcome::WaitingForTrigger
    }

    /// Ingests a discrete state transition sample for a channel.
    pub fn process_state_sample(
        &mut self,
        channel: ChannelId,
        t_ns: TimestampNs,
        code: i32,
    ) -> ScopeStepOutcome {
        if self.config.paused || self.state == TriggerRuntimeState::LatchedPinned {
            return ScopeStepOutcome::NoChange;
        }

        if self.armed_time_ns == 0 {
            self.armed_time_ns = t_ns;
        }
        self.latest_sample_time_ns = self.latest_sample_time_ns.max(t_ns);

        if self.config.sweep_mode == TriggerSweepMode::Roll {
            let window = self.compute_rolling_window(self.latest_sample_time_ns);
            self.current_window = Some(window);
            return ScopeStepOutcome::Rolling { window };
        }

        if self.state == TriggerRuntimeState::Triggered {
            if self.latest_sample_time_ns >= self.target_win_end_ns {
                let hit = self.active_hit.take().unwrap_or(TriggerHit::new(t_ns, 0.0));
                let window = self.compute_triggered_window(&hit);
                self.current_window = Some(window);

                if self.config.sweep_mode == TriggerSweepMode::Single {
                    self.state = TriggerRuntimeState::LatchedPinned;
                    self.is_pinned = true;
                    return ScopeStepOutcome::CaptureCompleted {
                        window,
                        latched_pinned: true,
                    };
                } else {
                    self.state = TriggerRuntimeState::Armed;
                    self.armed_time_ns = self.latest_sample_time_ns;
                    return ScopeStepOutcome::CaptureCompleted {
                        window,
                        latched_pinned: false,
                    };
                }
            }
            return ScopeStepOutcome::NoChange;
        }

        let mut hit_opt = None;
        if let Some(ref mut engine) = self.trigger_engine {
            if engine.target_channel() == channel {
                hit_opt = engine.evaluate_state(t_ns, code);
            }
        }

        if let Some(hit) = hit_opt {
            let pre_ratio = self.pre_trigger_ratio();
            let pre_span = (pre_ratio * self.config.time_span_ns as f64).round() as u64;
            let post_span = self.config.time_span_ns.saturating_sub(pre_span);
            let target_end = hit.timestamp_ns.saturating_add(post_span);

            if self.latest_sample_time_ns >= target_end {
                let window = self.compute_triggered_window(&hit);
                self.current_window = Some(window);

                if self.config.sweep_mode == TriggerSweepMode::Single {
                    self.state = TriggerRuntimeState::LatchedPinned;
                    self.is_pinned = true;
                    return ScopeStepOutcome::CaptureCompleted {
                        window,
                        latched_pinned: true,
                    };
                } else {
                    self.state = TriggerRuntimeState::Armed;
                    self.armed_time_ns = self.latest_sample_time_ns;
                    return ScopeStepOutcome::CaptureCompleted {
                        window,
                        latched_pinned: false,
                    };
                }
            } else {
                self.state = TriggerRuntimeState::Triggered;
                self.active_hit = Some(hit);
                self.target_win_end_ns = target_end;
                return ScopeStepOutcome::TriggerDetected { hit };
            }
        }

        if self.config.sweep_mode == TriggerSweepMode::Auto {
            let elapsed = self.latest_sample_time_ns.saturating_sub(self.armed_time_ns);
            if elapsed >= self.config.auto_timeout_ns {
                let window = self.compute_freerun_window(self.latest_sample_time_ns);
                self.current_window = Some(window);
                self.state = TriggerRuntimeState::Armed;
                self.armed_time_ns = self.latest_sample_time_ns;
                return ScopeStepOutcome::AutoFreerunFired { window };
            }
        }

        ScopeStepOutcome::WaitingForTrigger
    }

    /// Periodic clock step (e.g. at 60/120 Hz render ticks) to evaluate auto-timeout and roll updates.
    pub fn step(&mut self, now_ns: TimestampNs) -> ScopeStepOutcome {
        if self.config.paused || self.state == TriggerRuntimeState::LatchedPinned {
            return ScopeStepOutcome::NoChange;
        }

        if self.armed_time_ns == 0 {
            self.armed_time_ns = now_ns;
        }

        match self.state {
            TriggerRuntimeState::Roll => {
                self.latest_sample_time_ns = self.latest_sample_time_ns.max(now_ns);
                let window = self.compute_rolling_window(self.latest_sample_time_ns);
                self.current_window = Some(window);
                ScopeStepOutcome::Rolling { window }
            }
            TriggerRuntimeState::Triggered => {
                if now_ns >= self.target_win_end_ns && self.active_hit.is_some() {
                    self.latest_sample_time_ns = self.latest_sample_time_ns.max(now_ns);
                    let hit = self.active_hit.take().unwrap();
                    let window = self.compute_triggered_window(&hit);
                    self.current_window = Some(window);

                    if self.config.sweep_mode == TriggerSweepMode::Single {
                        self.state = TriggerRuntimeState::LatchedPinned;
                        self.is_pinned = true;
                        ScopeStepOutcome::CaptureCompleted {
                            window,
                            latched_pinned: true,
                        }
                    } else {
                        self.state = TriggerRuntimeState::Armed;
                        self.armed_time_ns = self.latest_sample_time_ns;
                        ScopeStepOutcome::CaptureCompleted {
                            window,
                            latched_pinned: false,
                        }
                    }
                } else {
                    ScopeStepOutcome::NoChange
                }
            }
            TriggerRuntimeState::Armed | TriggerRuntimeState::AutoFreerun => {
                if self.config.sweep_mode == TriggerSweepMode::Auto {
                    let elapsed = now_ns.saturating_sub(self.armed_time_ns);
                    if elapsed >= self.config.auto_timeout_ns {
                        self.latest_sample_time_ns = self.latest_sample_time_ns.max(now_ns);
                        let window = self.compute_freerun_window(self.latest_sample_time_ns);
                        self.current_window = Some(window);
                        self.state = TriggerRuntimeState::Armed;
                        self.armed_time_ns = now_ns;
                        ScopeStepOutcome::AutoFreerunFired { window }
                    } else {
                        ScopeStepOutcome::WaitingForTrigger
                    }
                } else {
                    ScopeStepOutcome::WaitingForTrigger
                }
            }
            TriggerRuntimeState::LatchedPinned => ScopeStepOutcome::NoChange,
        }
    }

    fn compute_triggered_window(&self, hit: &TriggerHit) -> ScopeViewportWindow {
        let span = self.config.time_span_ns;
        let pre_ratio = self.pre_trigger_ratio();
        let pre_span = (pre_ratio * span as f64).round() as u64;
        let base = hit.timestamp_ns.saturating_sub(pre_span);
        let win_start =
            ((base as i128) + (self.config.pan_offset_ns as i128)).clamp(0, u64::MAX as i128)
                as u64;

        ScopeViewportWindow {
            scope_id: self.config.scope_id,
            trigger_state: if self.config.sweep_mode == TriggerSweepMode::Single {
                TriggerRuntimeState::LatchedPinned
            } else {
                TriggerRuntimeState::Triggered
            },
            mode: self.config.sweep_mode,
            win_t_start_ns: win_start,
            win_t_span_ns: span,
            subpixel_offset_ns: hit.subpixel_offset_ns,
            trigger_timestamp_ns: Some(hit.timestamp_ns),
        }
    }

    fn compute_rolling_window(&self, latest_ns: u64) -> ScopeViewportWindow {
        let span = self.config.time_span_ns;
        let base = latest_ns.saturating_sub(span);
        let win_start =
            ((base as i128) + (self.config.pan_offset_ns as i128)).clamp(0, u64::MAX as i128)
                as u64;

        ScopeViewportWindow {
            scope_id: self.config.scope_id,
            trigger_state: TriggerRuntimeState::Roll,
            mode: TriggerSweepMode::Roll,
            win_t_start_ns: win_start,
            win_t_span_ns: span,
            subpixel_offset_ns: 0.0,
            trigger_timestamp_ns: None,
        }
    }

    fn compute_freerun_window(&self, latest_ns: u64) -> ScopeViewportWindow {
        let span = self.config.time_span_ns;
        let base = latest_ns.saturating_sub(span);
        let win_start =
            ((base as i128) + (self.config.pan_offset_ns as i128)).clamp(0, u64::MAX as i128)
                as u64;

        ScopeViewportWindow {
            scope_id: self.config.scope_id,
            trigger_state: TriggerRuntimeState::AutoFreerun,
            mode: TriggerSweepMode::Auto,
            win_t_start_ns: win_start,
            win_t_span_ns: span,
            subpixel_offset_ns: 0.0,
            trigger_timestamp_ns: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_subsample_linear_interpolation_midpoint() {
        // v0 = -1.0, v1 = 3.0, level = 0.0 -> alpha = (0 - (-1)) / (3 - (-1)) = 1 / 4 = 0.25
        let (alpha, offset) =
            calculate_subsample_offset(1000, -1.0, 1100, 3.0, 0.0).expect("valid interpolation");
        assert!((alpha - 0.25).abs() < 1e-9);
        assert!((offset - 25.0).abs() < 1e-5);

        // Exact midpoint: v0 = 0.0, v1 = 10.0, level = 5.0 -> alpha = 0.5
        let (alpha2, offset2) =
            calculate_subsample_offset(0, 0.0, 100, 10.0, 5.0).expect("valid interpolation");
        assert!((alpha2 - 0.5).abs() < 1e-9);
        assert!((offset2 - 50.0).abs() < 1e-5);
    }

    #[test]
    fn test_subsample_linear_interpolation_boundaries() {
        // Lower boundary: level == v0
        let (alpha_low, offset_low) =
            calculate_subsample_offset(100, 2.0, 200, 5.0, 2.0).expect("boundary interpolation");
        assert_eq!(alpha_low, 0.0);
        assert_eq!(offset_low, 0.0);

        // Upper boundary: level == v1
        let (alpha_high, offset_high) =
            calculate_subsample_offset(100, 2.0, 200, 5.0, 5.0).expect("boundary interpolation");
        assert_eq!(alpha_high, 1.0);
        assert_eq!(offset_high, 100.0);
    }

    #[test]
    fn test_subsample_linear_interpolation_edge_cases() {
        // Flat step (v1 == v0)
        let (alpha_flat, offset_flat) =
            calculate_subsample_offset(100, 5.0, 200, 5.0, 5.0).expect("flat interpolation");
        assert_eq!(alpha_flat, 0.0);
        assert_eq!(offset_flat, 0.0);

        // Tiny denominator (< 1e-12)
        let (alpha_tiny, _) =
            calculate_subsample_offset(100, 5.0, 200, 5.0 + 1e-14, 5.0).expect("tiny denominator");
        assert_eq!(alpha_tiny, 0.0);

        // NaN rejection
        assert!(calculate_subsample_offset(100, f64::NAN, 200, 5.0, 2.0).is_none());
        assert!(calculate_subsample_offset(100, 1.0, 200, f64::INFINITY, 2.0).is_none());
        assert!(calculate_subsample_offset(100, 1.0, 200, 5.0, f64::NAN).is_none());

        // Non-monotonic timestamps
        assert!(calculate_subsample_offset(200, 1.0, 100, 5.0, 2.0).is_none());

        // Zero duration (t1 == t0)
        let (alpha_zero_dt, offset_zero_dt) =
            calculate_subsample_offset(100, 1.0, 100, 5.0, 3.0).expect("zero dt");
        assert_eq!(alpha_zero_dt, 0.5);
        assert_eq!(offset_zero_dt, 0.0);
    }

    #[test]
    fn test_trigger_hit_methods() {
        let hit = TriggerHit::new(10_000, 25.4);
        assert!((hit.exact_timestamp_ns() - 10_025.4).abs() < 1e-4);
        assert_eq!(hit.interpolated_ts_u64(), 10_025);

        let hit2 = TriggerHit::new(10_000, 25.6);
        assert_eq!(hit2.interpolated_ts_u64(), 10_026);
    }

    #[test]
    fn test_hysteresis_debouncing_rising() {
        // Level = 2.0, Hysteresis = 0.5 -> V_low = 1.5, V_high = 2.5
        let cond = TriggerCondition::Edge {
            channel: ChannelId(1),
            level: 2.0,
            hysteresis: 0.5,
            slope: EdgeSlope::Rising,
        };
        let mut engine = TriggerEngine::new(cond);

        // Sample inside band (1.8): should NOT prime debouncer
        assert!(engine.evaluate_scalar(100, 1.8).is_none());
        assert_eq!(engine.state(), HysteresisBandState::Unprimed);

        // Crossing to 2.2 without arming low: NO trigger
        assert!(engine.evaluate_scalar(200, 2.2).is_none());
        assert_eq!(engine.state(), HysteresisBandState::Unprimed);

        // Drops to 1.4 (<= 1.5): Arms Low!
        assert!(engine.evaluate_scalar(300, 1.4).is_none());
        assert_eq!(engine.state(), HysteresisBandState::ArmedLow);

        // Crosses from 1.4 to 2.4: Trigger fires!
        let hit = engine
            .evaluate_scalar(400, 2.4)
            .expect("should fire rising trigger");
        assert_eq!(hit.timestamp_ns, 300);
        // alpha = (2.0 - 1.4) / (2.4 - 1.4) = 0.6 / 1.0 = 0.6. dt = 100. offset = 60.0 ns
        assert!((hit.subpixel_offset_ns - 60.0).abs() < 1e-4);
        assert_eq!(engine.state(), HysteresisBandState::Unprimed);

        // Intra-band noise oscillation between 1.8 and 2.2: 0 triggers!
        assert!(engine.evaluate_scalar(500, 1.8).is_none());
        assert!(engine.evaluate_scalar(600, 2.2).is_none());
        assert!(engine.evaluate_scalar(700, 1.9).is_none());
        assert!(engine.evaluate_scalar(800, 2.1).is_none());
        assert_eq!(engine.state(), HysteresisBandState::Unprimed);

        // Visits 1.3 (<= 1.5): Re-arms Low
        assert!(engine.evaluate_scalar(900, 1.3).is_none());
        assert_eq!(engine.state(), HysteresisBandState::ArmedLow);

        // Crosses to 2.3: Second trigger fires!
        assert!(engine.evaluate_scalar(1000, 2.3).is_some());
    }

    #[test]
    fn test_hysteresis_debouncing_falling() {
        // Level = 0.0, Hysteresis = 1.0 -> V_low = -1.0, V_high = 1.0
        let cond = TriggerCondition::Edge {
            channel: ChannelId(1),
            level: 0.0,
            hysteresis: 1.0,
            slope: EdgeSlope::Falling,
        };
        let mut engine = TriggerEngine::new(cond);

        // Visits 1.5 (>= 1.0): Arms High!
        assert!(engine.evaluate_scalar(100, 1.5).is_none());
        assert_eq!(engine.state(), HysteresisBandState::ArmedHigh);

        // Drops from 1.5 to -0.5: Fires falling trigger!
        let hit = engine
            .evaluate_scalar(200, -0.5)
            .expect("should fire falling trigger");
        assert_eq!(hit.timestamp_ns, 100);
        // alpha = (0.0 - 1.5) / (-0.5 - 1.5) = -1.5 / -2.0 = 0.75. dt = 100 -> offset = 75.0 ns
        assert!((hit.subpixel_offset_ns - 75.0).abs() < 1e-4);
        assert_eq!(engine.state(), HysteresisBandState::Unprimed);

        // Intra-band noise between -0.8 and 0.8: 0 triggers!
        assert!(engine.evaluate_scalar(300, 0.5).is_none());
        assert!(engine.evaluate_scalar(400, -0.5).is_none());
        assert_eq!(engine.state(), HysteresisBandState::Unprimed);

        // Rises to 1.2: Re-arms High
        assert!(engine.evaluate_scalar(500, 1.2).is_none());
        assert_eq!(engine.state(), HysteresisBandState::ArmedHigh);

        // Drops to -0.2: Second trigger fires!
        assert!(engine.evaluate_scalar(600, -0.2).is_some());
    }

    #[test]
    fn test_hysteresis_debouncing_either() {
        // Level = 0.0, Hysteresis = 1.0 -> V_low = -1.0, V_high = 1.0
        let cond = TriggerCondition::Edge {
            channel: ChannelId(1),
            level: 0.0,
            hysteresis: 1.0,
            slope: EdgeSlope::Either,
        };
        let mut engine = TriggerEngine::new(cond);

        // Signal visits -1.5 -> Arms Low
        assert!(engine.evaluate_scalar(100, -1.5).is_none());
        assert_eq!(engine.state(), HysteresisBandState::ArmedLow);

        // Rises to 1.5 -> Fires Rising trigger and transitions directly to ArmedHigh!
        let hit1 = engine
            .evaluate_scalar(200, 1.5)
            .expect("should fire rising on either");
        assert_eq!(hit1.timestamp_ns, 100);
        assert_eq!(engine.state(), HysteresisBandState::ArmedHigh);

        // Drops to -1.5 -> Fires Falling trigger and transitions directly to ArmedLow!
        let hit2 = engine
            .evaluate_scalar(300, -1.5)
            .expect("should fire falling on either");
        assert_eq!(hit2.timestamp_ns, 200);
        assert_eq!(engine.state(), HysteresisBandState::ArmedLow);
    }

    #[test]
    fn test_discrete_state_trigger() {
        let cond = TriggerCondition::StateEquals {
            channel: ChannelId(2),
            state_code: 42,
        };
        let mut engine = TriggerEngine::new(cond);

        // Code 0: not matching
        assert!(engine.evaluate_state(100, 0).is_none());

        // Code 1: not matching
        assert!(engine.evaluate_state(200, 1).is_none());

        // Transitions to 42: Fires trigger with offset 0.0
        let hit = engine
            .evaluate_state(300, 42)
            .expect("should trigger on state 42");
        assert_eq!(hit.timestamp_ns, 300);
        assert_eq!(hit.subpixel_offset_ns, 0.0);

        // Remains in 42: No re-trigger
        assert!(engine.evaluate_state(400, 42).is_none());

        // Transitions to 5, then back to 42: Fires trigger again!
        assert!(engine.evaluate_state(500, 5).is_none());
        assert!(engine.evaluate_state(600, 42).is_some());
    }

    #[test]
    fn test_evaluate_batch_scalar() {
        let cond = TriggerCondition::Edge {
            channel: ChannelId(1),
            level: 0.0,
            hysteresis: 0.5,
            slope: EdgeSlope::Rising,
        };
        let mut engine = TriggerEngine::new(cond);

        let timestamps = vec![100, 200, 300, 400];
        let values = vec![-1.0, -0.8, 0.5, 0.6];

        let hit = engine
            .evaluate_batch_scalar(&timestamps, &values)
            .expect("batch should detect rising edge");
        assert_eq!(hit.timestamp_ns, 200);
        assert!(hit.subpixel_offset_ns > 0.0);
    }

    #[test]
    fn test_scope_controller_roll_mode() {
        let config = ScopeControllerConfig {
            sweep_mode: TriggerSweepMode::Roll,
            time_span_ns: 10_000,
            ..Default::default()
        };
        let mut ctrl = ScopeController::new(config);
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::Roll);

        let outcome = ctrl.process_scalar_sample(ChannelId(1), 50_000, 1.23);
        match outcome {
            ScopeStepOutcome::Rolling { window } => {
                assert_eq!(window.mode, TriggerSweepMode::Roll);
                assert_eq!(window.trigger_state, TriggerRuntimeState::Roll);
                assert_eq!(window.win_t_start_ns, 40_000);
                assert_eq!(window.win_t_span_ns, 10_000);
                assert_eq!(window.subpixel_offset_ns, 0.0);
            }
            _ => panic!("expected Rolling outcome"),
        }
    }

    #[test]
    fn test_scope_controller_auto_timeout() {
        let config = ScopeControllerConfig {
            sweep_mode: TriggerSweepMode::Auto,
            time_span_ns: 10_000,
            auto_timeout_ns: 100_000_000, // 100 ms
            trigger: Some(TriggerCondition::Edge {
                channel: ChannelId(1),
                level: 2.0,
                hysteresis: 0.5,
                slope: EdgeSlope::Rising,
            }),
            ..Default::default()
        };
        let mut ctrl = ScopeController::new(config);
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);

        // Sample at 10ms (no trigger): waiting
        let outcome1 = ctrl.process_scalar_sample(ChannelId(1), 10_000_000, 0.0);
        assert_eq!(outcome1, ScopeStepOutcome::WaitingForTrigger);

        // Step at 50ms: still waiting
        let outcome2 = ctrl.step(50_000_000);
        assert_eq!(outcome2, ScopeStepOutcome::WaitingForTrigger);

        // Step at 110ms (> 100ms timeout since start): Auto freerun fires!
        let outcome3 = ctrl.step(110_000_001);
        match outcome3 {
            ScopeStepOutcome::AutoFreerunFired { window } => {
                assert_eq!(window.mode, TriggerSweepMode::Auto);
                assert_eq!(window.trigger_state, TriggerRuntimeState::AutoFreerun);
                assert_eq!(window.subpixel_offset_ns, 0.0);
                assert!(window.trigger_timestamp_ns.is_none());
            }
            _ => panic!("expected AutoFreerunFired outcome, got {:?}", outcome3),
        }

        // Controller immediately re-arms back to Armed
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);
    }

    #[test]
    fn test_scope_controller_normal_mode_waits_indefinitely() {
        let config = ScopeControllerConfig {
            sweep_mode: TriggerSweepMode::Normal,
            time_span_ns: 1_000,
            auto_timeout_ns: 100_000_000,
            pre_trigger_ratio: 0.5,
            trigger: Some(TriggerCondition::Edge {
                channel: ChannelId(1),
                level: 0.0,
                hysteresis: 0.5,
                slope: EdgeSlope::Rising,
            }),
            ..Default::default()
        };
        let mut ctrl = ScopeController::new(config);
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);

        // Advance 500ms without trigger: stays Armed, never auto-freeruns
        let step_res = ctrl.step(500_000_000);
        assert_eq!(step_res, ScopeStepOutcome::WaitingForTrigger);
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);

        // Signal arms low at -1.0
        assert_eq!(
            ctrl.process_scalar_sample(ChannelId(1), 500_000_100, -1.0),
            ScopeStepOutcome::WaitingForTrigger
        );

        // Crossing at 500_000_200 to 1.0 -> Triggers!
        let trig_res = ctrl.process_scalar_sample(ChannelId(1), 500_000_200, 1.0);
        // Pre-span = 500, post-span = 500. Target end = 500_000_100 + 500 = 500_000_600
        match trig_res {
            ScopeStepOutcome::TriggerDetected { hit } => {
                assert_eq!(hit.timestamp_ns, 500_000_100);
                assert_eq!(ctrl.current_state(), TriggerRuntimeState::Triggered);
            }
            _ => panic!("expected TriggerDetected outcome"),
        }

        // Next sample at 500_000_700 completes window
        let comp_res = ctrl.process_scalar_sample(ChannelId(1), 500_000_700, 1.1);
        match comp_res {
            ScopeStepOutcome::CaptureCompleted {
                window,
                latched_pinned,
            } => {
                assert!(!latched_pinned);
                assert_eq!(window.mode, TriggerSweepMode::Normal);
                assert_eq!(window.trigger_state, TriggerRuntimeState::Triggered);
                assert_eq!(window.trigger_timestamp_ns, Some(500_000_100));
            }
            _ => panic!("expected CaptureCompleted outcome"),
        }
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);
    }

    #[test]
    fn test_scope_controller_single_mode_pinning_and_rearm() {
        let config = ScopeControllerConfig {
            sweep_mode: TriggerSweepMode::Single,
            time_span_ns: 1_000,
            pre_trigger_ratio: 0.5,
            trigger: Some(TriggerCondition::Edge {
                channel: ChannelId(1),
                level: 0.0,
                hysteresis: 0.5,
                slope: EdgeSlope::Rising,
            }),
            ..Default::default()
        };
        let mut ctrl = ScopeController::new(config);
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);
        assert!(!ctrl.is_pinned());

        // Prime low
        ctrl.process_scalar_sample(ChannelId(1), 1_000, -1.0);

        // Edge crossing at 1_100 (t_trig = 1000). Target end = 1000 + 500 = 1500.
        let out = ctrl.process_scalar_sample(ChannelId(1), 1_100, 1.0);
        assert!(matches!(out, ScopeStepOutcome::TriggerDetected { .. }));
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::Triggered);

        // Sample at 1_600 finishes capture: enters LatchedPinned!
        let out_comp = ctrl.process_scalar_sample(ChannelId(1), 1_600, 1.0);
        match out_comp {
            ScopeStepOutcome::CaptureCompleted {
                window,
                latched_pinned,
            } => {
                assert!(latched_pinned);
                assert_eq!(window.mode, TriggerSweepMode::Single);
                assert_eq!(window.trigger_state, TriggerRuntimeState::LatchedPinned);
            }
            _ => panic!("expected CaptureCompleted with latched_pinned"),
        }
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::LatchedPinned);
        assert!(ctrl.is_pinned());

        // Further samples are ignored while pinned
        let out_subsequent = ctrl.process_scalar_sample(ChannelId(1), 2_000, -1.0);
        assert_eq!(out_subsequent, ScopeStepOutcome::NoChange);
        assert!(ctrl.is_pinned());

        // Explicit rearm() unlocks latch and returns to Armed
        ctrl.rearm();
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);
        assert!(!ctrl.is_pinned());
    }

    #[test]
    fn test_scope_controller_force_trigger() {
        let config = ScopeControllerConfig {
            sweep_mode: TriggerSweepMode::Normal,
            time_span_ns: 2_000,
            pre_trigger_ratio: 0.5,
            ..Default::default()
        };
        let mut ctrl = ScopeController::new(config);
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);

        // Force trigger at 50_000. Target end = 50_000 + 1_000 = 51_000.
        let out = ctrl.force_trigger(50_000);
        match out {
            ScopeStepOutcome::TriggerDetected { hit } => {
                assert_eq!(hit.timestamp_ns, 50_000);
                assert_eq!(hit.subpixel_offset_ns, 0.0);
                assert_eq!(ctrl.current_state(), TriggerRuntimeState::Triggered);
            }
            _ => panic!("expected TriggerDetected outcome"),
        }

        // Advance past end
        let out2 = ctrl.step(52_000);
        assert!(matches!(out2, ScopeStepOutcome::CaptureCompleted { .. }));
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);
    }

    #[test]
    fn test_scope_controller_mode_switch() {
        let config = ScopeControllerConfig {
            sweep_mode: TriggerSweepMode::Single,
            time_span_ns: 1_000,
            ..Default::default()
        };
        let mut ctrl = ScopeController::new(config);
        ctrl.force_trigger(10_000);
        ctrl.step(20_000);
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::LatchedPinned);
        assert!(ctrl.is_pinned());

        // Mode switch to Roll unpins and transitions to Roll
        ctrl.set_sweep_mode(TriggerSweepMode::Roll);
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::Roll);
        assert!(!ctrl.is_pinned());

        // Mode switch to Auto transitions to Armed
        ctrl.set_sweep_mode(TriggerSweepMode::Auto);
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);
    }

    #[test]
    fn test_hysteresis_zero_band() {
        // Hysteresis = 0.0 -> classic threshold detector
        let cond = TriggerCondition::Edge {
            channel: ChannelId(1),
            level: 1.0,
            hysteresis: 0.0,
            slope: EdgeSlope::Rising,
        };
        let mut engine = TriggerEngine::new(cond);

        // Sample below level: primes ArmedLow
        assert!(engine.evaluate_scalar(100, 0.5).is_none());
        assert_eq!(engine.state(), HysteresisBandState::ArmedLow);

        // Crossing above level: fires trigger!
        let hit = engine
            .evaluate_scalar(200, 1.5)
            .expect("crossing with h=0 should trigger");
        assert_eq!(hit.timestamp_ns, 100);
        // alpha = (1.0 - 0.5) / (1.5 - 0.5) = 0.5
        assert!((hit.subpixel_offset_ns - 50.0).abs() < 1e-4);
    }

    #[test]
    fn test_hysteresis_noise_rejection_multi_cycle() {
        // Level = 0.0, Hysteresis = 1.0. Lower = -1.0, Upper = +1.0.
        let cond = TriggerCondition::Edge {
            channel: ChannelId(1),
            level: 0.0,
            hysteresis: 1.0,
            slope: EdgeSlope::Rising,
        };
        let mut engine = TriggerEngine::new(cond);

        // Start within band
        assert!(engine.evaluate_scalar(0, 0.0).is_none());

        // 100 oscillations strictly inside band [-0.8, +0.8]
        for i in 1..=100 {
            let val = if i % 2 == 0 { 0.8 } else { -0.8 };
            assert!(
                engine.evaluate_scalar(i * 10, val).is_none(),
                "intra-band oscillation must not trigger at cycle {}",
                i
            );
        }
        assert_eq!(engine.state(), HysteresisBandState::Unprimed);

        // Drop below -1.0 to arm
        assert!(engine.evaluate_scalar(1010, -1.2).is_none());
        assert_eq!(engine.state(), HysteresisBandState::ArmedLow);

        // Cross level to 0.5 -> trigger!
        assert!(engine.evaluate_scalar(1020, 0.5).is_some());
    }

    #[test]
    fn test_scope_controller_with_state_trigger() {
        let config = ScopeControllerConfig {
            sweep_mode: TriggerSweepMode::Single,
            time_span_ns: 10_000,
            pre_trigger_ratio: 0.2, // Pre-span: 2000, post-span: 8000
            trigger: Some(TriggerCondition::StateEquals {
                channel: ChannelId(3),
                state_code: 7,
            }),
            ..Default::default()
        };
        let mut ctrl = ScopeController::new(config);
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::Armed);

        // State 0
        assert_eq!(
            ctrl.process_state_sample(ChannelId(3), 10_000, 0),
            ScopeStepOutcome::WaitingForTrigger
        );

        // Transition to 7 at t = 20_000 -> Triggers! Target end = 20_000 + 8_000 = 28_000
        let trig_res = ctrl.process_state_sample(ChannelId(3), 20_000, 7);
        match trig_res {
            ScopeStepOutcome::TriggerDetected { hit } => {
                assert_eq!(hit.timestamp_ns, 20_000);
                assert_eq!(hit.subpixel_offset_ns, 0.0);
            }
            _ => panic!("expected TriggerDetected for state trigger"),
        }
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::Triggered);

        // Advance past 28_000 to complete capture
        let comp_res = ctrl.process_state_sample(ChannelId(3), 30_000, 7);
        match comp_res {
            ScopeStepOutcome::CaptureCompleted {
                window,
                latched_pinned,
            } => {
                assert!(latched_pinned);
                assert_eq!(window.win_t_start_ns, 20_000 - 2000);
                assert_eq!(window.win_t_span_ns, 10_000);
                assert_eq!(window.subpixel_offset_ns, 0.0);
            }
            _ => panic!("expected CaptureCompleted"),
        }
        assert_eq!(ctrl.current_state(), TriggerRuntimeState::LatchedPinned);
        assert!(ctrl.is_pinned());
    }

    #[test]
    fn test_scope_controller_pan_offset_and_pause() {
        let config = ScopeControllerConfig {
            sweep_mode: TriggerSweepMode::Roll,
            time_span_ns: 10_000,
            pan_offset_ns: 2_500,
            ..Default::default()
        };
        let mut ctrl = ScopeController::new(config);

        let out = ctrl.process_scalar_sample(ChannelId(1), 50_000, 1.0);
        match out {
            ScopeStepOutcome::Rolling { window } => {
                // win_start = (50_000 - 10_000) + 2_500 = 42_500
                assert_eq!(window.win_t_start_ns, 42_500);
            }
            _ => panic!("expected Rolling"),
        }

        // Pause
        ctrl.set_paused(true);
        assert!(ctrl.is_paused());
        assert_eq!(
            ctrl.process_scalar_sample(ChannelId(1), 60_000, 2.0),
            ScopeStepOutcome::NoChange
        );
        assert_eq!(ctrl.step(70_000), ScopeStepOutcome::NoChange);

        // Unpause
        ctrl.set_paused(false);
        assert!(!ctrl.is_paused());
        assert!(matches!(
            ctrl.process_scalar_sample(ChannelId(1), 80_000, 3.0),
            ScopeStepOutcome::Rolling { .. }
        ));
    }

    #[test]
    fn test_interface_contract_forwarding_aliases_and_config_sanitization() {
        let config = ScopeControllerConfig {
            time_span_ns: 200,                // Below minimum 1000
            pre_trigger_ratio: f64::NAN,      // Non-finite
            sweep_mode: TriggerSweepMode::Auto,
            ..Default::default()
        };
        let mut ctrl = ScopeController::new(config);
        assert_eq!(ctrl.config().time_span_ns, 1000);
        assert_eq!(ctrl.config().pre_trigger_ratio, 0.5);
        assert_eq!(ctrl.state(), TriggerRuntimeState::Armed);

        // Test set_mode alias
        ctrl.set_mode(TriggerSweepMode::Single);
        assert_eq!(ctrl.state(), TriggerRuntimeState::Armed);
        assert_eq!(ctrl.sweep_mode(), TriggerSweepMode::Single);

        // Test update_config sanitization
        ctrl.update_config(ScopeControllerConfig {
            time_span_ns: 50,
            pre_trigger_ratio: 2.5,
            sweep_mode: TriggerSweepMode::Roll,
            ..Default::default()
        });
        assert_eq!(ctrl.config().time_span_ns, 1000);
        assert_eq!(ctrl.config().pre_trigger_ratio, 1.0);
        assert_eq!(ctrl.state(), TriggerRuntimeState::Roll);

        // Test TriggerEngine evaluate and band_state aliases
        let cond = TriggerCondition::Edge {
            channel: ChannelId(1),
            level: 0.0,
            hysteresis: 1.0,
            slope: EdgeSlope::Rising,
        };
        let mut engine = TriggerEngine::new(cond);
        assert_eq!(engine.band_state(), HysteresisBandState::Unprimed);

        assert!(engine.evaluate(0, -1.5).is_none());
        assert_eq!(engine.band_state(), HysteresisBandState::ArmedLow);

        let hit = engine
            .evaluate(10, 0.5)
            .expect("evaluate alias must detect crossing");
        assert_eq!(hit.timestamp_ns, 0);
        assert_eq!(engine.band_state(), HysteresisBandState::Unprimed);
    }
}
