//! Headless benchmark, replay verification, and diagnostics CLI for `ddscope-core`.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};

use ddscope_core::clock::ClockMode;
use ddscope_core::ddsf::{DdsfBlockKind, DdsfZeroCopyReader};
use ddscope_core::protocol::{
    FireWaterConfig, JustFloatConfig, SimPresetConfig, JUSTFLOAT_TAIL_MAGIC, SIM_PRESET_MAGIC,
    SIM_PRESET_TAIL,
};
use ddscope_core::scope::{
    EdgeSlope, ScopeControllerConfig, TriggerCondition, TriggerRuntimeState, TriggerSweepMode,
};
use ddscope_core::transport::{
    host_monotonic_ns, DdrawReader, RawFileReplayConfig, RawTapDirection, SimConfig,
    SimWireFormat, TxPriority, DDRAW_MAGIC,
};
use ddscope_core::types::{ChannelId, CoreError, CoreResult, ScopeId, SourceId};
use ddscope_core::workspace::{
    ChannelTreeSnapshot, CoreWorkspace, DownlinkRequest, ScopeConfig, ScopeTrackBinding,
    SourceConfig, SourceProtocolConfig, SourceTransportConfig,
};

#[derive(Debug, Parser)]
#[command(name = "ddscope-cli", version, about = "DDScopeHost headless engine & benchmark CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Print core engine build metadata.
    Info,
    /// Run headless acquisition & M4 throughput benchmarks.
    Bench {
        /// Number of simulated signal sources.
        #[arg(long, default_value_t = 1)]
        sources: usize,
        /// Target aggregate sample rate in points/sec across all channels.
        #[arg(long, default_value_t = 5_000_000)]
        rate: u64,
        /// Steady-state measurement duration in seconds.
        #[arg(long, default_value_t = 2.0)]
        duration_secs: f64,
        /// Global ring-buffer memory quota in MiB.
        #[arg(long, default_value_t = 32)]
        quota_mib: usize,
    },
    /// Verify offline raw file (.ddraw / .bin / .log) replay integrity.
    ReplayVerify {
        /// Path to the raw capture file.
        #[arg(long)]
        input: String,
        /// Protocol codec override: auto, sim-preset, just-float, fire-water.
        #[arg(long, default_value = "auto")]
        codec: String,
        /// Analog / column channel count hint (1..=16).
        #[arg(long)]
        channels: Option<u8>,
    },
    /// Run sustained mixed-signal high-throughput soak & steady-state memory stability gate.
    Soak {
        /// Number of simulated mixed-signal sources.
        #[arg(long, default_value_t = 2)]
        sources: usize,
        /// Target aggregate sample rate in points/sec across all analog + digital channels.
        #[arg(long, default_value_t = 5_200_000)]
        rate: u64,
        /// Frame extraction & DDSF v1 packing cadence in Hz.
        #[arg(long, default_value_t = 120)]
        fps: u32,
        /// Steady-state soak measurement duration in seconds.
        #[arg(long, default_value_t = 10.0)]
        duration_secs: f64,
        /// Global ring-buffer memory quota in MiB.
        #[arg(long, default_value_t = 32)]
        quota_mib: usize,
        /// Verify full DoD glitch latch, 10,000x zoom retention, and rearm pin release.
        #[arg(long, default_value_t = false)]
        verify_dod: bool,
        /// Optional path to write the machine-readable JSON soak report.
        #[arg(long)]
        json_out: Option<PathBuf>,
    },
}

#[derive(Debug, serde::Serialize)]
struct SoakReport {
    sources: usize,
    total_channels: usize,
    per_source_rate_hz: f64,
    target_pts_per_sec: u64,
    fps: u32,
    duration_secs: f64,
    pts_delta: u64,
    rows_delta: u64,
    achieved_pts_per_sec: f64,
    dropped_packets: u64,
    decode_errors: u64,
    frames_packed: usize,
    pack_p50_us: f64,
    pack_p99_us: f64,
    pack_budget_us: f64,
    quota_limit_bytes: usize,
    quota_peak_bytes: usize,
    steady_mem_samples: usize,
    steady_mem_mean_bytes: f64,
    steady_mem_min_bytes: usize,
    steady_mem_max_bytes: usize,
    steady_mem_fluctuation_pct: f64,
    evicted_samples_total: u64,
    verify_dod: bool,
    glitch_latched: bool,
    glitch_pinned: bool,
    pinned_bytes: usize,
    zoomed_ch3_max: Option<f32>,
    rearmed_pinned_bytes: usize,
    passed: bool,
}

#[derive(Debug)]
struct BenchReport {
    sources: usize,
    total_channels: usize,
    target_pts_per_sec: u64,
    per_source_rate_hz: f64,
    duration_secs: f64,
    pts_delta: u64,
    rows_delta: u64,
    achieved_pts_per_sec: f64,
    dropped_packets: u64,
    decode_errors: u64,
    frames_packed: usize,
    pack_p50_us: f64,
    pack_p99_us: f64,
    pack_budget_us: f64,
    glitch_latched: bool,
    glitch_pinned: bool,
    pinned_bytes: usize,
    evicted_samples_total: u64,
    zoom_span_ns: u64,
    zoomed_ch3_max: Option<f32>,
    quota_used_bytes: usize,
    quota_limit_bytes: usize,
}

