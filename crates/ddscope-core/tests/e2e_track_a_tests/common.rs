//! Shared test fixtures, synthetic deterministic signal generators, assertion macros,
//! and contract harnesses for DDScopeHost Phase 1 Track A E2E Test Suite.

pub use ddscope_core::scope::controller::*;
pub use ddscope_core::scope::ddsf::*;
pub use ddscope_core::scope::sync::*;
pub use ddscope_core::scope::*;
pub use ddscope_core::storage::m4::*;
pub use ddscope_core::storage::quota::*;
pub use ddscope_core::storage::*;
pub use ddscope_core::types::*;

// ============================================================================
// Specialized Assertion Macros
// ============================================================================

/// Float comparison macro with default epsilon of 1e-4.
#[macro_export]
macro_rules! assert_float_near {
    ($left:expr, $right:expr, $eps:expr) => {
        let (l, r, eps): (f64, f64, f64) = ($left as f64, $right as f64, $eps as f64);
        assert!(
            (l - r).abs() <= eps,
            "Float assertion failed: |{} - {}| = {} > {}",
            l,
            r,
            (l - r).abs(),
            eps
        );
    };
    ($left:expr, $right:expr) => {
        $crate::assert_float_near!($left, $right, 1e-4);
    };
}

// ============================================================================
// Deterministic Synthetic Waveform Generators
// ============================================================================

/// Generates monotonic nanosecond timestamps with uniform delta.
pub fn generate_timestamps(start_ns: u64, count: usize, step_ns: u64) -> Vec<TimestampNs> {
    (0..count).map(|i| start_ns + (i as u64) * step_ns).collect()
}

/// Pure Sine Wave Generator with explicit phase and DC offset (f64 values).
pub fn generate_sine_wave(
    sample_rate_hz: f64,
    freq_hz: f64,
    amplitude: f64,
    offset: f64,
    phase_rad: f64,
    count: usize,
) -> (Vec<u64>, Vec<f64>) {
    let dt_ns = if sample_rate_hz > 0.0 {
        (1_000_000_000.0 / sample_rate_hz).max(1.0) as u64
    } else {
        1
    };
    let mut timestamps = Vec::with_capacity(count);
    let mut values = Vec::with_capacity(count);
    for i in 0..count {
        let t_ns = (i as u64) * dt_ns;
        let t_sec = (i as f64) / sample_rate_hz.max(1.0);
        let val =
            offset + amplitude * (2.0 * std::f64::consts::PI * freq_hz * t_sec + phase_rad).sin();
        timestamps.push(t_ns);
        values.push(val);
    }
    (timestamps, values)
}

/// Pure Sine Wave Generator returning f32 values for decimation and M4 tests.
pub fn generate_sine_wave_f32(
    count: usize,
    sample_rate_hz: f64,
    freq_hz: f64,
    amplitude: f32,
    phase_rad: f32,
) -> Vec<f32> {
    let dt = 1.0 / sample_rate_hz.max(1.0);
    (0..count)
        .map(|i| {
            let t = (i as f64) * dt;
            amplitude * ((2.0 * std::f64::consts::PI * freq_hz * t) as f32 + phase_rad).sin()
        })
        .collect()
}

/// Deterministic Square Wave Generator with parameterized duty cycle.
pub fn generate_square_wave(
    sample_rate_hz: f64,
    freq_hz: f64,
    duty_cycle: f64,
    low_val: f64,
    high_val: f64,
    count: usize,
) -> (Vec<u64>, Vec<f64>) {
    let dt_ns = (1_000_000_000.0 / sample_rate_hz.max(1.0)).max(1.0) as u64;
    let period_samples = (sample_rate_hz / freq_hz.max(1.0)).round().max(2.0) as usize;
    let high_samples = ((period_samples as f64) * duty_cycle.clamp(0.0, 1.0)).round() as usize;
    let mut timestamps = Vec::with_capacity(count);
    let mut values = Vec::with_capacity(count);
    for i in 0..count {
        let t_ns = (i as u64) * dt_ns;
        let is_high = (i % period_samples) < high_samples;
        timestamps.push(t_ns);
        values.push(if is_high { high_val } else { low_val });
    }
    (timestamps, values)
}

/// Digital PWM Bitstream Generator.
pub fn generate_pwm_bits(count: usize, period_samples: usize, duty_cycle: f64) -> Vec<bool> {
    let period = period_samples.max(1);
    let high_samples = ((period as f64) * duty_cycle.clamp(0.0, 1.0)).round() as usize;
    (0..count).map(|i| (i % period) < high_samples).collect()
}

/// Synthetic 1-bit logic square wave bitstream (50% duty cycle).
pub fn generate_square_wave_bits(count: usize, period_samples: usize) -> Vec<bool> {
    let half = (period_samples / 2).max(1);
    (0..count).map(|i| (i % period_samples.max(1)) < half).collect()
}

