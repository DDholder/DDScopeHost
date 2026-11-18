//! End-to-end integration tests for Milestone M3 Gate Task `T04`:
//! - `CoreWorkspace` multi-source lifecycle, `channel_tree()`, TX downlink (`STATE=FAULT`, `GLITCH`, coalesced KV)
//! - `Single` 1-sample `+80V` glitch `LatchedPinned` survival across `>= 2x` `16 MiB` quota rollover + `10,000x` zoom-in + `rearm_scope`
//! - `StateEquals` trigger & mixed `Analog` + `Digital` + `StateRibbon` DDSF v1 3-lane output
//! - `SyncGroup` timebase & cursor synchronization across sibling scopes
//! - Bypass `.ddraw` recording + `DdrawReader` CRC32 validation + `RawFileReplay` seek & offline completion
//! - Zero-copy `DDSF v1` frame packing latency verification

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio::runtime::Handle;

use ddscope_core::clock::ClockMode;
use ddscope_core::ddsf::{
    DdsfBlockKind, DdsfZeroCopyReader, DDSF_FLAG_PINNED_ACTIVE, DDSF_MAGIC, DDSF_VERSION_V1,
};
use ddscope_core::protocol::{
    JustFloatConfig, JustTypedChannelKind, JustTypedChannelSpec, JustTypedConfig, SimPresetConfig,
    JUSTFLOAT_TAIL_MAGIC,
};
use ddscope_core::scope::{
    EdgeSlope, ScopeControllerConfig, TriggerCondition, TriggerRuntimeState, TriggerSweepMode,
};
use ddscope_core::storage::{ChannelEntry, ChannelStore};
use ddscope_core::transport::{
    host_monotonic_ns, DdrawReader, RawFileReplayConfig, RawTapDirection, SimConfig,
    SimWireFormat, TxPriority, DDRAW_VERSION,
};
use ddscope_core::types::{ChannelKind, ScalarType, ScalarValue, ScopeId, SourceId, SyncGroupId};
use ddscope_core::workspace::{
    CoreWorkspace, DownlinkRequest, ScopeConfig, ScopeTrackBinding, SourceConfig,
    SourceProtocolConfig, SourceRuntimeState, SourceTransportConfig, SyncGroupUpdate,
};

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

struct TempFileGuard {
    path: PathBuf,
}

