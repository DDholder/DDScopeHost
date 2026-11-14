//! Live `CoreWorkspace` bootstrap and background frame/telemetry runtime loops (`T06`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ddscope_core::clock::ClockMode;
use ddscope_core::protocol::SimPresetConfig;
use ddscope_core::transport::{host_monotonic_ns, SimConfig};
use ddscope_core::types::{CoreResult, SourceId};
use ddscope_core::{CoreWorkspace, SourceConfig, SourceProtocolConfig, SourceTransportConfig};
use tauri_specta::Event as _;
use tokio::sync::watch;
use tokio::time::{Instant as TokioInstant, MissedTickBehavior};
use tracing::{debug, warn};

use crate::commands::{convert, DesktopBridgeState, WorkspaceTelemetryEvent};
use crate::data_bridge::{MAX_DATA_BRIDGE_FPS, MIN_DATA_BRIDGE_FPS};

/// Default desktop global RAM ring-buffer quota (`2 GiB`, `FR-ST-03`).
pub const DEFAULT_DESKTOP_QUOTA_BYTES: usize = 2 * 1024 * 1024 * 1024;

/// Telemetry emission cadence (`125 ms` = `8 Hz`, within the 5–10 Hz target).
pub const DEFAULT_TELEMETRY_INTERVAL_MS: u64 = 125;

const MIN_CONFIG_QUOTA_MIB: usize = 16;
const MAX_CONFIG_QUOTA_MIB: usize = 16_384;
const DEFAULT_SIM_SOURCE_ID: &str = "sim0";

/// Startup configuration for [`build_live_desktop_state`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DesktopRuntimeConfig {
    /// Global chunk ring-buffer quota in bytes.
    pub quota_bytes: usize,
    /// Whether to seed the built-in `sim0` (`2 MHz`, 4 analog + digital + state) demo source.
    pub seed_default_sim_source: bool,
}

impl Default for DesktopRuntimeConfig {
    fn default() -> Self {
        Self {
            quota_bytes: DEFAULT_DESKTOP_QUOTA_BYTES,
            seed_default_sim_source: true,
        }
    }
}

impl DesktopRuntimeConfig {
    /// Reads runtime configuration overrides from environment variables (`DDSCOPE_QUOTA_MIB`,
    /// `DDSCOPE_NO_DEFAULT_SOURCE`).
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        if let Ok(raw_mib) = std::env::var("DDSCOPE_QUOTA_MIB") {
            if let Ok(mib) = raw_mib.trim().parse::<usize>() {
                let clamped = mib.clamp(MIN_CONFIG_QUOTA_MIB, MAX_CONFIG_QUOTA_MIB);
                cfg.quota_bytes = clamped * 1024 * 1024;
            }
        }
        if let Ok(raw_no_sim) = std::env::var("DDSCOPE_NO_DEFAULT_SOURCE") {
            let trimmed = raw_no_sim.trim();
            if trimmed == "1" || trimmed.eq_ignore_ascii_case("true") {
                cfg.seed_default_sim_source = false;
            }
        }
        cfg
    }
}

/// Instantiates a live [`CoreWorkspace`] (optionally seeding `sim0`) wrapped in [`DesktopBridgeState`].
pub fn build_live_desktop_state(
    tokio_handle: tokio::runtime::Handle,
    cfg: DesktopRuntimeConfig,
) -> CoreResult<DesktopBridgeState> {
    build_live_desktop_state_with_auto_start(tokio_handle, cfg, true)
}

/// Instantiates a live [`CoreWorkspace`] with explicit control over whether the seeded `sim0`
/// demo source starts acquisition immediately.
pub fn build_live_desktop_state_with_auto_start(
    tokio_handle: tokio::runtime::Handle,
    cfg: DesktopRuntimeConfig,
    auto_start_default_sim_source: bool,
) -> CoreResult<DesktopBridgeState> {
    let ws = Arc::new(CoreWorkspace::with_quota_bytes(tokio_handle, cfg.quota_bytes));

    if cfg.seed_default_sim_source {
        let sim_source_cfg = SourceConfig::new(
            SourceId::new(DEFAULT_SIM_SOURCE_ID),
            SourceTransportConfig::Sim(SimConfig {
                sample_rate_hz: 2_000_000.0,
                analog_channels: 4,
                enable_glitch: true,
                glitch_period_samples: 4_000_000,
                glitch_amplitude: 80.0,
                paced: true,
                ..SimConfig::default()
            }),
            ClockMode::Tier1HardwareCounter {
                tick_hz: 2_000_000.0,
                counter_bits: 32,
            },
            SourceProtocolConfig::SimPreset(SimPresetConfig {
                source_id: DEFAULT_SIM_SOURCE_ID.to_string(),
                analog_channels: 4,
                ..SimPresetConfig::default()
            }),
        )
        .with_display_name("Simulated FOC Motor (2 MHz)")
        .with_auto_start(auto_start_default_sim_source);

        ws.create_source(sim_source_cfg)?;
    }

    Ok(DesktopBridgeState::with_workspace(ws))
}