#[derive(Debug)]
struct InspectedCapture {
    is_ddraw: bool,
    file_bytes: u64,
    ddraw_version: Option<u16>,
    rx_records: u64,
    tx_records: u64,
    rx_payload_bytes: u64,
    tx_payload_bytes: u64,
    prefix: Vec<u8>,
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Info => {
            let info = ddscope_core::build_info();
            println!(
                "{} v{} (plugin-api v{})",
                info.name, info.version, info.plugin_api_version
            );
        }
        Commands::Bench {
            sources,
            rate,
            duration_secs,
            quota_mib,
        } => {
            if sources == 0
                || rate < 1_000
                || !duration_secs.is_finite()
                || duration_secs < 0.2
                || quota_mib < 8
            {
                eprintln!(
                    "Invalid benchmark parameters: require --sources >= 1, --rate >= 1000, \
                     --duration-secs >= 0.2, --quota-mib >= 8 (got sources={sources}, \
                     rate={rate}, duration_secs={duration_secs}, quota_mib={quota_mib})"
                );
                std::process::exit(2);
            }

            match run_bench(sources, rate, duration_secs, quota_mib) {
                Ok(report) => {
                    print_bench_report(&report);
                    let passed = report.achieved_pts_per_sec >= report.target_pts_per_sec as f64
                        && report.dropped_packets == 0
                        && report.decode_errors == 0
                        && report.pack_p99_us < report.pack_budget_us
                        && report.glitch_latched
                        && report.glitch_pinned
                        && report.zoomed_ch3_max.is_some_and(|m| m > 85.0);
                    if !passed {
                        eprintln!("BENCHMARK GATE FAILED:");
                        if report.achieved_pts_per_sec < report.target_pts_per_sec as f64 {
                            eprintln!(
                                "  - throughput {:.0} pts/s < target {} pts/s",
                                report.achieved_pts_per_sec, report.target_pts_per_sec
                            );
                        }
                        if report.dropped_packets > 0 {
                            eprintln!("  - dropped_packets = {} (expected 0)", report.dropped_packets);
                        }
                        if report.decode_errors > 0 {
                            eprintln!("  - decode_errors = {} (expected 0)", report.decode_errors);
                        }
                        if report.pack_p99_us >= report.pack_budget_us {
                            eprintln!(
                                "  - p99 frame pack latency {:.2} us >= budget {:.2} us",
                                report.pack_p99_us, report.pack_budget_us
                            );
                        }
                        if !report.glitch_latched || !report.glitch_pinned {
                            eprintln!(
                                "  - glitch scope did not latch+pin (latched={}, pinned={})",
                                report.glitch_latched, report.glitch_pinned
                            );
                        }
                        if !report.zoomed_ch3_max.is_some_and(|m| m > 85.0) {
                            eprintln!(
                                "  - zoomed ch3 max {:?} did not retain +80V glitch (> 85.0V)",
                                report.zoomed_ch3_max
                            );
                        }
                        std::process::exit(1);
                    }
                }
                Err(err) => {
                    eprintln!("Benchmark execution error: {err}");
                    std::process::exit(1);
                }
            }
        }
        Commands::ReplayVerify {
            input,
            codec,
            channels,
        } => {
            if let Some(ch) = channels {
                if !(1..=16).contains(&ch) {
                    eprintln!("Invalid --channels={ch}: must be within 1..=16");
                    std::process::exit(2);
                }
            }
            if !matches!(
                codec.as_str(),
                "auto" | "sim-preset" | "just-float" | "fire-water"
            ) {
                eprintln!(
                    "Invalid --codec={codec}: expected one of auto, sim-preset, just-float, fire-water"
                );
                std::process::exit(2);
            }

            if let Err(err) = run_replay_verify(Path::new(&input), &codec, channels) {
                eprintln!("Replay verification failed for `{input}`: {err}");
                std::process::exit(1);
            }
        }
        Commands::Soak {
            sources,
            rate,
            fps,
            duration_secs,
            quota_mib,
            verify_dod,
            json_out,
        } => {
            if sources == 0
                || rate < 1_000
                || !(1..=240).contains(&fps)
                || !duration_secs.is_finite()
                || duration_secs < 0.2
                || quota_mib < 8
            {
                eprintln!(
                    "Invalid soak parameters: require --sources >= 1, --rate >= 1000, \
                     --fps in 1..=240, --duration-secs >= 0.2, --quota-mib >= 8"
                );
                std::process::exit(2);
            }

            match run_soak(sources, rate, fps, duration_secs, quota_mib, verify_dod) {
                Ok(report) => {
                    print_soak_report(&report);
                    if let Some(ref out_path) = json_out {
                        if let Some(parent) = out_path.parent() {
                            if !parent.as_os_str().is_empty() {
                                let _ = std::fs::create_dir_all(parent);
                            }
                        }
                        match serde_json::to_string_pretty(&report) {
                            Ok(json_str) => {
                                if let Err(err) = std::fs::write(out_path, json_str) {
                                    eprintln!(
                                        "Failed to write JSON soak report to `{}`: {err}",
                                        out_path.display()
                                    );
                                    std::process::exit(1);
                                }
                            }
                            Err(err) => {
                                eprintln!("Failed to serialize JSON soak report: {err}");
                                std::process::exit(1);
                            }
                        }
                    }
                    if !report.passed {
                        eprintln!("SOAK GATE FAILED!");
                        std::process::exit(1);
                    }
                }
                Err(err) => {
                    eprintln!("Soak execution error: {err}");
                    std::process::exit(1);
                }
            }
        }
    }
}

