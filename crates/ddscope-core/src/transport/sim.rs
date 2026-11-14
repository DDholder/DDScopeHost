//! Built-in high-speed mixed-signal simulation transport (`FR-IO-01`, §3.1.2).
//!
//! [`SimSignalTransport`] generates deterministic multi-channel analog waveforms,
//! digital PWM/fault logic, and a cycling motor-control state machine packed directly
//! into [`PacketBufferPool`] slabs with zero per-sample heap allocations.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::{BufMut, BytesMut};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use super::buffer_pool::{BufferPoolConfig, BufferPoolStats};
use super::scheduler::{host_monotonic_ns, RxBackpressurePolicy, RxPacketEmitter};
use super::{
    OutboundFrame, OutboundReceiver, PacketSender, ShutdownReceiver, Transport, TransportCaps,
    TransportKind,
};
use crate::types::{CoreError, CoreResult};

/// Two-byte header magic identifying a canonical `SimPreset` wire frame (`[0xDD, 0x53]`).
pub const SIM_PRESET_MAGIC: [u8; 2] = [0xDD, 0x53];

/// Two-byte frame tail terminating a canonical `SimPreset` wire frame (`[0x55, 0xAA]`).
pub const SIM_PRESET_TAIL: [u8; 2] = [0x55, 0xAA];

/// Standard VOFA+ `JustFloat` 4-byte little-endian `f32` frame tail (`+inf`/NaN sentinel).
pub const JUST_FLOAT_TAIL: [u8; 4] = [0x00, 0x00, 0x80, 0x7F];

/// `flags_bits` bit 0: `pwm_a` digital logic state.
pub const SIM_FLAG_PWM_A: u8 = 1 << 0;
/// `flags_bits` bit 1: `pwm_b` digital logic state.
pub const SIM_FLAG_PWM_B: u8 = 1 << 1;
/// `flags_bits` bit 2: `fault_pin` digital logic state (asserts on glitch or `OCP_FAULT`).
pub const SIM_FLAG_FAULT_PIN: u8 = 1 << 2;
/// `flags_bits` bit 7: `glitch_active` single-sample over-voltage glitch marker.
pub const SIM_FLAG_GLITCH_ACTIVE: u8 = 1 << 7;

/// Motor-control state code `0`: `IDLE`.
pub const CTRL_STATE_IDLE: u8 = 0;
/// Motor-control state code `1`: `ALIGN`.
pub const CTRL_STATE_ALIGN: u8 = 1;
/// Motor-control state code `2`: `CLOSED_LOOP`.
pub const CTRL_STATE_CLOSED_LOOP: u8 = 2;
/// Motor-control state code `3`: `OCP_FAULT`.
pub const CTRL_STATE_OCP_FAULT: u8 = 3;

/// Minimum supported simulation sample rate in Hz (`1 kHz`).
pub const MIN_SIM_SAMPLE_RATE_HZ: f64 = 1_000.0;
/// Maximum supported simulation sample rate in Hz (`10 MHz`).
pub const MAX_SIM_SAMPLE_RATE_HZ: f64 = 10_000_000.0;
/// Minimum supported analog channel count.
pub const MIN_SIM_ANALOG_CHANNELS: u8 = 1;
/// Maximum supported analog channel count.
pub const MAX_SIM_ANALOG_CHANNELS: u8 = 16;

const DEFAULT_STATE_DWELL_SAMPLES: u64 = 250;
const HYBRID_SLEEP_THRESHOLD_US: u64 = 1_500;
const SPIN_THRESHOLD_US: u64 = 80;
const MAX_PACER_DEBT_NS: f64 = 100_000_000.0; // 100 ms debt cap against OS pauses
const UNPACED_YIELD_INTERVAL_PACKETS: u64 = 64;
const STATE_UNFORCED: u8 = 0xFF;

/// Wire encoding produced by [`SimSignalTransport`].
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SimWireFormat {
    /// Canonical `12 + 4 * A` byte mixed-signal binary frame shared with `SimPresetCodec`.
    #[default]
    #[serde(alias = "sim_preset")]
    SimPreset,
    /// VOFA+ `JustFloat` `A` LE `f32` values followed by `[0x00, 0x00, 0x80, 0x7F]`.
    #[serde(alias = "just_float")]
    JustFloat,
    /// Comma-separated ASCII float values terminated by `\n`.
    #[serde(alias = "fire_water_csv", alias = "fire_water")]
    FireWaterCsv,
}

/// Per-channel waveform synthesis mode for [`SimConfig::channel_waveforms`].
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SimWaveformKind {
    /// 3-phase motor currents (`ch0..ch2`), 24V DC bus (`ch3` or `ch{A-1}`), and auxiliary waves.
    #[default]
    #[serde(alias = "motor_3_phase_preset")]
    Motor3PhasePreset,
    /// Clean sinusoidal waveform with additive noise.
    #[serde(alias = "sine")]
    Sine,
    /// 50%-duty bipolar square wave with additive noise.
    #[serde(alias = "square")]
    Square,
    /// Symmetric bipolar triangle wave with additive noise.
    #[serde(alias = "triangle")]
    Triangle,
    /// Deterministic SplitMix64/Xoshiro white noise.
    #[serde(alias = "white_noise")]
    WhiteNoise,
}

fn default_sample_rate_hz() -> f64 {
    1_000_000.0
}

fn default_analog_channels() -> u8 {
    4
}

fn default_enable_glitch() -> bool {
    true
}

fn default_glitch_period_samples() -> u32 {
    100_000
}