/// Glitch Injector: Modifies signal array with single-sample narrow voltage spikes.
pub fn inject_glitches(values: &mut [f64], glitches: &[(usize, f64)]) {
    for &(idx, val) in glitches {
        if idx < values.len() {
            values[idx] = val;
        }
    }
}

/// Injects glitches into an f32 slice.
pub fn inject_glitches_f32(values: &mut [f32], glitches: &[(usize, f32)]) {
    for &(idx, val) in glitches {
        if idx < values.len() {
            values[idx] = val;
        }
    }
}

/// Baseline signal with a single-sample glitch spike.
pub fn generate_glitch_signal(
    count: usize,
    glitch_index: usize,
    glitch_value: f32,
    baseline: f32,
) -> Vec<f32> {
    let mut data = vec![baseline; count];
    if glitch_index < count {
        data[glitch_index] = glitch_value;
    }
    data
}

/// Linear ramp signal from `start` to `end`.
pub fn generate_ramp(count: usize, start: f32, end: f32) -> Vec<f32> {
    if count == 0 {
        return Vec::new();
    }
    if count == 1 {
        return vec![start];
    }
    let step = (end - start) / ((count - 1) as f32);
    (0..count).map(|i| start + (i as f32) * step).collect()
}

/// Linear Ramp Generator for Sub-Sample Interpolation Precision Testing.
pub fn generate_linear_ramp(
    start_ns: u64,
    step_ns: u64,
    v_start: f64,
    v_step: f64,
    count: usize,
) -> (Vec<u64>, Vec<f64>) {
    let mut timestamps = Vec::with_capacity(count);
    let mut values = Vec::with_capacity(count);
    for i in 0..count {
        timestamps.push(start_ns + (i as u64) * step_ns);
        values.push(v_start + (i as f64) * v_step);
    }
    (timestamps, values)
}

/// Deterministic lightweight Xorshift64 PRNG for reproducible pseudo-random noise.
#[derive(Clone, Debug)]
pub struct XorShift64 {
    state: u64,
}

impl XorShift64 {
    pub fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 {
                0x5EED_CAFE_BABE_FACE
            } else {
                seed
            },
        }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state ^= self.state << 13;
        self.state ^= self.state >> 7;
        self.state ^= self.state << 17;
        self.state
    }

    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() as f64) / (u64::MAX as f64)
    }
}

/// Deterministic Noisy Signal Generator for Hysteresis Debouncing.
pub fn generate_noisy_edge(
    sample_rate_hz: f64,
    edge_center_sample: usize,
    low_level: f64,
    high_level: f64,
    noise_amplitude: f64,
    seed: u64,
    count: usize,
) -> (Vec<u64>, Vec<f64>) {
    let dt_ns = (1_000_000_000.0 / sample_rate_hz.max(1.0)).max(1.0) as u64;
    let mut rng = XorShift64::new(seed);
    let mut timestamps = Vec::with_capacity(count);
    let mut values = Vec::with_capacity(count);

    for i in 0..count {
        let t_ns = (i as u64) * dt_ns;
        let base = if i < edge_center_sample {
            low_level
        } else {
            high_level
        };
        let noise = (rng.next_f64() * 2.0 - 1.0) * noise_amplitude;
        timestamps.push(t_ns);
        values.push(base + noise);
    }
    (timestamps, values)
}

/// Three-Phase Motor Inverter Currents Generator (120-deg separation).
pub fn generate_three_phase_inverter(
    sample_rate_hz: f64,
    fundamental_hz: f64,
    amplitude: f64,
    count: usize,
) -> (Vec<u64>, Vec<f64>, Vec<f64>, Vec<f64>) {
    let dt_ns = (1_000_000_000.0 / sample_rate_hz.max(1.0)).max(1.0) as u64;
    let mut ts = Vec::with_capacity(count);
    let mut ia = Vec::with_capacity(count);
    let mut ib = Vec::with_capacity(count);
    let mut ic = Vec::with_capacity(count);

    let two_pi = 2.0 * std::f64::consts::PI;
    let offset_b = 2.0 * std::f64::consts::PI / 3.0; // 120 deg
    let offset_c = 4.0 * std::f64::consts::PI / 3.0; // 240 deg

    for i in 0..count {
        let t_ns = (i as u64) * dt_ns;
        let t_sec = (i as f64) / sample_rate_hz.max(1.0);
        let theta = two_pi * fundamental_hz * t_sec;
        ts.push(t_ns);
        ia.push(amplitude * theta.sin());
        ib.push(amplitude * (theta - offset_b).sin());
        ic.push(amplitude * (theta - offset_c).sin());
    }
    (ts, ia, ib, ic)
}