fn run_bench(
    sources: usize,
    target_rate: u64,
    duration_secs: f64,
    quota_mib: usize,
) -> CoreResult<BenchReport> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(CoreError::Io)?;

    let quota_bytes = quota_mib.saturating_mul(1024 * 1024);
    let workspace = CoreWorkspace::with_quota_bytes(rt.handle().clone(), quota_bytes);
    let per_source_rate_hz =
        (((target_rate as f64) / ((sources * 4) as f64)) * 1.25).clamp(1_000.0, 10_000_000.0);

    let mut source_ids = Vec::with_capacity(sources);
    for idx in 0..sources {
        let id = SourceId::new(format!("sim{idx}"));
        let sim_cfg = SimConfig {
            sample_rate_hz: per_source_rate_hz,
            analog_channels: 4,
            enable_glitch: false,
            glitch_amplitude: 80.0,
            wire_format: SimWireFormat::JustFloat,
            paced: true,
            ..SimConfig::default()
        };
        let jf_cfg = JustFloatConfig::new(
            id.as_str(),
            vec![
                "ch0".into(),
                "ch1".into(),
                "ch2".into(),
                "ch3".into(),
            ],
        );
        let src_cfg = SourceConfig::new(
            id.clone(),
            SourceTransportConfig::Sim(sim_cfg),
            ClockMode::Tier2SoftwarePll {
                nominal_sample_rate_hz: Some(per_source_rate_hz),
            },
            SourceProtocolConfig::JustFloat(jf_cfg),
        );
        workspace.create_source(src_cfg)?;
        source_ids.push(id);
    }

    let sim0_id = &source_ids[0];
    let sim0_ch0 = resolve_ch_id(&workspace, sim0_id, "ch0")?;
    let sim0_ch1 = resolve_ch_id(&workspace, sim0_id, "ch1")?;
    let sim0_ch2 = resolve_ch_id(&workspace, sim0_id, "ch2")?;
    let sim0_ch3 = resolve_ch_id(&workspace, sim0_id, "ch3")?;

    let scope1_ctrl = ScopeControllerConfig {
        scope_id: ScopeId(1),
        title: Some("Glitch Catcher (Single)".into()),
        canvas_width_px: 1280,
        time_span_ns: 10_000_000,
        pre_trigger_ratio: 0.5,
        pan_offset_ns: 0,
        sweep_mode: TriggerSweepMode::Single,
        auto_timeout_ns: 100_000_000,
        trigger: Some(TriggerCondition::Edge {
            channel: sim0_ch3,
            level: 60.0,
            hysteresis: 2.0,
            slope: EdgeSlope::Rising,
        }),
        sync_group_id: None,
        paused: false,
    };
    workspace.upsert_scope(
        ScopeConfig::new(ScopeId(1), scope1_ctrl)
            .with_title("Glitch Catcher (Single)")
            .with_tracks(vec![
                ScopeTrackBinding::analog(sim0_ch0),
                ScopeTrackBinding::analog(sim0_ch1),
                ScopeTrackBinding::analog(sim0_ch2),
                ScopeTrackBinding::analog(sim0_ch3),
            ])
            .with_width_px(1280)
            .with_auto_pin_on_latch(true),
    )?;

    let mut roll_tracks = vec![
        ScopeTrackBinding::analog(sim0_ch0),
        ScopeTrackBinding::analog(sim0_ch1),
    ];
    if let Some(sim1_id) = source_ids.get(1) {
        roll_tracks.push(ScopeTrackBinding::analog(resolve_ch_id(
            &workspace, sim1_id, "ch0",
        )?));
        roll_tracks.push(ScopeTrackBinding::analog(resolve_ch_id(
            &workspace, sim1_id, "ch1",
        )?));
    }

    let scope2_ctrl = ScopeControllerConfig {
        scope_id: ScopeId(2),
        title: Some("Live Monitor (Roll)".into()),
        canvas_width_px: 1280,
        time_span_ns: 20_000_000,
        pre_trigger_ratio: 0.5,
        pan_offset_ns: 0,
        sweep_mode: TriggerSweepMode::Roll,
        auto_timeout_ns: 100_000_000,
        trigger: None,
        sync_group_id: None,
        paused: false,
    };
    workspace.upsert_scope(
        ScopeConfig::new(ScopeId(2), scope2_ctrl)
            .with_title("Live Monitor (Roll)")
            .with_tracks(roll_tracks)
            .with_width_px(1280),
    )?;

    for id in &source_ids {
        workspace.start_source(id)?;
    }

    // Warm up (~100 ms) so every source primes its TriggerEngine at ~24V (ArmedLow) and stabilizes PLL.
    let warmup_start = Instant::now();
    loop {
        workspace.step_and_pack_ddsf_with(host_monotonic_ns(), validate_ddsf_frame)??;
        let sim0_samples = workspace
            .find_channel(sim0_id, "ch3")
            .map_or(0, |e| e.total_samples());
        let elapsed = warmup_start.elapsed();
        if (elapsed >= Duration::from_millis(100) && sim0_samples >= 16)
            || elapsed >= Duration::from_millis(500)
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(8));
    }

    // Steady-state measurement window start (prior to glitch injection).
    let (pts_start, _, total_channels) = tree_totals(&workspace.channel_tree());
    let rows_start: u64 = workspace
        .telemetry_snapshot()
        .sources
        .iter()
        .map(|s| s.total_decoded_samples)
        .sum();
    let t_measure = Instant::now();

    // Inject a deterministic single-sample +80V glitch (~104V) on sim0/ch3.
    let _ = workspace.send_downlink(
        sim0_id,
        &DownlinkRequest::from_ascii("GLITCH", true, TxPriority::High),
    )?;

    let measure_dur = Duration::from_secs_f64(duration_secs);
    let tick_interval = Duration::from_micros(8_333); // 120 Hz UI frame pacing
    let mut pack_latencies_ns = Vec::with_capacity(((duration_secs * 130.0) as usize).max(32));
    let mut fallback_sent = false;
    let mut next_tick = Instant::now() + tick_interval;

    while t_measure.elapsed() < measure_dur {
        workspace.step_and_pack_ddsf_with(host_monotonic_ns(), validate_ddsf_frame)??;
        let snap = workspace.telemetry_snapshot();
        pack_latencies_ns.push(snap.last_frame_pack_ns);

        if !fallback_sent && t_measure.elapsed() >= Duration::from_millis(150) {
            let s1 = workspace.scope_summary(&ScopeId(1))?;
            if s1.runtime_state != TriggerRuntimeState::LatchedPinned {
                let _ = workspace.send_downlink(
                    sim0_id,
                    &DownlinkRequest::from_ascii("GLITCH", true, TxPriority::High),
                )?;
            }
            fallback_sent = true;
        }

        let now = Instant::now();
        if next_tick > now {
            let remaining = measure_dur.saturating_sub(t_measure.elapsed());
            let sleep_for = (next_tick - now).min(remaining);
            if !sleep_for.is_zero() {
                std::thread::sleep(sleep_for);
            }
        }
        next_tick += tick_interval;
    }

    // One final step/pack before taking steady-state totals (strictly BEFORE shutdown()).
    workspace.step_and_pack_ddsf_with(host_monotonic_ns(), validate_ddsf_frame)??;

    let measure_elapsed = t_measure.elapsed().as_secs_f64().max(1e-9);
    let (pts_end, evicted_samples_total, _) = tree_totals(&workspace.channel_tree());
    let telemetry = workspace.telemetry_snapshot();
    pack_latencies_ns.push(telemetry.last_frame_pack_ns);

    let rows_end: u64 = telemetry.sources.iter().map(|s| s.total_decoded_samples).sum();
    let pts_delta = pts_end.saturating_sub(pts_start);
    let rows_delta = rows_end.saturating_sub(rows_start);
    let achieved_pts_per_sec = pts_delta as f64 / measure_elapsed;
    let dropped_packets: u64 = telemetry.sources.iter().map(|s| s.dropped_packets).sum();
    let decode_errors: u64 = telemetry.sources.iter().map(|s| s.decode_errors).sum();

    let scope1_summary = workspace.scope_summary(&ScopeId(1))?;
    let glitch_latched = scope1_summary.runtime_state == TriggerRuntimeState::LatchedPinned;
    let glitch_pinned = scope1_summary.is_pinned;
    let pinned_bytes = scope1_summary.pinned_bytes;

    // Dynamically zoom ScopeId(1) around T0 while quota rollovers are active and inspect DDSF track stats.
    let zoom_span_ns = ((8.0 * 1_000_000_000.0 / per_source_rate_hz) as u64).max(10_000);
    workspace.set_scope_viewport(&ScopeId(1), zoom_span_ns, 0, Some(1280))?;
    let zoomed_ch3_max =
        workspace.pack_ddsf_frame_with(host_monotonic_ns(), |bytes| -> CoreResult<Option<f32>> {
            extract_track_max(bytes, 1, sim0_ch3.0)
        })??;

    let quota_used_bytes = telemetry.quota.allocated_bytes;
    let quota_limit_bytes = telemetry.quota.quota_bytes;

    rt.block_on(workspace.shutdown())?;

    pack_latencies_ns.sort_unstable();
    let pack_p50_us = percentile_sorted_ns(&pack_latencies_ns, 0.50) / 1_000.0;
    let pack_p99_us = percentile_sorted_ns(&pack_latencies_ns, 0.99) / 1_000.0;
    let pack_budget_us = if cfg!(debug_assertions) {
        10_000.0
    } else {
        500.0
    };

    Ok(BenchReport {
        sources,
        total_channels,
        target_pts_per_sec: target_rate,
        per_source_rate_hz,
        duration_secs: measure_elapsed,
        pts_delta,
        rows_delta,
        achieved_pts_per_sec,
        dropped_packets,
        decode_errors,
        frames_packed: pack_latencies_ns.len(),
        pack_p50_us,
        pack_p99_us,
        pack_budget_us,
        glitch_latched,
        glitch_pinned,
        pinned_bytes,
        evicted_samples_total,
        zoom_span_ns,
        zoomed_ch3_max,
        quota_used_bytes,
        quota_limit_bytes,
    })
}