fn default_glitch_amplitude() -> f32 {
    80.0
}

fn default_noise_amplitude() -> f32 {
    0.05
}

fn default_paced() -> bool {
    true
}

fn default_seed() -> u64 {
    0x243F_6A88_85A3_08D3
}

/// Configuration for [`SimSignalTransport`], compatible with `TransportConfigDto::Sim`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SimConfig {
    #[serde(default = "default_sample_rate_hz")]
    pub sample_rate_hz: f64,
    #[serde(default = "default_analog_channels")]
    pub analog_channels: u8,
    #[serde(default = "default_enable_glitch")]
    pub enable_glitch: bool,
    #[serde(default = "default_glitch_period_samples")]
    pub glitch_period_samples: u32,
    #[serde(default)]
    pub wire_format: SimWireFormat,
    #[serde(default)]
    pub channel_waveforms: Vec<SimWaveformKind>,
    #[serde(default = "default_glitch_amplitude")]
    pub glitch_amplitude: f32,
    #[serde(default = "default_noise_amplitude")]
    pub noise_amplitude: f32,
    #[serde(default = "default_paced")]
    pub paced: bool,
    #[serde(default)]
    pub max_samples: Option<u64>,
    #[serde(default = "default_seed")]
    pub seed: u64,
    #[serde(default)]
    pub rx_backpressure: RxBackpressurePolicy,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            sample_rate_hz: default_sample_rate_hz(),
            analog_channels: default_analog_channels(),
            enable_glitch: default_enable_glitch(),
            glitch_period_samples: default_glitch_period_samples(),
            wire_format: SimWireFormat::default(),
            channel_waveforms: Vec::new(),
            glitch_amplitude: default_glitch_amplitude(),
            noise_amplitude: default_noise_amplitude(),
            paced: default_paced(),
            max_samples: None,
            seed: default_seed(),
            rx_backpressure: RxBackpressurePolicy::default(),
        }
    }
}

impl SimConfig {
    /// Validates sample rate (`1_000.0..=10_000_000.0`), channel count (`1..=16`), and amplitudes.
    pub fn validate(&self) -> CoreResult<()> {
        validate_sample_rate_hz(self.sample_rate_hz)?;
        if !(MIN_SIM_ANALOG_CHANNELS..=MAX_SIM_ANALOG_CHANNELS).contains(&self.analog_channels) {
            return Err(CoreError::InvalidConfig(format!(
                "analog_channels must be in {MIN_SIM_ANALOG_CHANNELS}..={MAX_SIM_ANALOG_CHANNELS}, got {}",
                self.analog_channels
            )));
        }
        if !self.glitch_amplitude.is_finite() {
            return Err(CoreError::InvalidConfig(format!(
                "glitch_amplitude must be finite, got {}",
                self.glitch_amplitude
            )));
        }
        if !self.noise_amplitude.is_finite() || self.noise_amplitude < 0.0 {
            return Err(CoreError::InvalidConfig(format!(
                "noise_amplitude must be finite and >= 0.0, got {}",
                self.noise_amplitude
            )));
        }
        Ok(())
    }

    /// Returns the canonical byte size per encoded binary frame (`SimPreset` or `JustFloat`),
    /// or an upper-bound estimate per line for `FireWaterCsv`.
    #[inline]
    pub fn frame_byte_len(&self) -> usize {
        let a = self.analog_channels as usize;
        match self.wire_format {
            SimWireFormat::SimPreset => 12 + 4 * a,
            SimWireFormat::JustFloat => 4 + 4 * a,
            SimWireFormat::FireWaterCsv => 16 * a + 2,
        }
    }
}

fn validate_sample_rate_hz(hz: f64) -> CoreResult<()> {
    if !hz.is_finite() || !(MIN_SIM_SAMPLE_RATE_HZ..=MAX_SIM_SAMPLE_RATE_HZ).contains(&hz) {
        return Err(CoreError::InvalidConfig(format!(
            "sample_rate_hz must be in {MIN_SIM_SAMPLE_RATE_HZ}..={MAX_SIM_SAMPLE_RATE_HZ}, got {hz}"
        )));
    }
    Ok(())
}

#[derive(Debug)]
struct SimControlInner {
    sample_rate_bits: AtomicU64,
    rate_epoch: AtomicU64,
    pending_manual_glitches: AtomicU32,
    emitted_samples: AtomicU64,
    injected_glitches: AtomicU64,
    forced_ctrl_state: AtomicU8,
    current_ctrl_state: AtomicU8,
    pool_stats: Mutex<BufferPoolStats>,
}

/// Thread-safe runtime control and telemetry handle for a [`SimSignalTransport`].
#[derive(Clone, Debug)]
pub struct SimControlHandle {
    inner: Arc<SimControlInner>,
}

impl SimControlHandle {
    fn new(initial_rate_hz: f64) -> Self {
        Self {
            inner: Arc::new(SimControlInner {
                sample_rate_bits: AtomicU64::new(initial_rate_hz.to_bits()),
                rate_epoch: AtomicU64::new(0),
                pending_manual_glitches: AtomicU32::new(0),
                emitted_samples: AtomicU64::new(0),
                injected_glitches: AtomicU64::new(0),
                forced_ctrl_state: AtomicU8::new(STATE_UNFORCED),
                current_ctrl_state: AtomicU8::new(CTRL_STATE_IDLE),
                pool_stats: Mutex::new(BufferPoolStats::default()),
            }),
        }
    }