/// Manages background 60/120Hz `DDSF v1` frame emission, 8Hz telemetry events, and clean shutdown.
#[derive(Debug)]
pub struct DesktopRuntimeController {
    state: DesktopBridgeState,
    shutdown_tx: watch::Sender<bool>,
    stopped: AtomicBool,
}

impl DesktopRuntimeController {
    /// Spawns background zero-copy frame and telemetry loops on `tauri::async_runtime`.
    pub fn spawn(app_handle: tauri::AppHandle, state: DesktopBridgeState) -> Arc<Self> {
        let (shutdown_tx, shutdown_rx_frame) = watch::channel(false);
        let shutdown_rx_telemetry = shutdown_tx.subscribe();

        let ctrl = Arc::new(Self {
            state,
            shutdown_tx,
            stopped: AtomicBool::new(false),
        });

        let frame_state = ctrl.state.clone();
        tauri::async_runtime::spawn(async move {
            run_ddsf_frame_loop(frame_state, shutdown_rx_frame).await;
        });

        let telemetry_state = ctrl.state.clone();
        tauri::async_runtime::spawn(async move {
            run_telemetry_loop(app_handle, telemetry_state, shutdown_rx_telemetry).await;
        });

        ctrl
    }

    /// Signals background loops to exit, detaches the zero-copy bridge, and stops active sources.
    pub fn shutdown_blocking(&self) {
        if self.stopped.swap(true, Ordering::AcqRel) {
            return;
        }
        let _ = self.shutdown_tx.send(true);
        self.state.data_bridge().detach();

        if let Some(ws) = self.state.workspace() {
            if tokio::runtime::Handle::try_current().is_ok() {
                tauri::async_runtime::spawn(async move {
                    let _ = ws.shutdown().await;
                });
            } else {
                tauri::async_runtime::block_on(async move {
                    let _ = tokio::time::timeout(Duration::from_secs(2), ws.shutdown()).await;
                });
            }
        }
    }
}

async fn run_ddsf_frame_loop(
    state: DesktopBridgeState,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let mut next_tick = TokioInstant::now();
    let mut detached_ticks: u32 = 0;

    loop {
        if *shutdown_rx.borrow() {
            break;
        }

        let fps = state
            .data_bridge()
            .target_fps()
            .clamp(MIN_DATA_BRIDGE_FPS, MAX_DATA_BRIDGE_FPS);
        let period = Duration::from_nanos(1_000_000_000 / u64::from(fps));
        next_tick += period;
        let now = TokioInstant::now();
        if now > next_tick + period.saturating_mul(2) {
            next_tick = now + period;
        }

        tokio::select! {
            biased;
            res = shutdown_rx.changed() => {
                if res.is_err() || *shutdown_rx.borrow() {
                    break;
                }
            }
            _ = tokio::time::sleep_until(next_tick) => {}
        }

        if *shutdown_rx.borrow() {
            break;
        }

        let now_ns = host_monotonic_ns();
        if state.data_bridge().is_attached() {
            detached_ticks = 0;
            if let Err(err) = state.step_and_push_ddsf_frame(now_ns) {
                if err.code == "IPC_SEND_ERROR" {
                    debug!(message = %err.message, "detaching DDSF bridge after IPC send error");
                    state.data_bridge().detach();
                } else if err.code != "BRIDGE_NOT_ATTACHED" {
                    warn!(code = %err.code, message = %err.message, "DDSF frame push error");
                }
            }
        } else {
            detached_ticks = detached_ticks.wrapping_add(1);
            if detached_ticks % 6 == 0 {
                if let Some(ws) = state.workspace() {
                    let _ = ws.tick_scopes(now_ns);
                }
            }
        }
    }
}

async fn run_telemetry_loop(
    app_handle: tauri::AppHandle,
    state: DesktopBridgeState,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let mut ticker = tokio::time::interval(Duration::from_millis(DEFAULT_TELEMETRY_INTERVAL_MS));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            biased;
            res = shutdown_rx.changed() => {
                if res.is_err() || *shutdown_rx.borrow() {
                    break;
                }
            }
            _ = ticker.tick() => {
                if *shutdown_rx.borrow() {
                    break;
                }
                let Some(ws) = state.workspace() else {
                    continue;
                };
                let snap = ws.telemetry_snapshot();
                let evt: WorkspaceTelemetryEvent =
                    convert::telemetry_snapshot_to_event(&snap, now_epoch_ms());
                if let Err(err) = evt.emit(&app_handle) {
                    debug!(%err, "failed to emit workspace telemetry event");
                }
            }
        }
    }
}

#[inline]
fn now_epoch_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64() * 1_000.0)
}