fn print_bench_report(r: &BenchReport) {
    println!("DDScopeHost Benchmark Report");
    println!(
        "  sources={} total_channels={} per_source_rate_hz={:.0} duration={:.3}s",
        r.sources, r.total_channels, r.per_source_rate_hz, r.duration_secs
    );
    println!(
        "  throughput: {:.0} pts/s (target >= {} pts/s), rows_delta={}, pts_delta={}",
        r.achieved_pts_per_sec, r.target_pts_per_sec, r.rows_delta, r.pts_delta
    );
    println!(
        "  reliability: dropped_packets={}, decode_errors={}",
        r.dropped_packets, r.decode_errors
    );
    println!(
        "  ddsf_pack: frames={}, p50={:.2} us, p99={:.2} us (budget < {:.0} us)",
        r.frames_packed, r.pack_p50_us, r.pack_p99_us, r.pack_budget_us
    );
    println!(
        "  glitch_pin: latched={}, pinned={}, pinned_bytes={}, evicted_samples_total={}",
        r.glitch_latched, r.glitch_pinned, r.pinned_bytes, r.evicted_samples_total
    );
    println!(
        "  glitch_zoom: zoom_span_ns={} ns, ch3_max={:?} V, quota_used={}/{} bytes",
        r.zoom_span_ns, r.zoomed_ch3_max, r.quota_used_bytes, r.quota_limit_bytes
    );
}