    /// Queues `count` single-sample narrow-pulse over-voltage glitches.
    ///
    /// Multiple queued glitches are emitted on separated samples so each glitch pulse
    /// remains strictly 1 sample wide.
    pub fn inject_manual_glitch(&self, count: u32) {
        if count > 0 {
            self.inner
                .pending_manual_glitches
                .fetch_add(count, Ordering::AcqRel);
        }
    }

    /// Dynamically updates the simulation sample rate (validated in `1 kHz..=10 MHz`).
    pub fn set_sample_rate_hz(&self, sample_rate_hz: f64) -> CoreResult<()> {
        validate_sample_rate_hz(sample_rate_hz)?;
        self.inner
            .sample_rate_bits
            .store(sample_rate_hz.to_bits(), Ordering::Release);
        self.inner.rate_epoch.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    /// Returns the currently configured sample rate in Hz.
    #[inline]
    pub fn sample_rate_hz(&self) -> f64 {
        f64::from_bits(self.inner.sample_rate_bits.load(Ordering::Acquire))
    }

    /// Returns the total number of samples emitted by the transport so far.
    #[inline]
    pub fn emitted_samples(&self) -> u64 {
        self.inner.emitted_samples.load(Ordering::Acquire)
    }

    /// Returns the total number of glitches (periodic + manual) injected so far.
    #[inline]
    pub fn injected_glitches(&self) -> u64 {
        self.inner.injected_glitches.load(Ordering::Acquire)
    }

    /// Forces `ctrl_state` to a specific code (`0..=3`) until cleared via [`Self::clear_forced_ctrl_state`].
    pub fn set_ctrl_state(&self, code: u8) -> CoreResult<()> {
        if code > CTRL_STATE_OCP_FAULT {
            return Err(CoreError::InvalidConfig(format!(
                "ctrl_state must be in 0..=3, got {code}"
            )));
        }
        self.inner.forced_ctrl_state.store(code, Ordering::Release);
        self.inner.current_ctrl_state.store(code, Ordering::Release);
        Ok(())
    }

    /// Resumes automatic `0:IDLE -> 1:ALIGN -> 2:CLOSED_LOOP -> 3:OCP_FAULT` cycling.
    pub fn clear_forced_ctrl_state(&self) {
        self.inner
            .forced_ctrl_state
            .store(STATE_UNFORCED, Ordering::Release);
    }

    /// Returns the most recently generated `ctrl_state` code (`0..=3`).
    #[inline]
    pub fn ctrl_state(&self) -> u8 {
        self.inner.current_ctrl_state.load(Ordering::Acquire)
    }

    /// Returns the latest [`BufferPoolStats`] snapshot recorded by the IO task.
    pub fn pool_stats(&self) -> BufferPoolStats {
        *self.inner.pool_stats.lock()
    }

    #[inline]
    fn rate_epoch(&self) -> u64 {
        self.inner.rate_epoch.load(Ordering::Acquire)
    }

    #[inline]
    fn try_consume_manual_glitch(&self) -> bool {
        let mut cur = self.inner.pending_manual_glitches.load(Ordering::Acquire);
        while cur > 0 {
            match self.inner.pending_manual_glitches.compare_exchange_weak(
                cur,
                cur - 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(actual) => cur = actual,
            }
        }
        false
    }

    #[inline]
    fn add_emitted_samples(&self, count: u64) {
        self.inner
            .emitted_samples
            .fetch_add(count, Ordering::Relaxed);
    }

    #[inline]
    fn add_injected_glitches(&self, count: u64) {
        if count > 0 {
            self.inner
                .injected_glitches
                .fetch_add(count, Ordering::Relaxed);
        }
    }

    #[inline]
    fn forced_ctrl_state(&self) -> Option<u8> {
        let raw = self.inner.forced_ctrl_state.load(Ordering::Acquire);
        if raw <= CTRL_STATE_OCP_FAULT {
            Some(raw)
        } else {
            None
        }
    }

    #[inline]
    fn store_current_ctrl_state(&self, state: u8) {
        self.inner
            .current_ctrl_state
            .store(state, Ordering::Release);
    }

    #[inline]
    fn store_pool_stats(&self, stats: BufferPoolStats) {
        *self.inner.pool_stats.lock() = stats;
    }
}

/// Built-in deterministic mixed-signal simulation [`Transport`] (`FR-IO-01`).
#[derive(Debug)]
pub struct SimSignalTransport {
    config: SimConfig,
    control: SimControlHandle,
}

impl SimSignalTransport {
    /// Validates `config` and constructs a new [`SimSignalTransport`].
    pub fn new(config: SimConfig) -> CoreResult<Self> {
        config.validate()?;
        let control = SimControlHandle::new(config.sample_rate_hz);
        Ok(Self { config, control })
    }

    /// Returns the immutable configuration used to construct this transport.
    #[inline]
    pub fn config(&self) -> &SimConfig {
        &self.config
    }

    /// Returns a cloneable [`SimControlHandle`] for live glitch/rate/state control and telemetry.
    #[inline]
    pub fn control_handle(&self) -> SimControlHandle {
        self.control.clone()
    }
}

impl Transport for SimSignalTransport {
    fn capabilities(&self) -> TransportCaps {
        TransportCaps {
            kind: TransportKind::Sim,
            supports_tx: true,
            supports_seek: false,
            supports_raw_record: true,
            supports_auto_reconnect: false,
        }
    }