// ============================================================================
// Specialized Assertion Utilities
// ============================================================================

/// Asserts that a single-point peak or valley glitch is 100% preserved in an M4 query result.
pub fn assert_m4_extrema_retained(
    columns: &[DdsfM4Column],
    expected_peak: Option<f32>,
    expected_valley: Option<f32>,
) {
    if let Some(peak) = expected_peak {
        let found = columns.iter().any(|c| (c.v_max - peak).abs() < 1e-3);
        assert!(
            found,
            "expected peak {peak} not preserved in M4 columns v_max"
        );
    }
    if let Some(val) = expected_valley {
        let found = columns.iter().any(|c| (c.v_min - val).abs() < 1e-3);
        assert!(
            found,
            "expected valley {val} not preserved in M4 columns v_min"
        );
    }
}

/// Asserts that sub-sample interpolation accurately calculates alpha and subpixel_offset_ns.
pub fn assert_subsample_interpolation_accuracy(
    v0: f64,
    v1: f64,
    level: f64,
    t0_ns: u64,
    t1_ns: u64,
    subpixel_offset_ns: f32,
) {
    let expected_alpha = ((level - v0) / (v1 - v0)) as f32;
    let expected_offset = expected_alpha * ((t1_ns - t0_ns) as f32);
    let diff = (subpixel_offset_ns - expected_offset).abs();
    assert!(
        diff < 1e-3,
        "subpixel offset {subpixel_offset_ns} differs from expected {expected_offset} by {diff}"
    );
}

/// Asserts that a serialized DDSF frame buffer matches all header alignment and length constraints.
pub fn assert_ddsf_frame_valid(
    buffer: &[u8],
    expected_seq: u32,
    expected_scope_count: u16,
    expected_snapshot_count: u16,
) {
    assert!(buffer.len() >= 64, "frame buffer smaller than 64-byte header");
    let header: &DdsfFrameHeader = bytemuck::from_bytes(&buffer[0..64]);
    assert_eq!(header.magic, DDSF_MAGIC, "magic mismatch");
    assert_eq!(header.version, DDSF_VERSION_V1, "version mismatch");
    assert_eq!(header.frame_seq, expected_seq, "sequence mismatch");
    assert_eq!(header.scope_count, expected_scope_count, "scope count mismatch");
    assert_eq!(
        header.snapshot_count, expected_snapshot_count,
        "snapshot count mismatch"
    );
    assert_eq!(
        header.total_byte_len as usize,
        buffer.len(),
        "length mismatch"
    );
    assert_eq!(
        (buffer.as_ptr() as usize) % 64,
        0,
        "buffer not 64-byte aligned"
    );
}

// ============================================================================
// Controller & Trigger Helper Builders
// ============================================================================

/// Constructs an edge trigger engine for tests.
pub fn make_edge_trigger_engine(
    channel: ChannelId,
    level: f64,
    hysteresis: f64,
    slope: EdgeSlope,
) -> TriggerEngine {
    TriggerEngine::new(TriggerCondition::Edge {
        channel,
        level,
        hysteresis,
        slope,
    })
}

/// Constructs a configured ScopeController for tests.
pub fn make_scope_controller(
    scope_id: u32,
    mode: TriggerSweepMode,
    time_span_ns: u64,
    auto_timeout_ns: u64,
) -> ScopeController {
    ScopeController::new(ScopeControllerConfig {
        scope_id: ScopeId(scope_id),
        sweep_mode: mode,
        time_span_ns,
        auto_timeout_ns,
        ..Default::default()
    })
}

// ============================================================================
// Genuine Production Storage Quota & Frame Packer Re-exports
// ============================================================================

pub use ddscope_core::storage::quota::{
    MemoryQuotaManager, PinnedGuard, DEFAULT_MEMORY_QUOTA_BYTES, DEFAULT_PINNED_LIMIT_PERCENT,
    DEFAULT_QUOTA_BYTES, DEFAULT_WARNING_PERCENT, MemoryQuotaStats, QuotaError,
    PINNED_QUOTA_MAX_FRACTION, WARNING_THRESHOLD_FRACTION,
};
pub use ddscope_core::scope::ddsf::{
    DdsfFramePacker, DdsfScopeIter, DdsfScopePacketView, DdsfTrackBlockView, DdsfTrackIter,
    DdsfZeroCopyReader, ScopePacketView, ScopeViewportPayload, TrackBlockView, TrackPayload,
    TrackPayloadSlice,
};

pub const WARNING_THRESHOLD_PERCENT: usize = DEFAULT_WARNING_PERCENT;
pub const PINNED_CAP_PERCENT: usize = DEFAULT_PINNED_LIMIT_PERCENT;