fn run_soak(
    sources: usize,
    target_rate: u64,
    fps: u32,
    duration_secs: f64,
    quota_mib: usize,
    verify_dod: bool,
) -> CoreResult<SoakReport> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(CoreError::Io)?;

    let quota_bytes = quota_mib.saturating_mul(1024 * 1024);
    let workspace = CoreWorkspace::with_quota_bytes(rt.handle().clone(), quota_bytes);
    // SimPreset registers 4 analog + 3 bool dense channels = 7 high-rate channels per source.
    let per_source_rate_hz =
        (((target_rate as f64) / ((sources * 7) as f64)) * 1.35).clamp(1_000.0, 10_000_000.0);

    let mut source_ids = Vec::with_capacity(sources);
    for idx in 0..sources {
        let id = SourceId::new(format!("sim{idx}"));
        let sim_cfg = SimConfig {
            sample_rate_hz: per_source_rate_hz,
            analog_channels: 4,
            enable_glitch: false,
            glitch_amplitude: 80.0,
            wire_format: SimWireFormat::SimPreset,
            paced: true,
            ..SimConfig::default()
        };
        let preset_cfg = SimPresetConfig {
            source_id: id.as_str().to_string(),
            analog_channels: 4,
            ..SimPresetConfig::default()
        };
        let src_cfg = SourceConfig::new(
            id.clone(),
            SourceTransportConfig::Sim(sim_cfg),
            ClockMode::Tier1HardwareCounter {
                tick_hz: per_source_rate_hz,
                counter_bits: 32,
            },
            SourceProtocolConfig::SimPreset(preset_cfg),
        );
        workspace.create_source(src_cfg)?;
        source_ids.push(id);
    }

    let sim0_id = &source_ids[0];
    let sim0_ch0 = resolve_ch_id(&workspace, sim0_id, "ch0")?;
    let sim0_ch3 = resolve_ch_id(&workspace, sim0_id, "ch3")?;
    let sim0_pwm = resolve_ch_id(&workspace, sim0_id, "pwm_a")?;
    let sim0_fault = resolve_ch_id(&workspace, sim0_id, "fault_pin")?;
    let sim0_state = resolve_ch_id(&workspace, sim0_id, "ctrl_state")?;

    let scope1_ctrl = ScopeControllerConfig {
        scope_id: ScopeId(1),
        title: Some("MSO Scope 1 (Glitch Catcher)".into()),
        canvas_width_px: 1280,
        time_span_ns: 20_000_000,
        pre_trigger_ratio: 0.5,
        pan_offset_ns: 0,
        sweep_mode: if verify_dod {
            TriggerSweepMode::Single
        } else {
            TriggerSweepMode::Roll
        },
        auto_timeout_ns: 100_000_000,
        trigger: if verify_dod {
            Some(TriggerCondition::Edge {
                channel: sim0_ch3,
                level: 60.0,
                hysteresis: 2.0,
                slope: EdgeSlope::Rising,
            })
        } else {
            None
        },
        sync_group_id: None,
        paused: false,
    };
    workspace.upsert_scope(
        ScopeConfig::new(ScopeId(1), scope1_ctrl)
            .with_title("MSO Scope 1 (Glitch Catcher)")
            .with_tracks(vec![
                ScopeTrackBinding::analog(sim0_ch0),
                ScopeTrackBinding::analog(sim0_ch3),
                ScopeTrackBinding::digital(sim0_pwm),
                ScopeTrackBinding::digital(sim0_fault),
                ScopeTrackBinding::state_ribbon(sim0_state),
            ])
            .with_width_px(1280)
            .with_auto_pin_on_latch(verify_dod),
    )?;

    let mut roll_tracks = vec![
        ScopeTrackBinding::analog(sim0_ch0),
        ScopeTrackBinding::analog(sim0_ch3),
        ScopeTrackBinding::digital(sim0_pwm),
        ScopeTrackBinding::state_ribbon(sim0_state),
    ];
    if let Some(sim1_id) = source_ids.get(1) {
        roll_tracks.push(ScopeTrackBinding::analog(resolve_ch_id(
            &workspace, sim1_id, "ch0",
        )?));
        roll_tracks.push(ScopeTrackBinding::digital(resolve_ch_id(
            &workspace, sim1_id, "pwm_a",
        )?));
    }

    workspace.upsert_scope(
        ScopeConfig::new(
            ScopeId(2),
            ScopeControllerConfig {
                scope_id: ScopeId(2),
                title: Some("MSO Scope 2 (Roll Monitor)".into()),
                canvas_width_px: 1280,
                time_span_ns: 20_000_000,
                pre_trigger_ratio: 0.5,
                pan_offset_ns: 0,
                sweep_mode: TriggerSweepMode::Roll,
                auto_timeout_ns: 100_000_000,
                trigger: None,
                sync_group_id: None,
                paused: false,
            },
        )
        .with_title("MSO Scope 2 (Roll Monitor)")
        .with_tracks(roll_tracks)
        .with_width_px(1280),
    )?;

    for id in &source_ids {
        workspace.start_source(id)?;
    }

    // Warm up (~150 ms) so every source stabilizes and Scope 1 TriggerEngine primes ArmedLow.
    let warmup_start = Instant::now();
    loop {
        workspace.step_and_pack_ddsf_with(host_monotonic_ns(), validate_ddsf_frame)??;
        let sim0_samples = workspace
            .find_channel(sim0_id, "ch3")
            .map_or(0, |e| e.total_samples());
        let elapsed = warmup_start.elapsed();
        if (elapsed >= Duration::from_millis(150) && sim0_samples >= 64)
            || elapsed >= Duration::from_millis(600)
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(8));
    }

    let (pts_start, _, total_channels) = tree_totals(&workspace.channel_tree());
    let rows_start: u64 = workspace
        .telemetry_snapshot()
        .sources
        .iter()
        .map(|s| s.total_decoded_samples)
        .sum();
    let t_measure = Instant::now();

    if verify_dod {
        let _ = workspace.send_downlink(
            sim0_id,
            &DownlinkRequest::from_ascii("GLITCH", true, TxPriority::High),
        )?;
    }

    let measure_dur = Duration::from_secs_f64(duration_secs);
    let tick_interval = Duration::from_secs_f64(1.0 / (fps as f64));
    let mut pack_latencies_ns =
        Vec::with_capacity(((duration_secs * (fps as f64) * 1.1) as usize).max(32));
    let mut saturated_mem_samples = Vec::new();
    let mut quota_peak_bytes = 0usize;
    let mut fallback_sent = false;
    let mut next_tick = Instant::now() + tick_interval;

    while t_measure.elapsed() < measure_dur {
        workspace.step_and_pack_ddsf_with(host_monotonic_ns(), validate_ddsf_frame)??;
        let snap = workspace.telemetry_snapshot();
        pack_latencies_ns.push(snap.last_frame_pack_ns);
        quota_peak_bytes = quota_peak_bytes.max(snap.quota.allocated_bytes);

        let (_, ev_so_far, _) = tree_totals(&workspace.channel_tree());
        if ev_so_far > 0 && snap.quota.allocated_bytes >= (snap.quota.quota_bytes * 3) / 4 {
            saturated_mem_samples.push(snap.quota.allocated_bytes);
        }

        if verify_dod && !fallback_sent && t_measure.elapsed() >= Duration::from_millis(150) {
            let s1 = workspace.scope_summary(&ScopeId(1))?;
            if s1.runtime_state != TriggerRuntimeState::LatchedPinned {
                let _ = workspace.send_downlink(
                    sim0_id,
                    &DownlinkRequest::from_ascii("GLITCH", true, TxPriority::High),
                )?;
            }
            fallback_sent = true;
        }

        let now = Instant::now();
        if next_tick > now {
            let remaining = measure_dur.saturating_sub(t_measure.elapsed());
            let sleep_for = (next_tick - now).min(remaining);
            if !sleep_for.is_zero() {
                std::thread::sleep(sleep_for);
            }
        }
        next_tick = (next_tick + tick_interval).max(Instant::now() - tick_interval);
    }

    workspace.step_and_pack_ddsf_with(host_monotonic_ns(), validate_ddsf_frame)??;
    let measure_elapsed = t_measure.elapsed().as_secs_f64().max(1e-9);
    let (pts_end, evicted_samples_total, _) = tree_totals(&workspace.channel_tree());
    let telemetry = workspace.telemetry_snapshot();
    pack_latencies_ns.push(telemetry.last_frame_pack_ns);
    quota_peak_bytes = quota_peak_bytes.max(telemetry.quota.allocated_bytes);

    let rows_end: u64 = telemetry.sources.iter().map(|s| s.total_decoded_samples).sum();
    let pts_delta = pts_end.saturating_sub(pts_start);
    let rows_delta = rows_end.saturating_sub(rows_start);
    let achieved_pts_per_sec = pts_delta as f64 / measure_elapsed;
    let dropped_packets: u64 = telemetry.sources.iter().map(|s| s.dropped_packets).sum();
    let decode_errors: u64 = telemetry.sources.iter().map(|s| s.decode_errors).sum();

    let scope1_summary = workspace.scope_summary(&ScopeId(1))?;
    let glitch_latched = scope1_summary.runtime_state == TriggerRuntimeState::LatchedPinned;
    let glitch_pinned = scope1_summary.is_pinned;
    let pinned_bytes = scope1_summary.pinned_bytes;

    let mut zoomed_ch3_max = None;
    let mut rearmed_pinned_bytes = pinned_bytes;
    if verify_dod {
        let zoom_span_ns = ((8.0 * 1_000_000_000.0 / per_source_rate_hz) as u64).max(2_000);
        workspace.set_scope_viewport(&ScopeId(1), zoom_span_ns, 0, Some(1280))?;
        zoomed_ch3_max =
            workspace.pack_ddsf_frame_with(host_monotonic_ns(), |bytes| -> CoreResult<Option<f32>> {
                extract_track_max(bytes, 1, sim0_ch3.0)
            })??;
        workspace.rearm_scope(&ScopeId(1))?;
        rearmed_pinned_bytes = workspace.quota_stats().pinned_bytes;
    }

    let quota_limit_bytes = telemetry.quota.quota_bytes;
    rt.block_on(workspace.shutdown())?;

    pack_latencies_ns.sort_unstable();
    let pack_p50_us = percentile_sorted_ns(&pack_latencies_ns, 0.50) / 1_000.0;
    let pack_p99_us = percentile_sorted_ns(&pack_latencies_ns, 0.99) / 1_000.0;
    let pack_budget_us = if cfg!(debug_assertions) {
        15_000.0
    } else {
        500.0
    };

    let (steady_mem_mean_bytes, steady_mem_min_bytes, steady_mem_max_bytes, steady_mem_fluctuation_pct) =
        if saturated_mem_samples.is_empty() {
            (0.0, 0, 0, 0.0)
        } else {
            // Skip initial 2 transition samples right at saturation boundary.
            let slice = if saturated_mem_samples.len() > 8 {
                &saturated_mem_samples[2..]
            } else {
                &saturated_mem_samples[..]
            };
            let min_b = *slice.iter().min().unwrap();
            let max_b = *slice.iter().max().unwrap();
            let mean_b = slice.iter().map(|&b| b as f64).sum::<f64>() / (slice.len() as f64);
            let fluct = if mean_b > 0.0 {
                ((max_b.saturating_sub(min_b) as f64) / (2.0 * mean_b)) * 100.0
            } else {
                0.0
            };
            (mean_b, min_b, max_b, fluct)
        };

    let base_passed = achieved_pts_per_sec >= target_rate as f64
        && dropped_packets == 0
        && decode_errors == 0
        && pack_p99_us < pack_budget_us
        && quota_peak_bytes <= quota_limit_bytes
        && steady_mem_fluctuation_pct <= 5.0;
    let dod_passed = !verify_dod
        || (glitch_latched
            && glitch_pinned
            && zoomed_ch3_max.is_some_and(|m| m > 85.0)
            && rearmed_pinned_bytes == 0);

    Ok(SoakReport {
        sources,
        total_channels,
        per_source_rate_hz,
        target_pts_per_sec: target_rate,
        fps,
        duration_secs: measure_elapsed,
        pts_delta,
        rows_delta,
        achieved_pts_per_sec,
        dropped_packets,
        decode_errors,
        frames_packed: pack_latencies_ns.len(),
        pack_p50_us,
        pack_p99_us,
        pack_budget_us,
        quota_limit_bytes,
        quota_peak_bytes,
        steady_mem_samples: saturated_mem_samples.len(),
        steady_mem_mean_bytes,
        steady_mem_min_bytes,
        steady_mem_max_bytes,
        steady_mem_fluctuation_pct,
        evicted_samples_total,
        verify_dod,
        glitch_latched,
        glitch_pinned,
        pinned_bytes,
        zoomed_ch3_max,
        rearmed_pinned_bytes,
        passed: base_passed && dod_passed,
    })
}