    fn start(
        &mut self,
        rx_sink: PacketSender,
        tx_rx: OutboundReceiver,
        shutdown_rx: ShutdownReceiver,
    ) -> CoreResult<JoinHandle<CoreResult<()>>> {
        self.config.validate()?;
        let config = self.config.clone();
        let control = self.control.clone();
        Ok(tokio::spawn(async move {
            run_sim_loop(config, control, rx_sink, tx_rx, shutdown_rx).await
        }))
    }
}

/// Factory function instantiating a boxed [`SimSignalTransport`] from JSON config.
///
/// Matches [`crate::plugin::TransportFactoryFn`].
pub fn create_sim_transport(json_config: &str) -> CoreResult<Box<dyn Transport>> {
    let config: SimConfig = serde_json::from_str(json_config)
        .map_err(|err| CoreError::InvalidConfig(format!("invalid SimConfig JSON: {err}")))?;
    let transport = SimSignalTransport::new(config)?;
    Ok(Box::new(transport))
}

/// Fast deterministic SplitMix64 + Xoshiro256++ generator for zero-alloc white noise.
#[derive(Clone, Debug)]
struct FastNoiseRng {
    s: [u64; 4],
}

impl FastNoiseRng {
    #[inline]
    fn splitmix64(state: &mut u64) -> u64 {
        *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn from_seed(seed: u64) -> Self {
        let mut sm = seed;
        Self {
            s: [
                Self::splitmix64(&mut sm),
                Self::splitmix64(&mut sm),
                Self::splitmix64(&mut sm),
                Self::splitmix64(&mut sm),
            ],
        }
    }

    #[inline(always)]
    fn next_u64(&mut self) -> u64 {
        let result = (self.s[0].wrapping_add(self.s[3]))
            .rotate_left(23)
            .wrapping_add(self.s[0]);
        let t = self.s[1] << 17;
        self.s[2] ^= self.s[0];
        self.s[3] ^= self.s[1];
        self.s[1] ^= self.s[2];
        self.s[0] ^= self.s[3];
        self.s[2] ^= t;
        self.s[3] = self.s[3].rotate_left(45);
        result
    }

    /// Returns a uniform sample in `[-1.0, 1.0)`.
    #[inline(always)]
    fn next_bipolar_f32(&mut self) -> f32 {
        const INV_HALF_24: f32 = 1.0 / 8_388_608.0;
        ((self.next_u64() >> 40) as f32) * INV_HALF_24 - 1.0
    }
}

/// Fast branch-minimal minimax polynomial approximation of `sin(2 * pi * turns)` (~3e-6 max err).
#[inline(always)]
fn fast_sin_turns(turns: f32) -> f32 {
    let frac = turns - turns.floor();
    let u = frac * 4.0;
    let y = if u < 1.0 {
        u
    } else if u < 3.0 {
        2.0 - u
    } else {
        u - 4.0
    };
    let y2 = y * y;
    y * (1.570_794_8 + y2 * (-0.645_921 + y2 * (0.079_487_66 - 0.004_362_476 * y2)))
}

#[inline(always)]
fn triangle_turns(turns: f32) -> f32 {
    let frac = turns - turns.floor();
    let u = frac * 4.0;
    if u < 1.0 {
        u
    } else if u < 3.0 {
        2.0 - u
    } else {
        u - 4.0
    }
}

#[inline(always)]
fn square_turns(turns: f32) -> f32 {
    let frac = turns - turns.floor();
    if frac < 0.5 {
        1.0
    } else {
        -1.0
    }
}

struct SampleSynthesizer {
    config: SimConfig,
    control: SimControlHandle,
    rng: FastNoiseRng,
    sample_idx: u64,
    phase_turns: f64,
    prev_sample_was_glitch: bool,
    glitch_channel: usize,
    channels_buf: [f32; MAX_SIM_ANALOG_CHANNELS as usize],
    scratch_frame: [u8; 12 + 4 * MAX_SIM_ANALOG_CHANNELS as usize],
    last_ctrl_state: u8,
    pending_glitches_batch: u64,
}

impl SampleSynthesizer {
    fn new(config: SimConfig, control: SimControlHandle) -> Self {
        let a = config.analog_channels as usize;
        let glitch_channel = if a >= 4 { 3 } else { a - 1 };
        let rng = FastNoiseRng::from_seed(config.seed);
        Self {
            config,
            control,
            rng,
            sample_idx: 0,
            phase_turns: 0.0,
            prev_sample_was_glitch: false,
            glitch_channel,
            channels_buf: [0.0; MAX_SIM_ANALOG_CHANNELS as usize],
            scratch_frame: [0; 12 + 4 * MAX_SIM_ANALOG_CHANNELS as usize],
            last_ctrl_state: CTRL_STATE_IDLE,
            pending_glitches_batch: 0,
        }
    }

    #[inline(always)]
    fn waveform_for(&self, ch: usize) -> SimWaveformKind {
        self.config
            .channel_waveforms
            .get(ch)
            .copied()
            .unwrap_or(SimWaveformKind::Motor3PhasePreset)
    }

    #[cfg(test)]
    #[inline(always)]
    fn synthesize_sample(&mut self, sample_rate_hz: f64) -> (u32, u8, u8) {
        self.synthesize_sample_with_manual(sample_rate_hz, true)
    }