impl TempFileGuard {
    fn new(prefix: &str, ext: &str) -> Self {
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("ddscope_t04_{prefix}_{pid}_{seq}.{ext}"));
        let _ = std::fs::remove_file(&path);
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn entry_evicted_samples(entry: &ChannelEntry) -> u64 {
    match entry.store() {
        ChannelStore::Scalar(s) => s.read().evicted_samples(),
        ChannelStore::Frame { .. } | ChannelStore::Event(_) => 0,
    }
}

async fn wait_until<F>(mut cond: F, timeout: Duration, label: &str)
where
    F: FnMut() -> bool,
{
    let deadline = tokio::time::Instant::now() + timeout;
    while !cond() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out after {timeout:?} waiting for: {label}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn t04_multi_source_lifecycle_channel_tree_and_downlink() {
    let ws = CoreWorkspace::new(Handle::current());
    let sim0 = SourceId::new("sim0");
    let sim1 = SourceId::new("sim1");

    ws.create_source(SourceConfig::sim(
        sim0.clone(),
        SimConfig {
            sample_rate_hz: 100_000.0,
            analog_channels: 4,
            enable_glitch: false,
            paced: true,
            ..SimConfig::default()
        },
    ))
    .unwrap();

    ws.create_source(SourceConfig::new(
        sim1.clone(),
        SourceTransportConfig::Sim(SimConfig {
            sample_rate_hz: 100_000.0,
            analog_channels: 4,
            enable_glitch: false,
            wire_format: SimWireFormat::JustFloat,
            paced: true,
            ..SimConfig::default()
        }),
        ClockMode::Tier2SoftwarePll {
            nominal_sample_rate_hz: Some(100_000.0),
        },
        SourceProtocolConfig::JustFloat(JustFloatConfig::new(
            "sim1",
            vec!["ch0".into(), "ch1".into(), "ch2".into(), "ch3".into()],
        )),
    ))
    .unwrap();

    let tree_before = ws.channel_tree();
    assert_eq!(tree_before.total_channels, 12); // 8 on sim0 (SimPreset) + 4 on sim1 (JustFloat)
    assert_eq!(tree_before.sources.len(), 2);
    assert!(ws.find_channel(&sim0, "sim0/ch3").is_none()); // local path is "ch3"
    assert!(ws.find_channel(&sim0, "ch3").is_some());
    assert!(ws.find_channel(&sim0, "ctrl_state").is_some());
    assert!(ws.find_channel(&sim1, "ch2").is_some());

    ws.start_source(&sim0).unwrap();
    ws.start_source(&sim1).unwrap();

    wait_until(
        || {
            let s0_ok = ws
                .find_channel(&sim0, "ch0")
                .is_some_and(|e| e.total_samples() >= 1_000);
            let s1_ok = ws
                .find_channel(&sim1, "ch0")
                .is_some_and(|e| e.total_samples() >= 1_000);
            s0_ok && s1_ok
        },
        Duration::from_secs(5),
        "sim0 and sim1 decoded >= 1000 samples",
    )
    .await;

    // Downlink STATE=FAULT must drive ctrl_state -> 3 (OCP_FAULT) and fault_pin -> 1.
    let fault_receipt = ws
        .send_downlink(
            &sim0,
            &DownlinkRequest::from_ascii("STATE=FAULT", true, TxPriority::High),
        )
        .unwrap();
    assert!(fault_receipt.accepted);

    wait_until(
        || {
            let st_ok = ws
                .find_channel(&sim0, "ctrl_state")
                .and_then(|e| e.latest_value())
                .is_some_and(|(_, v)| (v - 3.0).abs() < 1e-6);
            let fp_ok = ws
                .find_channel(&sim0, "fault_pin")
                .and_then(|e| e.latest_value())
                .is_some_and(|(_, v)| v > 0.5);
            st_ok && fp_ok
        },
        Duration::from_secs(3),
        "ctrl_state == OCP_FAULT (3) and fault_pin == 1",
    )
    .await;

    // Coalesced key-value rate command + manual glitch downlink.
    for rate_val in [110_000.0, 125_000.0] {
        let kv_receipt = ws
            .send_downlink(
                &sim0,
                &DownlinkRequest::from_key_value("RATE", ScalarValue::F64(rate_val), TxPriority::Normal)
                    .with_coalesce_key("sim0_rate"),
            )
            .unwrap();
        assert!(kv_receipt.accepted);
    }

    ws.send_downlink(
        &sim0,
        &DownlinkRequest::from_ascii("GLITCH", true, TxPriority::High),
    )
    .unwrap();
    let sim0_ctrl = ws.sim_control(&sim0).unwrap().unwrap();
    wait_until(
        || sim0_ctrl.injected_glitches() >= 1,
        Duration::from_secs(3),
        "manual glitch consumed by sim0",
    )
    .await;

    let stopped = ws.stop_source(&sim1).await.unwrap();
    assert_eq!(stopped.state, SourceRuntimeState::Stopped);
    ws.remove_source(&sim1).await.unwrap();
    assert_eq!(ws.channel_tree().total_channels, 8);

    ws.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn t04_single_glitch_latch_survives_quota_rollover_10000x_zoom_and_rearm() {
    // 16 MiB ring-buffer quota -> 40% pinned ceiling is 6.4 MiB.
    let quota_bytes = 16 * 1024 * 1024;
    let ws = CoreWorkspace::with_quota_bytes(Handle::current(), quota_bytes);
    let sim0 = SourceId::new("sim0");

    ws.create_source(SourceConfig::sim(
        sim0.clone(),
        SimConfig {
            sample_rate_hz: 2_000_000.0,
            analog_channels: 4,
            enable_glitch: false,
            glitch_amplitude: 80.0,
            paced: true,
            ..SimConfig::default()
        },
    ))
    .unwrap();

    let ch0 = ws.find_channel(&sim0, "ch0").unwrap().id();
    let ch3 = ws.find_channel(&sim0, "ch3").unwrap().id();
    let fault_pin = ws.find_channel(&sim0, "fault_pin").unwrap().id();

    let scope1 = ScopeId(1);
    let scope2 = ScopeId(2);
    ws.upsert_scope(
        ScopeConfig::new(
            scope1,
            ScopeControllerConfig {
                scope_id: scope1,
                title: Some("Single Glitch Catcher".into()),
                canvas_width_px: 1280,
                time_span_ns: 20_000_000, // 20 ms initial window
                pre_trigger_ratio: 0.5,
                pan_offset_ns: 0,
                sweep_mode: TriggerSweepMode::Single,
                auto_timeout_ns: 100_000_000,
                trigger: Some(TriggerCondition::Edge {
                    channel: ch3,
                    level: 60.0,
                    hysteresis: 2.0,
                    slope: EdgeSlope::Rising,
                }),
                sync_group_id: None,
                paused: false,
            },
        )
        .with_tracks(vec![
            ScopeTrackBinding::analog(ch3),
            ScopeTrackBinding::digital(fault_pin),
        ])
        .with_width_px(1280)
        .with_auto_pin_on_latch(true),
    )
    .unwrap();

    ws.upsert_scope(
        ScopeConfig::new(
            scope2,
            ScopeControllerConfig {
                scope_id: scope2,
                title: Some("Independent Roll Scope".into()),
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
        .with_tracks(vec![ScopeTrackBinding::analog(ch0)])
        .with_width_px(1280),
    )
    .unwrap();

    ws.start_source(&sim0).unwrap();

    // Warm up until TriggerEngine has primed at ~24V ArmedLow.
    let ch3_entry = ws.find_channel(&sim0, "ch3").unwrap();
    let warm_deadline = Instant::now() + Duration::from_secs(5);
    while ch3_entry.total_samples() < 2_000 {
        assert!(Instant::now() < warm_deadline, "warm-up timed out");
        let _ = ws.step_and_pack_ddsf(host_monotonic_ns()).unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let _ = ws.step_and_pack_ddsf(host_monotonic_ns()).unwrap();
    assert_eq!(
        ws.scope_summary(&scope1).unwrap().runtime_state,
        TriggerRuntimeState::Armed
    );

    // Inject deterministic 1-sample +80V spike (~104V) and wait for LatchedPinned.
    ws.send_downlink(
        &sim0,
        &DownlinkRequest::from_ascii("GLITCH", true, TxPriority::High),
    )
    .unwrap();

    let latch_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let _ = ws.step_and_pack_ddsf(host_monotonic_ns()).unwrap();
        let s1 = ws.scope_summary(&scope1).unwrap();
        if s1.runtime_state == TriggerRuntimeState::LatchedPinned {
            assert!(s1.is_pinned);
            assert!(s1.pinned_bytes > 0);
            break;
        }
        assert!(
            Instant::now() < latch_deadline,
            "scope1 did not reach LatchedPinned; state={:?}",
            s1.runtime_state
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    let roll_win_at_latch = ws.scope_summary(&scope2).unwrap().window.unwrap();
    let trig_ts_before_rollover = ws.scope_summary(&scope1).unwrap().trigger_time_ns.unwrap();

    // Crank source rate to 10 MHz (40M F32/s) and drive >= 2x 16 MiB ring-buffer rollover.
    ws.send_downlink(
        &sim0,
        &DownlinkRequest::from_ascii("RATE=10000000", true, TxPriority::High),
    )
    .unwrap();

    let rollover_deadline = Instant::now() + Duration::from_secs(20);
    while entry_evicted_samples(&ch3_entry) < 3_000_000 {
        assert!(
            Instant::now() < rollover_deadline,
            "timed out waiting for >= 3M evicted_samples on sim0/ch3 (got {})",
            entry_evicted_samples(&ch3_entry)
        );
        let _ = ws.step_and_pack_ddsf(host_monotonic_ns()).unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Confirm scope2 (Roll) kept advancing live while scope1 remained frozen on its glitch.
    let _ = ws.step_and_pack_ddsf(host_monotonic_ns()).unwrap();
    let roll_win_after = ws.scope_summary(&scope2).unwrap().window.unwrap();
    assert!(
        roll_win_after.win_t_start_ns > roll_win_at_latch.win_t_start_ns,
        "Roll scope must keep advancing independently during Single latch"
    );

    // Zoom scope1 in 10,000x (20 ms -> 2,000 ns) around the glitch T0 and verify DDSF v1 output.
    ws.set_scope_viewport(&scope1, 2_000, 0, Some(1280)).unwrap();
    let ddsf_frame = ws.pack_ddsf_frame(host_monotonic_ns()).unwrap();
    let reader = DdsfZeroCopyReader::new(&ddsf_frame).unwrap();
    assert_eq!(reader.header().magic, DDSF_MAGIC);
    assert_eq!(reader.header().version, DDSF_VERSION_V1);
    assert_ne!(
        reader.header().flags & DDSF_FLAG_PINNED_ACTIVE,
        0,
        "DDSF frame flag must indicate active pinned guard"
    );

    let mut found_ch3_glitch = false;
    for scope_view in reader.scopes() {
        if scope_view.header().scope_id != scope1.0 {
            continue;
        }
        assert_eq!(
            scope_view.header().trigger_state,
            TriggerRuntimeState::LatchedPinned.as_u8()
        );
        assert_eq!(scope_view.header().win_t_span_ns, 2_000);
        for track in scope_view.tracks() {
            if track.header().channel_id == ch3.0 {
                let stats = track.stats().expect("ch3 zoomed track must carry stats");
                assert!(
                    stats.max > 95.0,
                    "Single-sample +80V glitch (~104V) must survive rollover & 10,000x zoom, got max={}",
                    stats.max
                );
                let raw_pts = track.raw_points().expect("2us zoomed view at 2MHz (~4pts) uses AnalogRawXY");
                assert!(
                    raw_pts.iter().any(|p| p.y_val > 95.0),
                    "AnalogRawXY points must contain the ~104V glitch sample"
                );
                found_ch3_glitch = true;
            }
        }
    }
    assert!(found_ch3_glitch, "scope1 ch3 track missing from zoomed DDSF frame");
    assert_eq!(
        ws.scope_summary(&scope1).unwrap().trigger_time_ns,
        Some(trig_ts_before_rollover)
    );

    // Re-arm scope1 and verify all pinned chunks are immediately released.
    let rearmed = ws.rearm_scope(&scope1).unwrap();
    assert!(!rearmed.is_pinned);
    assert_eq!(rearmed.pinned_bytes, 0);
    assert_eq!(ws.quota_stats().pinned_bytes, 0);

    ws.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn t04_state_equals_trigger_and_mso_three_lane_ddsf() {
    let ws = CoreWorkspace::new(Handle::current());
    let sim0 = SourceId::new("sim0");
    ws.create_source(SourceConfig::sim(
        sim0.clone(),
        SimConfig {
            sample_rate_hz: 100_000.0,
            analog_channels: 4,
            enable_glitch: false,
            paced: true,
            ..SimConfig::default()
        },
    ))
    .unwrap();

    let ch0 = ws.find_channel(&sim0, "ch0").unwrap().id();
    let pwm_a = ws.find_channel(&sim0, "pwm_a").unwrap().id();
    let fault_pin = ws.find_channel(&sim0, "fault_pin").unwrap().id();
    let ctrl_state = ws.find_channel(&sim0, "ctrl_state").unwrap().id();

    let scope1 = ScopeId(10);
    ws.upsert_scope(
        ScopeConfig::new(
            scope1,
            ScopeControllerConfig {
                scope_id: scope1,
                title: Some("MSO State Trigger".into()),
                canvas_width_px: 1280,
                time_span_ns: 10_000_000,
                pre_trigger_ratio: 0.5,
                pan_offset_ns: 0,
                sweep_mode: TriggerSweepMode::Single,
                auto_timeout_ns: 100_000_000,
                trigger: Some(TriggerCondition::StateEquals {
                    channel: ctrl_state,
                    state_code: 3, // OCP_FAULT
                }),
                sync_group_id: None,
                paused: false,
            },
        )
        .with_tracks(vec![
            ScopeTrackBinding::analog(ch0),
            ScopeTrackBinding::digital(pwm_a),
            ScopeTrackBinding::digital(fault_pin),
            ScopeTrackBinding::state_ribbon(ctrl_state),
        ])
        .with_width_px(1280),
    )
    .unwrap();

    ws.start_source(&sim0).unwrap();

    // Drive ctrl_state cleanly into CLOSED_LOOP (state=2), then re-arm before forcing OCP_FAULT (3).
    ws.send_downlink(
        &sim0,
        &DownlinkRequest::from_ascii("STATE=CLOSED_LOOP", true, TxPriority::High),
    )
    .unwrap();
    let ctrl_entry = ws.find_channel(&sim0, "ctrl_state").unwrap();
    wait_until(
        || {
            ctrl_entry
                .latest_value()
                .is_some_and(|(_, val)| (val - 2.0).abs() < 1e-6)
        },
        Duration::from_secs(3),
        "sim0 ctrl_state entered CLOSED_LOOP",
    )
    .await;

    ws.rearm_scope(&scope1).unwrap();
    ws.send_downlink(
        &sim0,
        &DownlinkRequest::from_ascii("STATE=OCP_FAULT", true, TxPriority::High),
    )
    .unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let _ = ws.step_and_pack_ddsf(host_monotonic_ns()).unwrap();
        if ws.scope_summary(&scope1).unwrap().runtime_state == TriggerRuntimeState::LatchedPinned {
            break;
        }
        assert!(Instant::now() < deadline, "MSO StateEquals did not latch");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    let frame = ws.pack_ddsf_frame(host_monotonic_ns()).unwrap();
    let reader = DdsfZeroCopyReader::new(&frame).unwrap();
    let mut saw_analog = false;
    let mut saw_digital_pwm = false;
    let mut saw_digital_fault = false;
    let mut saw_state_ribbon = false;

    for scope_view in reader.scopes() {
        if scope_view.header().scope_id != scope1.0 {
            continue;
        }
        for track in scope_view.tracks() {
            let hdr = track.header();
            match DdsfBlockKind::from_u8(hdr.block_kind) {
                Some(DdsfBlockKind::AnalogRawXY) if hdr.channel_id == ch0.0 => {
                    assert!(!track.raw_points().unwrap().is_empty());
                    saw_analog = true;
                }
                Some(DdsfBlockKind::AnalogM4Column) if hdr.channel_id == ch0.0 => {
                    assert!(!track.m4_columns().unwrap().is_empty());
                    saw_analog = true;
                }
                Some(DdsfBlockKind::DigitalEdges) if hdr.channel_id == pwm_a.0 => {
                    let _ = track.digital_edges().unwrap();
                    saw_digital_pwm = true;
                }
                Some(DdsfBlockKind::DigitalEdges) if hdr.channel_id == fault_pin.0 => {
                    let edges = track.digital_edges().unwrap();
                    assert!(
                        !edges.is_empty(),
                        "fault_pin must record rising edge into OCP_FAULT"
                    );
                    saw_digital_fault = true;
                }
                Some(DdsfBlockKind::StateRibbons) if hdr.channel_id == ctrl_state.0 => {
                    let ribbons = track.state_ribbons().unwrap();
                    assert!(
                        ribbons.iter().any(|r| r.state_id == 3),
                        "state ribbons must contain OCP_FAULT (state_id=3)"
                    );
                    saw_state_ribbon = true;
                }
                _ => {}
            }
        }
    }

    assert!(saw_analog && saw_digital_pwm && saw_digital_fault && saw_state_ribbon);
    ws.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn t04_sync_group_viewport_and_cursor_coordination() {
    let ws = CoreWorkspace::new(Handle::current());
    let group_id = SyncGroupId(7);
    let s1 = ScopeId(1);
    let s2 = ScopeId(2);
    let s3 = ScopeId(3);

    ws.upsert_scope(
        ScopeConfig::new(
            s1,
            ScopeControllerConfig {
                scope_id: s1,
                sweep_mode: TriggerSweepMode::Roll,
                ..Default::default()
            },
        )
        .with_sync_group(group_id),
    )
    .unwrap();
    ws.upsert_scope(
        ScopeConfig::new(
            s2,
            ScopeControllerConfig {
                scope_id: s2,
                sweep_mode: TriggerSweepMode::Roll,
                ..Default::default()
            },
        )
        .with_sync_group(group_id),
    )
    .unwrap();
    ws.upsert_scope(ScopeConfig::new(
        s3,
        ScopeControllerConfig {
            scope_id: s3,
            sweep_mode: TriggerSweepMode::Roll,
            ..Default::default()
        },
    ))
    .unwrap();

    // Updating s1 viewport must propagate timebase + pan to s2, leaving ungrouped s3 unchanged.
    ws.set_scope_viewport(&s1, 2_500_000, -120_000, Some(1600))
        .unwrap();
    let sync_state = ws.sync_group_state(group_id).unwrap();
    assert_eq!(sync_state.time_span_ns, 2_500_000);
    assert_eq!(sync_state.center_offset_ns, -120_000);

    let step = ws.step_and_pack_ddsf(10_000_000).unwrap();
    let reader = DdsfZeroCopyReader::new(&step).unwrap();
    let mut spans = Vec::new();
    for sv in reader.scopes() {
        spans.push((sv.header().scope_id, sv.header().win_t_span_ns));
    }
    assert!(spans.contains(&(s1.0, 2_500_000)));
    assert!(spans.contains(&(s2.0, 2_500_000)));
    assert!(spans.contains(&(s3.0, 10_000_000)));

    // Explicit update_sync_group updates cursors & synchronizes sibling scopes.
    let updated = ws
        .update_sync_group(&SyncGroupUpdate {
            group_id,
            origin_scope_id: Some(s1),
            time_span_ns: 500_000,
            center_offset_ns: 45_000,
            cursor_a_ratio: Some(0.25),
            cursor_b_ratio: Some(0.75),
        })
        .unwrap();
    assert_eq!(updated.time_span_ns, 500_000);
    assert_eq!(updated.center_offset_ns, 45_000);
    assert_eq!(updated.cursor_a_ratio, Some(0.25));
    assert_eq!(updated.cursor_b_ratio, Some(0.75));

    let s2_step = ws.step_and_pack_ddsf(20_000_000).unwrap();
    let reader2 = DdsfZeroCopyReader::new(&s2_step).unwrap();
    let mut spans2 = Vec::new();
    for sv in reader2.scopes() {
        spans2.push((sv.header().scope_id, sv.header().win_t_span_ns));
    }
    assert!(spans2.contains(&(s1.0, 500_000)));
    assert!(spans2.contains(&(s2.0, 500_000)));
    assert!(spans2.contains(&(s3.0, 10_000_000)));

    ws.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn t04_ddraw_record_and_raw_file_replay_seek_roundtrip() {
    let ws = CoreWorkspace::new(Handle::current());
    let ddraw_file = TempFileGuard::new("replay", "ddraw");

    let live_id = SourceId::new("sim_rec");
    ws.create_source(SourceConfig::sim(
        live_id.clone(),
        SimConfig {
            sample_rate_hz: 100_000.0,
            analog_channels: 4,
            enable_glitch: true,
            glitch_period_samples: 500,
            paced: true,
            ..SimConfig::default()
        },
    ))
    .unwrap();
    ws.start_source(&live_id).unwrap();
    ws.start_raw_recording(&live_id, ddraw_file.path()).unwrap();

    wait_until(
        || {
            std::fs::metadata(ddraw_file.path())
                .map(|m| m.len() >= 16_384)
                .unwrap_or(false)
                && ws
                    .find_channel(&live_id, "ch0")
                    .is_some_and(|e| e.total_samples() >= 2_000)
        },
        Duration::from_secs(5),
        "live .ddraw recording accumulated >= 16 KiB and >= 2000 samples",
    )
    .await;

    ws.stop_raw_recording(&live_id).unwrap();
    ws.stop_source(&live_id).await.unwrap();
    ws.remove_source(&live_id).await.unwrap();

    // Validate .ddraw header, CRC32 checksums, and RX timestamps via DdrawReader.
    let reader = DdrawReader::open(ddraw_file.path()).unwrap();
    assert_eq!(reader.header().version, DDRAW_VERSION);
    let mut rx_records = 0_usize;
    let mut prev_rx_ts = 0_u64;
    for rec in reader {
        let rec = rec.unwrap();
        if rec.direction == RawTapDirection::Rx {
            assert!(rec.timestamp_ns >= prev_rx_ts);
            prev_rx_ts = rec.timestamp_ns;
            rx_records += 1;
        }
    }
    assert!(rx_records > 0);

    // Test live seek + speed change using looped paced replay.
    let replay_paced_id = SourceId::new("replay_paced");
    let mut paced_cfg = RawFileReplayConfig::paced(ddraw_file.path(), Some(200_000.0));
    paced_cfg.loop_playback = true;
    ws.create_source(SourceConfig::new(
        replay_paced_id.clone(),
        SourceTransportConfig::RawFileReplay(paced_cfg),
        ClockMode::Tier1HardwareCounter {
            tick_hz: 100_000.0,
            counter_bits: 64,
        },
        SourceProtocolConfig::SimPreset(SimPresetConfig {
            source_id: replay_paced_id.as_str().to_owned(),
            analog_channels: 4,
            ..SimPresetConfig::default()
        }),
    ))
    .unwrap();
    ws.start_source(&replay_paced_id).unwrap();

    let ch0_paced = ws.find_channel(&replay_paced_id, "ch0").unwrap();
    wait_until(
        || ch0_paced.total_samples() >= 500,
        Duration::from_secs(5),
        "paced replay decoded initial samples",
    )
    .await;
    let ts_before_seek = ch0_paced.latest_value().unwrap().0;
    let count_before_seek = ch0_paced.total_samples();

    ws.seek_replay_source(&replay_paced_id, Some(0.25), Some(false), Some(4.0))
        .unwrap();
    wait_until(
        || ch0_paced.total_samples() > count_before_seek + 500,
        Duration::from_secs(5),
        "paced replay continued decoding after seek",
    )
    .await;
    let ts_after_seek = ch0_paced.latest_value().unwrap().0;
    assert!(
        ts_after_seek >= ts_before_seek,
        "timestamps must remain monotonic after seek ({ts_after_seek} < {ts_before_seek})"
    );
    ws.stop_source(&replay_paced_id).await.unwrap();

    // Test full-speed offline replay completion & decode_errors == 0.
    let replay_offline_id = SourceId::new("replay_offline");
    ws.create_source(SourceConfig::new(
        replay_offline_id.clone(),
        SourceTransportConfig::RawFileReplay(RawFileReplayConfig::offline(ddraw_file.path())),
        ClockMode::Tier1HardwareCounter {
            tick_hz: 100_000.0,
            counter_bits: 64,
        },
        SourceProtocolConfig::SimPreset(SimPresetConfig {
            source_id: replay_offline_id.as_str().to_owned(),
            analog_channels: 4,
            ..SimPresetConfig::default()
        }),
    ))
    .unwrap();
    ws.start_source(&replay_offline_id).unwrap();
    ws.wait_for_source_completion(&replay_offline_id, Duration::from_secs(10))
        .await
        .unwrap();

    let telemetry = ws.telemetry_snapshot();
    let offline_snap = telemetry
        .sources
        .iter()
        .find(|s| s.source_id == replay_offline_id)
        .unwrap();
    assert_eq!(offline_snap.decode_errors, 0);
    assert!(offline_snap.total_decoded_samples > 1_000);

    ws.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn t04_zero_copy_ddsf_pack_latency_within_budget() {
    let ws = CoreWorkspace::new(Handle::current());
    let sim0 = SourceId::new("sim0");
    ws.create_source(SourceConfig::sim(
        sim0.clone(),
        SimConfig {
            sample_rate_hz: 1_000_000.0,
            analog_channels: 4,
            enable_glitch: false,
            paced: true,
            ..SimConfig::default()
        },
    ))
    .unwrap();

    let ch0 = ws.find_channel(&sim0, "ch0").unwrap().id();
    let ch1 = ws.find_channel(&sim0, "ch1").unwrap().id();
    let ch2 = ws.find_channel(&sim0, "ch2").unwrap().id();
    let ch3 = ws.find_channel(&sim0, "ch3").unwrap().id();

    for sid in [ScopeId(1), ScopeId(2)] {
        ws.upsert_scope(
            ScopeConfig::new(
                sid,
                ScopeControllerConfig {
                    scope_id: sid,
                    sweep_mode: TriggerSweepMode::Roll,
                    time_span_ns: 10_000_000,
                    canvas_width_px: 1280,
                    ..Default::default()
                },
            )
            .with_tracks(vec![
                ScopeTrackBinding::analog(ch0),
                ScopeTrackBinding::analog(ch1),
                ScopeTrackBinding::analog(ch2),
                ScopeTrackBinding::analog(ch3),
            ])
            .with_width_px(1280),
        )
        .unwrap();
    }

    ws.start_source(&sim0).unwrap();
    wait_until(
        || {
            ws.find_channel(&sim0, "ch0")
                .is_some_and(|e| e.total_samples() >= 20_000)
        },
        Duration::from_secs(5),
        "sim0 populated >= 20_000 samples",
    )
    .await;

    // Warm allocator / frame buffer once, then measure 25 zero-copy DDSF packs.
    let _ = ws
        .step_and_pack_ddsf_with(host_monotonic_ns(), |b| DdsfZeroCopyReader::new(b).is_ok())
        .unwrap();

    let mut samples_ns = Vec::with_capacity(25);
    for _ in 0..25 {
        let now_ns = host_monotonic_ns();
        let valid = ws
            .pack_ddsf_frame_with(now_ns, |bytes| DdsfZeroCopyReader::new(bytes).is_ok())
            .unwrap();
        assert!(valid);
        samples_ns.push(ws.telemetry_snapshot().last_frame_pack_ns);
    }
    samples_ns.sort_unstable();
    let p99_ns = samples_ns[(samples_ns.len() - 1) * 99 / 100];
    let budget_ns: u64 = if cfg!(debug_assertions) {
        5_000_000 // 5 ms generous ceiling under unoptimized debug builds
    } else {
        500_000 // < 0.5 ms target in release builds
    };
    assert!(
        p99_ns < budget_ns,
        "zero-copy DDSF p99 pack latency {} ns exceeded budget {} ns",
        p99_ns,
        budget_ns
    );

    ws.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t04_just_typed_workspace_ingests_float_int_uint_and_preserves_32bit_snapshots() {
    let guard = TempFileGuard::new("just_typed_replay", "bin");
    let mut raw = Vec::new();
    for _ in 0..16 {
        raw.extend_from_slice(&12.5_f32.to_le_bytes());
        raw.extend_from_slice(&(-42_i32).to_le_bytes());
        raw.extend_from_slice(&0xDEAD_BEEF_u32.to_le_bytes());
        raw.extend_from_slice(&JUSTFLOAT_TAIL_MAGIC);
    }
    std::fs::write(guard.path(), &raw).unwrap();

    let ws = CoreWorkspace::new(Handle::current());
    let src_id = SourceId::new("jt_src");
    let replay_cfg = RawFileReplayConfig::offline(guard.path().to_path_buf());
    let jt_cfg = JustTypedConfig::new(
        src_id.as_str(),
        vec![
            JustTypedChannelSpec::new("vbus", JustTypedChannelKind::Float),
            JustTypedChannelSpec::new("err_cnt", JustTypedChannelKind::Int),
            JustTypedChannelSpec::new("status_u32", JustTypedChannelKind::Uint),
        ],
    );

    ws.create_source(SourceConfig::new(
        src_id.clone(),
        SourceTransportConfig::RawFileReplay(replay_cfg),
        ClockMode::Tier2SoftwarePll {
            nominal_sample_rate_hz: Some(10_000.0),
        },
        SourceProtocolConfig::JustTyped(jt_cfg),
    ))
    .unwrap();

    let ch_vbus = ws.find_channel(&src_id, "vbus").unwrap();
    let ch_err = ws.find_channel(&src_id, "err_cnt").unwrap();
    let ch_stat = ws.find_channel(&src_id, "status_u32").unwrap();
    let extract_scalar = |entry: &ChannelEntry| match &entry.descriptor().kind {
        ChannelKind::Scalar { scalar_type, .. } => Some(*scalar_type),
        _ => None,
    };
    assert_eq!(extract_scalar(&ch_vbus), Some(ScalarType::F32));
    assert_eq!(extract_scalar(&ch_err), Some(ScalarType::I32));
    assert_eq!(extract_scalar(&ch_stat), Some(ScalarType::U32));

    ws.start_source(&src_id).unwrap();
    wait_until(
        || {
            ws.find_channel(&src_id, "status_u32")
                .is_some_and(|e| e.total_samples() >= 16)
        },
        Duration::from_secs(5),
        "just_typed replay decoded >= 16 samples",
    )
    .await;

    let ddsf_bytes = ws.pack_ddsf_frame(host_monotonic_ns()).unwrap();
    let reader = DdsfZeroCopyReader::new(&ddsf_bytes).unwrap();
    let snaps = reader.snapshots().unwrap();

    let snap_val = |cid: u32| -> Option<f64> {
        snaps
            .iter()
            .find(|s| s.channel_num_id == cid)
            .map(|s| s.val_f64)
    };
    assert_eq!(snap_val(ch_vbus.id().0), Some(12.5));
    assert_eq!(snap_val(ch_err.id().0), Some(-42.0));
    assert_eq!(snap_val(ch_stat.id().0), Some(f64::from(0xDEAD_BEEF_u32)));

    ws.shutdown().await.unwrap();
}