fn print_soak_report(r: &SoakReport) {
    println!("DDScopeHost M5 Soak Report (passed={})", r.passed);
    println!(
        "  sources={} total_channels={} per_source_rate_hz={:.0} fps={} duration={:.3}s",
        r.sources, r.total_channels, r.per_source_rate_hz, r.fps, r.duration_secs
    );
    println!(
        "  throughput: {:.0} pts/s (target >= {} pts/s), rows_delta={}, pts_delta={}",
        r.achieved_pts_per_sec, r.target_pts_per_sec, r.rows_delta, r.pts_delta
    );
    println!(
        "  reliability: dropped_packets={}, decode_errors={}",
        r.dropped_packets, r.decode_errors
    );
    println!(
        "  ddsf_pack: frames={}, p50={:.2} us, p99={:.2} us (budget < {:.0} us)",
        r.frames_packed, r.pack_p50_us, r.pack_p99_us, r.pack_budget_us
    );
    println!(
        "  memory: peak={}/{} B, steady_samples={}, steady_range=[{}..{}], fluctuation=±{:.2}% (limit <= ±5.0%), evicted_total={}",
        r.quota_peak_bytes,
        r.quota_limit_bytes,
        r.steady_mem_samples,
        r.steady_mem_min_bytes,
        r.steady_mem_max_bytes,
        r.steady_mem_fluctuation_pct,
        r.evicted_samples_total
    );
    if r.verify_dod {
        println!(
            "  dod_check: latched={}, pinned={}, pinned_bytes={}, zoomed_ch3_max={:?} V, rearmed_pinned_bytes={}",
            r.glitch_latched, r.glitch_pinned, r.pinned_bytes, r.zoomed_ch3_max, r.rearmed_pinned_bytes
        );
    }
}