    #[inline(always)]
    fn synthesize_sample_with_manual(
        &mut self,
        sample_rate_hz: f64,
        allow_manual_glitch: bool,
    ) -> (u32, u8, u8) {
        let idx = self.sample_idx;
        self.sample_idx = idx.wrapping_add(1);

        let f_sig = (sample_rate_hz / 200.0).max(50.0);
        let phase_inc = f_sig / sample_rate_hz;
        let p = self.phase_turns as f32;
        self.phase_turns += phase_inc;
        if self.phase_turns >= 1.0 {
            self.phase_turns -= self.phase_turns.floor();
        }

        let ctrl_state = match self.control.forced_ctrl_state() {
            Some(forced) => forced,
            None => ((idx / DEFAULT_STATE_DWELL_SAMPLES) % 4) as u8,
        };
        self.last_ctrl_state = ctrl_state;

        let periodic_glitch = self.config.enable_glitch
            && self.config.glitch_period_samples > 0
            && (idx + 1) % (self.config.glitch_period_samples as u64) == 0;

        let manual_glitch = allow_manual_glitch
            && !periodic_glitch
            && !self.prev_sample_was_glitch
            && self.control.try_consume_manual_glitch();

        let glitch_active = periodic_glitch || manual_glitch;
        self.prev_sample_was_glitch = glitch_active;
        if glitch_active {
            self.pending_glitches_batch += 1;
        }

        let (pwm_a, pwm_b) = match ctrl_state {
            CTRL_STATE_ALIGN => (true, false),
            CTRL_STATE_CLOSED_LOOP => {
                let high = (idx / 8) % 2 == 0;
                (high, !high)
            }
            _ => (false, false),
        };
        let fault_pin = glitch_active || ctrl_state == CTRL_STATE_OCP_FAULT;

        let mut flags_bits = 0u8;
        if pwm_a {
            flags_bits |= SIM_FLAG_PWM_A;
        }
        if pwm_b {
            flags_bits |= SIM_FLAG_PWM_B;
        }
        if fault_pin {
            flags_bits |= SIM_FLAG_FAULT_PIN;
        }
        if glitch_active {
            flags_bits |= SIM_FLAG_GLITCH_ACTIVE;
        }

        let a = self.config.analog_channels as usize;
        let noise_amp = self.config.noise_amplitude;

        for ch in 0..a {
            let noise = self.rng.next_bipolar_f32();
            let mut val = match self.waveform_for(ch) {
                SimWaveformKind::Motor3PhasePreset => {
                    if ch == self.glitch_channel {
                        24.0 + noise_amp * noise
                    } else if ch < 3 {
                        let phase_shift = match ch {
                            0 => p,
                            1 => p + (2.0 / 3.0),
                            _ => p + (1.0 / 3.0),
                        };
                        10.0 * fast_sin_turns(phase_shift)
                            + 1.2 * fast_sin_turns(phase_shift * 5.0)
                            + 0.6 * fast_sin_turns(phase_shift * 7.0)
                            + noise_amp * noise
                    } else {
                        5.0 * fast_sin_turns(p + (ch as f32) * 0.125) + noise_amp * noise
                    }
                }
                SimWaveformKind::Sine => {
                    10.0 * fast_sin_turns(p + (ch as f32) * 0.125) + noise_amp * noise
                }
                SimWaveformKind::Square => {
                    5.0 * square_turns(p + (ch as f32) * 0.125) + noise_amp * noise
                }
                SimWaveformKind::Triangle => {
                    5.0 * triangle_turns(p + (ch as f32) * 0.125) + noise_amp * noise
                }
                SimWaveformKind::WhiteNoise => {
                    let amp = if noise_amp > 0.0 { noise_amp } else { 1.0 };
                    amp * noise
                }
            };

            if glitch_active && ch == self.glitch_channel {
                val += self.config.glitch_amplitude;
            }
            self.channels_buf[ch] = val;
        }

        (idx as u32, flags_bits, ctrl_state)
    }

    #[inline(always)]
    fn encode_sim_preset_into(&mut self, buf: &mut BytesMut, hw_tick: u32, flags_bits: u8, ctrl_state: u8) {
        let a = self.config.analog_channels as usize;
        let frame_len = 12 + 4 * a;
        let dst = &mut self.scratch_frame[..frame_len];
        dst[0] = SIM_PRESET_MAGIC[0];
        dst[1] = SIM_PRESET_MAGIC[1];
        dst[2] = a as u8;
        dst[3] = flags_bits;
        dst[4..8].copy_from_slice(&hw_tick.to_le_bytes());
        let mut offset = 8;
        for &val in &self.channels_buf[..a] {
            dst[offset..offset + 4].copy_from_slice(&val.to_le_bytes());
            offset += 4;
        }
        dst[offset] = ctrl_state;
        dst[offset + 1] = 0;
        dst[offset + 2] = SIM_PRESET_TAIL[0];
        dst[offset + 3] = SIM_PRESET_TAIL[1];
        buf.extend_from_slice(dst);
    }

    #[inline(always)]
    fn encode_just_float_into(&mut self, buf: &mut BytesMut) {
        let a = self.config.analog_channels as usize;
        let frame_len = 4 * a + 4;
        let dst = &mut self.scratch_frame[..frame_len];
        let mut offset = 0;
        for &val in &self.channels_buf[..a] {
            dst[offset..offset + 4].copy_from_slice(&val.to_le_bytes());
            offset += 4;
        }
        dst[offset..offset + 4].copy_from_slice(&JUST_FLOAT_TAIL);
        buf.extend_from_slice(dst);
    }

    fn encode_firewater_line_into(&self, buf: &mut BytesMut) {
        let a = self.config.analog_channels as usize;
        for (i, &val) in self.channels_buf[..a].iter().enumerate() {
            if i > 0 {
                buf.put_u8(b',');
            }
            write_f32_compact(buf, val);
        }
        buf.put_u8(b'\n');
    }