fn run_replay_verify(path: &Path, codec_arg: &str, channels_hint: Option<u8>) -> CoreResult<()> {
    let inspected = inspect_input_file(path)?;
    println!(
        "Inspected `{}` ({} bytes, format={})",
        path.display(),
        inspected.file_bytes,
        if inspected.is_ddraw { "ddraw" } else { "raw" }
    );
    if inspected.is_ddraw {
        println!(
            "  ddraw v{}: rx_records={} ({} B), tx_records={} ({} B), CRC32=OK, timestamps=monotonic",
            inspected.ddraw_version.unwrap_or(1),
            inspected.rx_records,
            inspected.rx_payload_bytes,
            inspected.tx_records,
            inspected.tx_payload_bytes
        );
        if inspected.rx_payload_bytes == 0 {
            println!("  note: .ddraw file contains 0 RX payload bytes; header/CRC verification passed.");
            return Ok(());
        }
    }

    let source_id = SourceId::new("replay0");
    let (protocol, clock_mode, detected_codec) = resolve_protocol_and_clock(
        codec_arg,
        channels_hint,
        &inspected.prefix,
        source_id.as_str(),
    )?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(CoreError::Io)?;
    let workspace = CoreWorkspace::with_quota_bytes(rt.handle().clone(), 64 * 1024 * 1024);

    let replay_cfg = RawFileReplayConfig::offline(PathBuf::from(path));
    workspace.create_source(SourceConfig::new(
        source_id.clone(),
        SourceTransportConfig::RawFileReplay(replay_cfg),
        clock_mode,
        protocol,
    ))?;
    workspace.start_source(&source_id)?;
    rt.block_on(workspace.wait_for_source_completion(&source_id, Duration::from_secs(30)))?;

    let telemetry = workspace.telemetry_snapshot();
    let tree = workspace.channel_tree();
    let src_snap = telemetry
        .sources
        .iter()
        .find(|s| s.source_id == source_id)
        .ok_or_else(|| CoreError::SourceNotFound(source_id.clone()))?
        .clone();

    if src_snap.decode_errors > 0 {
        let _ = rt.block_on(workspace.shutdown());
        return Err(CoreError::Protocol {
            message: format!(
                "replay produced {} decode errors with codec={detected_codec}",
                src_snap.decode_errors
            ),
            recoverable: false,
        });
    }
    if src_snap.total_decoded_samples == 0 {
        let _ = rt.block_on(workspace.shutdown());
        return Err(CoreError::Protocol {
            message: format!(
                "replay decoded 0 samples with codec={detected_codec} (try --codec/--channels)"
            ),
            recoverable: false,
        });
    }

    let (pts_total, _, channel_count) = tree_totals(&tree);
    let mut latest_ts_ok = false;
    for group in &tree.sources {
        for ch in &group.channels {
            if let Some(ts) = ch.latest_timestamp_ns {
                if ts > 0 {
                    latest_ts_ok = true;
                }
            }
        }
    }
    if !latest_ts_ok {
        let _ = rt.block_on(workspace.shutdown());
        return Err(CoreError::Storage(
            "replay channels reported no non-zero timestamps".into(),
        ));
    }

    rt.block_on(workspace.shutdown())?;

    println!(
        "  replay_decode: codec={}, channels={}, decoded_rows={}, channel_points={}, rx_bytes={}, decode_errors={}",
        detected_codec,
        channel_count,
        src_snap.total_decoded_samples,
        pts_total,
        src_snap.total_rx_bytes,
        src_snap.decode_errors
    );
    Ok(())
}

fn inspect_input_file(path: &Path) -> CoreResult<InspectedCapture> {
    let mut file = File::open(path)?;
    let file_bytes = file.metadata()?.len();
    if file_bytes == 0 {
        return Err(CoreError::InvalidConfig(format!(
            "capture file `{}` is empty",
            path.display()
        )));
    }

    let mut head = [0u8; 4];
    let is_ddraw = if file_bytes >= 4 {
        file.read_exact(&mut head)?;
        head == DDRAW_MAGIC
    } else {
        false
    };

    if is_ddraw {
        drop(file);
        let reader = DdrawReader::open(path)?;
        let header = reader.header();
        let mut rx_records = 0_u64;
        let mut tx_records = 0_u64;
        let mut rx_payload_bytes = 0_u64;
        let mut tx_payload_bytes = 0_u64;
        let mut prev_rx_ts: Option<u64> = None;
        let mut prev_tx_ts: Option<u64> = None;
        let mut prefix = Vec::with_capacity(512);

        for rec_res in reader {
            let rec = rec_res?;
            match rec.direction {
                RawTapDirection::Rx => {
                    if let Some(prev) = prev_rx_ts {
                        if rec.timestamp_ns < prev {
                            return Err(CoreError::Storage(format!(
                                ".ddraw RX timestamp regression: {} < {}",
                                rec.timestamp_ns, prev
                            )));
                        }
                    }
                    prev_rx_ts = Some(rec.timestamp_ns);
                    rx_records = rx_records.saturating_add(1);
                    rx_payload_bytes = rx_payload_bytes.saturating_add(rec.payload.len() as u64);
                    if prefix.len() < 512 && !rec.payload.is_empty() {
                        let take = (512 - prefix.len()).min(rec.payload.len());
                        prefix.extend_from_slice(&rec.payload[..take]);
                    }
                }
                RawTapDirection::Tx => {
                    if let Some(prev) = prev_tx_ts {
                        if rec.timestamp_ns < prev {
                            return Err(CoreError::Storage(format!(
                                ".ddraw TX timestamp regression: {} < {}",
                                rec.timestamp_ns, prev
                            )));
                        }
                    }
                    prev_tx_ts = Some(rec.timestamp_ns);
                    tx_records = tx_records.saturating_add(1);
                    tx_payload_bytes = tx_payload_bytes.saturating_add(rec.payload.len() as u64);
                }
            }
        }

        Ok(InspectedCapture {
            is_ddraw: true,
            file_bytes,
            ddraw_version: Some(header.version),
            rx_records,
            tx_records,
            rx_payload_bytes,
            tx_payload_bytes,
            prefix,
        })
    } else {
        drop(file);
        let mut reader = BufReader::new(File::open(path)?);
        let mut prefix = vec![0u8; (file_bytes as usize).min(512)];
        reader.read_exact(&mut prefix)?;
        Ok(InspectedCapture {
            is_ddraw: false,
            file_bytes,
            ddraw_version: None,
            rx_records: 0,
            tx_records: 0,
            rx_payload_bytes: file_bytes,
            tx_payload_bytes: 0,
            prefix,
        })
    }
}