    fn emit_batch(
        &mut self,
        emitter: &mut RxPacketEmitter,
        batch_samples: usize,
        sample_rate_hz: f64,
    ) -> CoreResult<(usize, bool)> {
        if batch_samples == 0 {
            return Ok((0, true));
        }

        let slab_bytes = emitter.pool().slab_bytes();
        self.pending_glitches_batch = 0;
        let mut actual_samples = 0usize;

        match self.config.wire_format {
            SimWireFormat::SimPreset => {
                let frame_len = self.config.frame_byte_len();
                let max_fit = (slab_bytes / frame_len).max(1);
                let count = batch_samples.min(max_fit);
                let glitch_min_idx = if count >= 4 { 2 } else { 0 };
                let buf = emitter.pool_mut().acquire_buf(count * frame_len);
                for batch_idx in 0..count {
                    let (hw_tick, flags_bits, ctrl_state) =
                        self.synthesize_sample_with_manual(sample_rate_hz, batch_idx >= glitch_min_idx);
                    self.encode_sim_preset_into(buf, hw_tick, flags_bits, ctrl_state);
                }
                actual_samples = count;
            }
            SimWireFormat::JustFloat => {
                let frame_len = self.config.frame_byte_len();
                let max_fit = (slab_bytes / frame_len).max(1);
                let count = batch_samples.min(max_fit);
                let glitch_min_idx = if count >= 4 { 2 } else { 0 };
                let buf = emitter.pool_mut().acquire_buf(count * frame_len);
                for batch_idx in 0..count {
                    let _ = self
                        .synthesize_sample_with_manual(sample_rate_hz, batch_idx >= glitch_min_idx);
                    self.encode_just_float_into(buf);
                }
                actual_samples = count;
            }
            SimWireFormat::FireWaterCsv => {
                let max_line_bytes = self.config.frame_byte_len();
                let max_fit_lines = (slab_bytes / max_line_bytes).max(1);
                let glitch_min_idx = if batch_samples.min(max_fit_lines) >= 4 {
                    2
                } else {
                    0
                };
                let buf = emitter.pool_mut().acquire_buf(slab_bytes);
                for batch_idx in 0..batch_samples {
                    if slab_bytes.saturating_sub(buf.len()) < max_line_bytes {
                        break;
                    }
                    let _ = self
                        .synthesize_sample_with_manual(sample_rate_hz, batch_idx >= glitch_min_idx);
                    self.encode_firewater_line_into(buf);
                    actual_samples += 1;
                }
            }
        }

        self.control.store_current_ctrl_state(self.last_ctrl_state);
        self.control
            .add_injected_glitches(self.pending_glitches_batch);
        self.control.add_emitted_samples(actual_samples as u64);

        let delivered = match emitter
            .pool_mut()
            .freeze_packet(host_monotonic_ns(), None)
        {
            Some(pkt) => emitter.emit_packet(pkt)?,
            None => true,
        };
        self.control.store_pool_stats(emitter.pool_stats());
        Ok((actual_samples, delivered))
    }
}

#[inline]
fn write_f32_compact(buf: &mut BytesMut, val: f32) {
    if val.is_finite() && val.abs() < 1_000_000.0 {
        let neg = val.is_sign_negative() && val != 0.0;
        let scaled = ((val.abs() as f64) * 100_000.0 + 0.5) as u64;
        if neg && scaled > 0 {
            buf.put_u8(b'-');
        }
        let int_part = scaled / 100_000;
        let frac_part = (scaled % 100_000) as u32;

        let mut int_digits = [0u8; 16];
        let mut len = 0usize;
        let mut n = int_part;
        loop {
            int_digits[len] = b'0' + (n % 10) as u8;
            len += 1;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        for i in (0..len).rev() {
            buf.put_u8(int_digits[i]);
        }

        buf.put_u8(b'.');
        let d0 = (frac_part / 10_000) as u8;
        let d1 = ((frac_part / 1_000) % 10) as u8;
        let d2 = ((frac_part / 100) % 10) as u8;
        let d3 = ((frac_part / 10) % 10) as u8;
        let d4 = (frac_part % 10) as u8;
        buf.extend_from_slice(&[b'0' + d0, b'0' + d1, b'0' + d2, b'0' + d3, b'0' + d4]);
    } else {
        let _ = write!(buf, "{val:.5}");
    }
}

fn handle_tx_frame(frame: OutboundFrame, control: &SimControlHandle) {
    let Ok(text) = std::str::from_utf8(&frame.payload) else {
        warn!(
            len = frame.payload.len(),
            "sim transport ignored non-UTF8 TX command"
        );
        return;
    };

    for raw_line in text.split(['\n', '\r', ';']) {
        let cmd = raw_line.trim();
        if cmd.is_empty() {
            continue;
        }
        let upper = cmd.to_ascii_uppercase();
        if upper == "GLITCH" {
            control.inject_manual_glitch(1);
        } else if let Some(rest) = upper.strip_prefix("GLITCH=") {
            match rest.trim().parse::<u32>() {
                Ok(count) => control.inject_manual_glitch(count),
                Err(_) => warn!(cmd, "invalid GLITCH count command"),
            }
        } else if let Some(rest) = upper.strip_prefix("RATE=") {
            match rest.trim().parse::<f64>() {
                Ok(hz) => {
                    if let Err(err) = control.set_sample_rate_hz(hz) {
                        warn!(cmd, %err, "rejected RATE command");
                    }
                }
                Err(_) => warn!(cmd, "invalid RATE value command"),
            }
        } else if let Some(rest) = upper.strip_prefix("STATE=") {
            let arg = rest.trim();
            match arg {
                "AUTO" | "CYCLE" => control.clear_forced_ctrl_state(),
                "0" | "IDLE" => {
                    let _ = control.set_ctrl_state(CTRL_STATE_IDLE);
                }
                "1" | "ALIGN" => {
                    let _ = control.set_ctrl_state(CTRL_STATE_ALIGN);
                }
                "2" | "CLOSED_LOOP" | "RUN" => {
                    let _ = control.set_ctrl_state(CTRL_STATE_CLOSED_LOOP);
                }
                "3" | "OCP_FAULT" | "FAULT" => {
                    let _ = control.set_ctrl_state(CTRL_STATE_OCP_FAULT);
                }
                _ => warn!(cmd, "unknown STATE code command"),
            }
        } else {
            debug!(cmd, "sim transport ignored unrecognized TX command");
        }
    }
}

fn drain_tx_nonblocking(
    tx_rx: &mut OutboundReceiver,
    tx_closed: &mut bool,
    control: &SimControlHandle,
) {
    if *tx_closed {
        return;
    }
    loop {
        match tx_rx.try_recv() {
            Ok(frame) => handle_tx_frame(frame, control),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                *tx_closed = true;
                break;
            }
        }
    }
}

fn target_batch_samples(sample_rate_hz: f64, max_batch_samples: usize) -> usize {
    // Target ~2 ms cadence for smooth pacing, bounded to one 64 KiB slab.
    let target = (sample_rate_hz * 0.002).round() as usize;
    target.clamp(1, max_batch_samples.max(1))
}

async fn wait_until_deadline(
    deadline: Instant,
    shutdown_rx: &mut ShutdownReceiver,
    tx_rx: &mut OutboundReceiver,
    tx_closed: &mut bool,
    control: &SimControlHandle,
) -> bool {
    loop {
        if *shutdown_rx.borrow() {
            return false;
        }
        drain_tx_nonblocking(tx_rx, tx_closed, control);

        let now = Instant::now();
        if now >= deadline {
            return true;
        }
        let remaining = deadline - now;

        if remaining > Duration::from_micros(HYBRID_SLEEP_THRESHOLD_US) {
            let sleep_dur = remaining - Duration::from_micros(HYBRID_SLEEP_THRESHOLD_US);
            let sleep = tokio::time::sleep(sleep_dur);
            tokio::pin!(sleep);

            tokio::select! {
                biased;
                res = shutdown_rx.changed() => {
                    if res.is_err() || *shutdown_rx.borrow() {
                        return false;
                    }
                }
                maybe_tx = tx_rx.recv(), if !*tx_closed => {
                    match maybe_tx {
                        Some(frame) => handle_tx_frame(frame, control),
                        None => *tx_closed = true,
                    }
                }
                _ = &mut sleep => {}
            }
        } else if remaining > Duration::from_micros(SPIN_THRESHOLD_US) {
            tokio::task::yield_now().await;
        } else {
            while Instant::now() < deadline {
                if *shutdown_rx.borrow() {
                    return false;
                }
                std::hint::spin_loop();
            }
            return true;
        }
    }
}

async fn run_sim_loop(
    config: SimConfig,
    control: SimControlHandle,
    rx_sink: PacketSender,
    mut tx_rx: OutboundReceiver,
    mut shutdown_rx: ShutdownReceiver,
) -> CoreResult<()> {
    let mut emitter = RxPacketEmitter::new(
        rx_sink,
        BufferPoolConfig::default(),
        config.rx_backpressure,
    );
    let max_samples = config.max_samples;
    let paced = config.paced;
    let slab_bytes = emitter.pool().slab_bytes();
    let max_batch_samples = (slab_bytes / config.frame_byte_len()).max(1);

    let mut synth = SampleSynthesizer::new(config, control.clone());
    let mut tx_closed = false;
    let mut total_emitted: u64 = 0;

    let mut observed_rate_epoch = control.rate_epoch();
    let mut current_rate_hz = control.sample_rate_hz();
    let mut pacer_anchor = Instant::now();
    let mut samples_since_anchor: u64 = 0;
    let mut unpaced_packets: u64 = 0;

    loop {
        if *shutdown_rx.borrow() {
            break;
        }
        drain_tx_nonblocking(&mut tx_rx, &mut tx_closed, &control);

        let cur_epoch = control.rate_epoch();
        if cur_epoch != observed_rate_epoch {
            observed_rate_epoch = cur_epoch;
            current_rate_hz = control.sample_rate_hz();
            pacer_anchor = Instant::now();
            samples_since_anchor = 0;
        }

        let remaining_quota = max_samples.map(|limit| limit.saturating_sub(total_emitted));
        if remaining_quota == Some(0) {
            break;
        }

        let desired_batch = if paced {
            target_batch_samples(current_rate_hz, max_batch_samples)
        } else {
            max_batch_samples
        };
        let batch_samples = match remaining_quota {
            Some(rem) => desired_batch.min(rem as usize),
            None => desired_batch,
        };

        if paced && samples_since_anchor > 0 {
            let scheduled_ns = (samples_since_anchor as f64) * (1_000_000_000.0 / current_rate_hz);
            let deadline = pacer_anchor + Duration::from_nanos(scheduled_ns.round() as u64);
            let now = Instant::now();
            if now > deadline {
                // Clamp accumulated debt so long scheduler stalls do not trigger unbounded bursts.
                let debt_ns = (now - deadline).as_nanos() as f64;
                if debt_ns > MAX_PACER_DEBT_NS {
                    let clamp_shift_ns = debt_ns - MAX_PACER_DEBT_NS;
                    pacer_anchor += Duration::from_nanos(clamp_shift_ns.round() as u64);
                }
            } else if !wait_until_deadline(
                deadline,
                &mut shutdown_rx,
                &mut tx_rx,
                &mut tx_closed,
                &control,
            )
            .await
            {
                break;
            }
        }

        match synth.emit_batch(&mut emitter, batch_samples, current_rate_hz) {
            Ok((emitted, _delivered)) => {
                total_emitted += emitted as u64;
                samples_since_anchor += emitted as u64;
            }
            Err(CoreError::ChannelClosed) => {
                debug!("sim RX sink closed; stopping SimSignalTransport");
                break;
            }
            Err(err) => return Err(err),
        }

        if !paced {
            unpaced_packets += 1;
            if unpaced_packets % UNPACED_YIELD_INTERVAL_PACKETS == 0 {
                tokio::task::yield_now().await;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_deserializes_minimal_dto_json_and_validates_bounds() {
        let json = r#"{
            "sample_rate_hz": 1000000.0,
            "analog_channels": 4,
            "enable_glitch": true,
            "glitch_period_samples": 100000
        }"#;
        let cfg: SimConfig = serde_json::from_str(json).expect("DTO JSON must deserialize");
        assert_eq!(cfg.sample_rate_hz, 1_000_000.0);
        assert_eq!(cfg.analog_channels, 4);
        assert!(cfg.enable_glitch);
        assert_eq!(cfg.glitch_period_samples, 100_000);
        assert_eq!(cfg.wire_format, SimWireFormat::SimPreset);
        assert_eq!(cfg.glitch_amplitude, 80.0);
        assert_eq!(cfg.noise_amplitude, 0.05);
        assert!(cfg.paced);
        cfg.validate().expect("valid default config");

        let mut bad_rate = cfg.clone();
        bad_rate.sample_rate_hz = 999.0;
        assert!(matches!(bad_rate.validate(), Err(CoreError::InvalidConfig(_))));

        bad_rate.sample_rate_hz = 10_000_001.0;
        assert!(matches!(bad_rate.validate(), Err(CoreError::InvalidConfig(_))));

        let mut bad_ch = cfg;
        bad_ch.analog_channels = 0;
        assert!(matches!(bad_ch.validate(), Err(CoreError::InvalidConfig(_))));

        bad_ch.analog_channels = 17;
        assert!(matches!(bad_ch.validate(), Err(CoreError::InvalidConfig(_))));
    }

    #[test]
    fn fast_sin_and_rng_are_deterministic_and_bounded() {
        for i in 0..1000 {
            let t = i as f32 / 1000.0;
            let exact = (2.0 * std::f32::consts::PI * t).sin();
            let approx = fast_sin_turns(t);
            assert!(
                (exact - approx).abs() < 2e-5,
                "t={t} exact={exact} approx={approx}"
            );
        }

        let mut r1 = FastNoiseRng::from_seed(42);
        let mut r2 = FastNoiseRng::from_seed(42);
        for _ in 0..256 {
            let v1 = r1.next_bipolar_f32();
            let v2 = r2.next_bipolar_f32();
            assert_eq!(v1, v2);
            assert!((-1.0..1.0).contains(&v1));
        }
    }

    #[test]
    fn sim_preset_frame_layout_is_exact_byte_for_byte() {
        let cfg = SimConfig {
            sample_rate_hz: 100_000.0,
            analog_channels: 4,
            enable_glitch: true,
            glitch_period_samples: 5,
            wire_format: SimWireFormat::SimPreset,
            noise_amplitude: 0.0,
            ..Default::default()
        };
        let control = SimControlHandle::new(cfg.sample_rate_hz);
        let mut synth = SampleSynthesizer::new(cfg, control);
        let mut buf = BytesMut::with_capacity(280);
        for _ in 0..5 {
            let (hw_tick, flags, state) = synth.synthesize_sample(100_000.0);
            synth.encode_sim_preset_into(&mut buf, hw_tick, flags, state);
        }
        assert_eq!(buf.len(), 5 * 28);

        // Sample index 4 (5th sample) must have the periodic glitch on ch3.
        let frame4 = &buf[4 * 28..5 * 28];
        assert_eq!(&frame4[0..2], &SIM_PRESET_MAGIC);
        assert_eq!(frame4[2], 4);
        assert_ne!(frame4[3] & SIM_FLAG_GLITCH_ACTIVE, 0);
        assert_ne!(frame4[3] & SIM_FLAG_FAULT_PIN, 0);
        assert_eq!(u32::from_le_bytes(frame4[4..8].try_into().unwrap()), 4);
        let ch3 = f32::from_le_bytes(frame4[20..24].try_into().unwrap());
        assert!((ch3 - 104.0).abs() < 1e-4, "ch3={ch3}");
        assert_eq!(frame4[24], CTRL_STATE_IDLE);
        assert_eq!(frame4[25], 0);
        assert_eq!(&frame4[26..28], &SIM_PRESET_TAIL);

        // Sample index 3 (before glitch) must NOT have glitch or fault in IDLE.
        let frame3 = &buf[3 * 28..4 * 28];
        assert_eq!(frame3[3] & SIM_FLAG_GLITCH_ACTIVE, 0);
        assert_eq!(frame3[3] & SIM_FLAG_FAULT_PIN, 0);
        let ch3_prev = f32::from_le_bytes(frame3[20..24].try_into().unwrap());
        assert!((ch3_prev - 24.0).abs() < 1e-4);
    }
}