fn resolve_protocol_and_clock(
    codec_arg: &str,
    channels_hint: Option<u8>,
    prefix: &[u8],
    source_id: &str,
) -> CoreResult<(SourceProtocolConfig, ClockMode, &'static str)> {
    let resolved = match codec_arg {
        "sim-preset" | "just-float" | "fire-water" => codec_arg,
        "auto" => {
            if detect_sim_preset_channels(prefix).is_some() {
                "sim-preset"
            } else if detect_just_float_channels(prefix).is_some() {
                "just-float"
            } else {
                "fire-water"
            }
        }
        other => {
            return Err(CoreError::InvalidConfig(format!(
                "unsupported codec `{other}`"
            )));
        }
    };

    match resolved {
        "sim-preset" => {
            let analog_channels = channels_hint
                .or_else(|| detect_sim_preset_channels(prefix))
                .unwrap_or(4);
            let cfg = SimPresetConfig {
                source_id: source_id.to_owned(),
                analog_channels,
                ..SimPresetConfig::default()
            };
            Ok((
                SourceProtocolConfig::SimPreset(cfg),
                ClockMode::Tier1HardwareCounter {
                    tick_hz: 1_000_000.0,
                    counter_bits: 64,
                },
                "sim-preset",
            ))
        }
        "just-float" => {
            let ch_count = channels_hint
                .or_else(|| detect_just_float_channels(prefix))
                .unwrap_or(4);
            let channel_names = (0..ch_count).map(|i| format!("ch{i}")).collect();
            let cfg = JustFloatConfig::new(source_id, channel_names);
            Ok((
                SourceProtocolConfig::JustFloat(cfg),
                ClockMode::Tier3HostArrival,
                "just-float",
            ))
        }
        "fire-water" => {
            let ch_count = channels_hint
                .or_else(|| detect_fire_water_channels(prefix))
                .unwrap_or(1);
            let channel_names = (0..ch_count).map(|i| format!("ch{i}")).collect();
            let cfg = FireWaterConfig::from_channel_names(source_id.to_owned(), channel_names);
            Ok((
                SourceProtocolConfig::FireWater(cfg),
                ClockMode::Tier3HostArrival,
                "fire-water",
            ))
        }
        _ => unreachable!(),
    }
}

fn detect_sim_preset_channels(prefix: &[u8]) -> Option<u8> {
    if prefix.len() < 16 {
        return None;
    }
    let search_limit = prefix.len().saturating_sub(1);
    for off in 0..search_limit {
        if prefix[off..off + 2] == SIM_PRESET_MAGIC {
            let a = *prefix.get(off + 2)?;
            if (1..=16).contains(&a) {
                let tail_off = off + 10 + 4 * usize::from(a);
                if prefix.get(tail_off..tail_off + 2) == Some(&SIM_PRESET_TAIL) {
                    return Some(a);
                }
            }
        }
    }
    None
}

fn detect_just_float_channels(prefix: &[u8]) -> Option<u8> {
    // Prefer matching two consecutive JustFloat frames of identical channel count.
    for ch in 1..=16_usize {
        let frame_bytes = 4 * ch + 4;
        let tail0 = 4 * ch;
        let tail1 = frame_bytes + 4 * ch;
        if prefix.get(tail0..tail0 + 4) == Some(&JUSTFLOAT_TAIL_MAGIC)
            && prefix.get(tail1..tail1 + 4) == Some(&JUSTFLOAT_TAIL_MAGIC)
        {
            return Some(ch as u8);
        }
    }
    for ch in 1..=16_usize {
        let tail0 = 4 * ch;
        if prefix.get(tail0..tail0 + 4) == Some(&JUSTFLOAT_TAIL_MAGIC) {
            return Some(ch as u8);
        }
    }
    None
}

fn detect_fire_water_channels(prefix: &[u8]) -> Option<u8> {
    let text = std::str::from_utf8(prefix).ok()?;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let body = trimmed
            .split_once(':')
            .map_or(trimmed, |(_, rhs)| rhs.trim());
        let cols = body.split(',').filter(|s| !s.trim().is_empty()).count();
        if (1..=16).contains(&cols) {
            return Some(cols as u8);
        }
    }
    None
}

fn resolve_ch_id(workspace: &CoreWorkspace, source_id: &SourceId, name: &str) -> CoreResult<ChannelId> {
    workspace
        .find_channel(source_id, name)
        .map(|entry| entry.id())
        .ok_or_else(|| {
            CoreError::ChannelNotFound(format!("{}/{name}", source_id.as_str()))
        })
}

fn validate_ddsf_frame(bytes: &[u8]) -> CoreResult<()> {
    let reader = DdsfZeroCopyReader::new(bytes)?;
    for scope_view in reader.scopes() {
        for track in scope_view.tracks() {
            match DdsfBlockKind::from_u8(track.header().block_kind) {
                Some(DdsfBlockKind::AnalogRawXY) => {
                    let _ = track.raw_points()?;
                }
                Some(DdsfBlockKind::AnalogM4Column) => {
                    let _ = track.m4_columns()?;
                }
                Some(DdsfBlockKind::DigitalEdges) => {
                    let _ = track.digital_edges()?;
                }
                Some(DdsfBlockKind::StateRibbons) => {
                    let _ = track.state_ribbons()?;
                }
                Some(DdsfBlockKind::SpectrumFrame) => {
                    let _ = track.spectrum_bins()?;
                }
                None => {
                    return Err(CoreError::Protocol {
                        message: format!(
                            "unknown DDSF block kind {}",
                            track.header().block_kind
                        ),
                        recoverable: false,
                    });
                }
            }
        }
    }
    let _ = reader.snapshots()?;
    Ok(())
}

fn extract_track_max(bytes: &[u8], scope_id: u32, channel_id: u32) -> CoreResult<Option<f32>> {
    let reader = DdsfZeroCopyReader::new(bytes)?;
    for scope_view in reader.scopes() {
        if scope_view.header().scope_id != scope_id {
            continue;
        }
        for track in scope_view.tracks() {
            if track.header().channel_id != channel_id {
                continue;
            }
            if let Some(stats) = track.stats() {
                return Ok(Some(stats.max));
            }
            if let Ok(pts) = track.raw_points() {
                return Ok(pts
                    .iter()
                    .map(|p| p.y_val)
                    .filter(|v| v.is_finite())
                    .reduce(f32::max));
            }
            if let Ok(cols) = track.m4_columns() {
                return Ok(cols
                    .iter()
                    .map(|c| c.v_max)
                    .filter(|v| v.is_finite())
                    .reduce(f32::max));
            }
        }
    }
    Ok(None)
}

fn tree_totals(tree: &ChannelTreeSnapshot) -> (u64, u64, usize) {
    let mut total_pts = 0_u64;
    let mut evicted_pts = 0_u64;
    for src in &tree.sources {
        for ch in &src.channels {
            total_pts = total_pts.saturating_add(ch.total_samples);
            evicted_pts = evicted_pts.saturating_add(ch.evicted_samples);
        }
    }
    (total_pts, evicted_pts, tree.total_channels)
}

fn percentile_sorted_ns(sorted: &[u64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() - 1) as f64 * q).round() as usize;
    sorted[idx.min(sorted.len() - 1)] as f64
}
