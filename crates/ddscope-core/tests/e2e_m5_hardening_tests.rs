//! Phase 1 Milestone M5 (`T08` / Gate 4) Core Hardening & Invariant Tests:
//! 1. Serial USB/UART hot-plug mid-frame unplug/replug + MCU hardware tick reset recovery.
//! 2. TCP client network disconnect / auto-reconnect propagation and codec reset recovery.
//! 3. 20 s @ 2 MHz (40M pts/ch) macro MSO viewport decimation latency + parity & 1-sample glitch invariants.
//! 4. EventSeries + ScalarSeries multi-round quota rollover with Single-trigger chunk pinning & release.

use std::time::{Duration, Instant};

use tokio::io::AsyncWriteExt;
use tokio::runtime::Handle;

use ddscope_core::clock::{ClockMode, StreamClockSynchronizer};
use ddscope_core::ddsf::{DdsfBlockKind, DdsfZeroCopyReader};
use ddscope_core::protocol::{
    encode_sim_preset_frame, sim_ctrl_state_labels, sim_preset_flags, SimPresetCodec,
    SimPresetConfig,
};
use ddscope_core::scope::{
    EdgeSlope, ScopeControllerConfig, TriggerCondition, TriggerRuntimeState, TriggerSweepMode,
};
use ddscope_core::transport::{
    DuplexTcpNetwork, SerialConfig, SerialTransport, TcpClientConfig, TcpClientTransport,
    TransportRuntimeConfig, VirtualSerialPair,
};
use ddscope_core::types::{
    ChannelDescriptor, ScalarSlice, ScalarType, ScopeId, SourceId, CHUNK_LEN,
};
use ddscope_core::workspace::{
    CoreWorkspace, ScopeConfig, ScopeTrackBinding, SourceRuntimeState, SourceSession,
    SourceSessionComponents,
};

async fn wait_until<F>(mut cond: F, timeout: Duration, label: &str)
where
    F: FnMut() -> bool,
{
    let deadline = Instant::now() + timeout;
    while !cond() {
        assert!(
            Instant::now() < deadline,
            "timed out after {timeout:?} waiting for: {label}"
        );
        tokio::time::sleep(Duration::from_millis(8)).await;
    }
}

fn assert_all_timestamps_monotonic_and_unwrapped(
    ws: &CoreWorkspace,
    source: &str,
    ch_name: &str,
    expected_min_samples: u64,
    max_total_span_ns: u64,
) {
    let entry = ws
        .find_channel(&SourceId::new(source), ch_name)
        .expect("channel should exist");
    let count = entry.total_samples();
    assert!(
        count >= expected_min_samples,
        "{source}/{ch_name}: expected >= {expected_min_samples} samples, got {count}"
    );
    entry
        .with_scalar_read(|series| {
            let mut prev_ts: Option<u64> = None;
            let mut first_ts: Option<u64> = None;
            let mut last_ts = 0_u64;
            for idx in 0..count {
                let (ts, val) = series
                    .get_sample_by_index(idx)
                    .expect("sample index within total_samples");
                assert!(val.is_finite(), "decoded sample {idx} must be finite");
                if first_ts.is_none() {
                    first_ts = Some(ts);
                }
                if let Some(p) = prev_ts {
                    assert!(
                        ts >= p,
                        "non-monotonic timestamp across reconnect at sample {idx}: {ts} < {p}"
                    );
                }
                prev_ts = Some(ts);
                last_ts = ts;
            }
            let span = last_ts.saturating_sub(first_ts.unwrap_or(last_ts));
            assert!(
                span < max_total_span_ns,
                "MCU tick reset caused bogus wrap jump! total span = {span} ns (>= {max_total_span_ns} ns)"
            );
        })
        .expect("scalar channel read");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn m5_serial_hotplug_midframe_and_mcu_tick_reset_recovery() {
    let ws = CoreWorkspace::new(Handle::current());
    let serial_pair = VirtualSerialPair::new("VCOM_M5_HOTPLUG");
    let mut peer1 = serial_pair.take_peer().expect("initial virtual peer");

    let transport = SerialTransport::with_provider(
        SerialConfig {
            port_name: "VCOM_M5_HOTPLUG".into(),
            baud_rate: 2_000_000,
            auto_reconnect: true,
            reconnect_delay_ms: 20,
            ..SerialConfig::default()
        },
        serial_pair.provider(),
    )
    .expect("construct SerialTransport");

    let codec = SimPresetCodec::new(SimPresetConfig {
        source_id: "mcu0".into(),
        analog_channels: 2,
        ..SimPresetConfig::default()
    })
    .expect("construct SimPresetCodec");

    let clock = StreamClockSynchronizer::new(ClockMode::Tier1HardwareCounter {
        tick_hz: 1_000_000.0,
        counter_bits: 32,
    })
    .expect("construct Tier1 clock");

    let session = SourceSession::from_components(
        SourceSessionComponents {
            source_id: SourceId::new("mcu0"),
            display_name: Some("MCU Serial Hot-Plug".into()),
            transport: Box::new(transport),
            clock,
            codec: Box::new(codec),
            runtime_config: TransportRuntimeConfig::default(),
            sim_control: None,
            replay_control: None,
        },
        ws.hub().clone(),
    )
    .expect("build SourceSession");
    ws.register_source_session(session)
        .expect("register mcu0 session");
    ws.start_source(&SourceId::new("mcu0"))
        .expect("start mcu0");

    wait_until(
        || {
            ws.list_sources()
                .iter()
                .any(|s| s.source_id.as_str() == "mcu0" && s.state == SourceRuntimeState::Running)
        },
        Duration::from_secs(2),
        "mcu0 initial Running state",
    )
    .await;

    // Session 1: 20 valid frames (ticks 10_000..30_000), plus 7-byte truncated mid-frame prefix right before unplug.
    let mut pre_buf = Vec::with_capacity(20 * 20 + 7);
    for i in 0..20_u32 {
        encode_sim_preset_frame(
            10_000 + i * 1_000,
            &[12.0 + i as f32 * 0.1, 24.0],
            sim_preset_flags(i % 2 == 0, false, false, false),
            2,
            &mut pre_buf,
        );
    }
    peer1.send_rx(&pre_buf).await.expect("send pre-unplug batch");

    let ch0 = ws
        .find_channel(&SourceId::new("mcu0"), "ch0")
        .expect("mcu0/ch0");
    wait_until(
        || ch0.total_samples() >= 20,
        Duration::from_secs(2),
        "mcu0/ch0 received 20 pre-unplug samples",
    )
    .await;

    let mut half_frame = Vec::new();
    encode_sim_preset_frame(99_999, &[99.0, 99.0], 0, 3, &mut half_frame);
    peer1
        .send_rx(&half_frame[..7])
        .await
        .expect("send truncated half-frame");
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Unplug serial cable mid-stream -> source state propagates Running -> Reconnecting.
    serial_pair.unplug();
    drop(peer1);
    wait_until(
        || {
            ws.list_sources().iter().any(|s| {
                s.source_id.as_str() == "mcu0" && s.state == SourceRuntimeState::Reconnecting
            })
        },
        Duration::from_secs(2),
        "mcu0 transitions to Reconnecting on unplug",
    )
    .await;

    // Replug cable -> Reconnecting -> Running + MCU tick counter resets back to 0!
    let mut peer2 = serial_pair.replug();
    wait_until(
        || {
            ws.list_sources()
                .iter()
                .any(|s| s.source_id.as_str() == "mcu0" && s.state == SourceRuntimeState::Running)
        },
        Duration::from_secs(2),
        "mcu0 transitions back to Running on replug",
    )
    .await;

    let mut post_buf = Vec::with_capacity(20 * 20);
    for i in 0..20_u32 {
        encode_sim_preset_frame(
            i * 1_000, // MCU counter rebooted from 0 (less than pre-unplug 29_000)
            &[42.0 + i as f32 * 0.25, 48.0],
            sim_preset_flags(true, false, false, false),
            2,
            &mut post_buf,
        );
    }
    peer2.send_rx(&post_buf).await.expect("send post-replug batch");

    wait_until(
        || ch0.total_samples() >= 40,
        Duration::from_secs(2),
        "mcu0/ch0 decoded 20 post-replug samples cleanly without framing corruption",
    )
    .await;

    let (_, latest_ch0) = ch0.latest_value().expect("latest ch0");
    assert!(
        (latest_ch0 - 46.75).abs() < 1e-3,
        "expected latest post-reconnect ch0 ~= 46.75, got {latest_ch0}"
    );
    assert_all_timestamps_monotonic_and_unwrapped(&ws, "mcu0", "ch0", 40, 10_000_000_000);

    ws.shutdown().await.expect("workspace shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn m5_tcp_client_auto_reconnect_and_state_propagation() {
    let ws = CoreWorkspace::new(Handle::current());
    let network = DuplexTcpNetwork::new();
    let listen_addr: std::net::SocketAddr = "127.0.0.1:43105".parse().unwrap();
    let mut listener = network
        .bind_raw(listen_addr)
        .await
        .expect("bind virtual TCP listener");

    let transport = TcpClientTransport::with_connector(
        TcpClientConfig {
            remote_addr: listen_addr.to_string(),
            auto_reconnect: true,
            tcp_nodelay: true,
            reconnect_delay_ms: 20,
        },
        network.connector(),
    )
    .expect("construct TcpClientTransport");

    let codec = SimPresetCodec::new(SimPresetConfig {
        source_id: "tcp0".into(),
        analog_channels: 2,
        ..SimPresetConfig::default()
    })
    .expect("SimPresetCodec");

    let clock = StreamClockSynchronizer::new(ClockMode::Tier1HardwareCounter {
        tick_hz: 1_000_000.0,
        counter_bits: 32,
    })
    .expect("Tier1 clock");

    let session = SourceSession::from_components(
        SourceSessionComponents {
            source_id: SourceId::new("tcp0"),
            display_name: Some("TCP Auto-Reconnect".into()),
            transport: Box::new(transport),
            clock,
            codec: Box::new(codec),
            runtime_config: TransportRuntimeConfig::default(),
            sim_control: None,
            replay_control: None,
        },
        ws.hub().clone(),
    )
    .expect("TcpClient SourceSession");
    ws.register_source_session(session).expect("register tcp0");
    ws.start_source(&SourceId::new("tcp0"))
        .expect("start tcp0");

    let mut conn1 = tokio::time::timeout(Duration::from_secs(2), listener.accept())
        .await
        .expect("accept conn1 timeout")
        .expect("accept conn1");

    wait_until(
        || {
            ws.list_sources()
                .iter()
                .any(|s| s.source_id.as_str() == "tcp0" && s.state == SourceRuntimeState::Running)
        },
        Duration::from_secs(2),
        "tcp0 Running after initial connect",
    )
    .await;

    let mut batch1 = Vec::new();
    for i in 0..16_u32 {
        encode_sim_preset_frame(5_000 + i * 1_000, &[11.0, 22.0], 0x01, 1, &mut batch1);
    }
    conn1.writer.write_all(&batch1).await.unwrap();
    conn1.writer.flush().await.unwrap();

    let ch0 = ws
        .find_channel(&SourceId::new("tcp0"), "ch0")
        .expect("tcp0/ch0");
    wait_until(
        || ch0.total_samples() >= 16,
        Duration::from_secs(2),
        "tcp0/ch0 initial 16 samples",
    )
    .await;

    // Force link loss while server temporarily refuses new connections -> verify Reconnecting state.
    network.set_accepting(false);
    drop(conn1);
    network.disconnect_all();
    wait_until(
        || {
            ws.list_sources().iter().any(|s| {
                s.source_id.as_str() == "tcp0" && s.state == SourceRuntimeState::Reconnecting
            })
        },
        Duration::from_secs(2),
        "tcp0 propagates Reconnecting state",
    )
    .await;

    // Restore server listener and verify auto-reconnect -> Running + fresh post-reconnect samples.
    network.set_accepting(true);
    let mut conn2 = tokio::time::timeout(Duration::from_secs(2), listener.accept())
        .await
        .expect("accept conn2 timeout")
        .expect("accept conn2");

    wait_until(
        || {
            ws.list_sources()
                .iter()
                .any(|s| s.source_id.as_str() == "tcp0" && s.state == SourceRuntimeState::Running)
        },
        Duration::from_secs(2),
        "tcp0 returns to Running on reconnect",
    )
    .await;

    let mut batch2 = Vec::new();
    for i in 0..16_u32 {
        encode_sim_preset_frame(i * 1_000, &[42.5, 85.0], 0x03, 2, &mut batch2);
    }
    conn2.writer.write_all(&batch2).await.unwrap();
    conn2.writer.flush().await.unwrap();

    wait_until(
        || ch0.total_samples() >= 32,
        Duration::from_secs(2),
        "tcp0/ch0 post-reconnect samples decoded",
    )
    .await;

    let (_, latest) = ch0.latest_value().expect("latest tcp0/ch0");
    assert!((latest - 42.5).abs() < 1e-3, "expected 42.5, got {latest}");
    assert_all_timestamps_monotonic_and_unwrapped(&ws, "tcp0", "ch0", 32, 10_000_000_000);

    ws.shutdown().await.expect("workspace shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn m5_macro_20s_mso_viewport_latency_and_parity_invariants() {
    let ws = CoreWorkspace::with_quota_bytes(Handle::current(), 512 * 1024 * 1024);
    let hub = ws.hub();

    let ch0_entry = hub
        .register_or_get(ChannelDescriptor::new_scalar(
            "mso0",
            "ch0",
            ScalarType::F32,
        ))
        .expect("register mso0/ch0");
    let pwm_entry = hub
        .register_or_get(ChannelDescriptor::new_bool("mso0", "pwm_a"))
        .expect("register mso0/pwm_a");
    let state_entry = hub
        .register_or_get(ChannelDescriptor::new_state_machine(
            "mso0",
            "ctrl_state",
            sim_ctrl_state_labels(),
        ))
        .expect("register mso0/ctrl_state");

    // 611 * 65_536 = 40_042_496 samples (> 20s @ 2 MHz = 40M samples) per channel.
    const TOTAL_CHUNKS: usize = 611;
    const DT_NS: u64 = 500;
    const T0_NS: u64 = 1_000_000_000_000;
    const GLITCH_SAMPLE_IDX: usize = 35_000_000;

    let mut ts_buf = vec![0_u64; CHUNK_LEN];
    let mut f32_buf = vec![0_f32; CHUNK_LEN];
    let mut bool_buf = vec![false; CHUNK_LEN];

    let dense_chunk_pattern: Vec<bool> = (0..CHUNK_LEN).map(|i| (i / 4) % 2 == 1).collect();
    let base_vals: Vec<f32> = (0..CHUNK_LEN).map(|i| ((i % 256) as f32) * 0.1).collect();

    for c in 0..TOTAL_CHUNKS {
        let base_idx = c * CHUNK_LEN;
        for (i, ts) in ts_buf.iter_mut().enumerate() {
            *ts = T0_NS + ((base_idx + i) as u64) * DT_NS;
        }
        f32_buf.copy_from_slice(&base_vals);
        if base_idx < 30_000_000 {
            bool_buf.copy_from_slice(&dense_chunk_pattern);
        } else {
            bool_buf.fill(false);
        }

        if GLITCH_SAMPLE_IDX >= base_idx && GLITCH_SAMPLE_IDX < base_idx + CHUNK_LEN {
            let local = GLITCH_SAMPLE_IDX - base_idx;
            bool_buf[local] = true;
            f32_buf[local] = 99.5;
        }

        ch0_entry
            .append_scalar_batch(&ts_buf, ScalarSlice::F32(&f32_buf))
            .expect("append ch0");
        pwm_entry
            .append_scalar_batch(&ts_buf, ScalarSlice::Bool(&bool_buf))
            .expect("append pwm_a");
    }

    let t_glitch = T0_NS + (GLITCH_SAMPLE_IDX as u64) * DT_NS;
    state_entry
        .append_state_events(
            &[T0_NS, T0_NS + 2_000_000_000, t_glitch],
            &[0, 2, 3],
        )
        .expect("append state events");

    let last_ts = T0_NS + ((TOTAL_CHUNKS * CHUNK_LEN - 1) as u64) * DT_NS;
    let span_20s = 20_000_000_000_u64;

    ws.upsert_scope(
        ScopeConfig::new(
            ScopeId(1),
            ScopeControllerConfig {
                scope_id: ScopeId(1),
                title: Some("20s Macro MSO".into()),
                canvas_width_px: 1280,
                time_span_ns: span_20s,
                pre_trigger_ratio: 0.5,
                pan_offset_ns: 0,
                sweep_mode: TriggerSweepMode::Roll,
                auto_timeout_ns: 100_000_000,
                trigger: None,
                sync_group_id: None,
                paused: false,
            },
        )
        .with_tracks(vec![
            ScopeTrackBinding::analog(ch0_entry.id()),
            ScopeTrackBinding::digital(pwm_entry.id()),
            ScopeTrackBinding::state_ribbon(state_entry.id()),
        ])
        .with_width_px(1280),
    )
    .expect("upsert 20s macro scope");

    let _ = ws.tick_scopes(last_ts);
    // Warm up one pack cycle, then measure sustained extract+pack latency over 20 frames.
    let _ = ws
        .pack_ddsf_frame_with(last_ts, |b| b.len())
        .expect("warmup pack");

    let mut durations_us = Vec::with_capacity(20);
    let mut last_frame = Vec::new();
    for _ in 0..20 {
        let t0 = Instant::now();
        last_frame = ws
            .pack_ddsf_frame_with(last_ts, |b| b.to_vec())
            .expect("macro MSO pack");
        durations_us.push(t0.elapsed().as_secs_f64() * 1_000_000.0);
    }
    durations_us.sort_by(f64::total_cmp);
    let p99_us = durations_us[(durations_us.len() * 99) / 100];
    let budget_us = if cfg!(debug_assertions) {
        15_000.0
    } else {
        500.0
    };
    assert!(
        p99_us < budget_us,
        "20s (40M pts) MSO viewport extract+pack p99={p99_us:.2}us exceeded budget {budget_us:.2}us"
    );

    let reader = DdsfZeroCopyReader::new(&last_frame).expect("parse macro DDSF");
    let scope_view = reader.scopes().next().expect("scope 1 present");
    let win_start = scope_view.header().win_t_start_ns;
    let glitch_norm = ((t_glitch - win_start) as f64 / span_20s as f64) as f32;

    let mut saw_digital = false;
    let mut saw_analog = false;
    let mut saw_ribbon = false;

    for tr in scope_view.tracks() {
        match DdsfBlockKind::from_u8(tr.header().block_kind) {
            Some(DdsfBlockKind::AnalogM4Column) | Some(DdsfBlockKind::AnalogRawXY) => {
                let stats = tr.stats().expect("analog stats");
                assert!(
                    stats.max > 95.0,
                    "expected 40M M4 decimation to preserve +99.5V glitch spike, got {}",
                    stats.max
                );
                saw_analog = true;
            }
            Some(DdsfBlockKind::DigitalEdges) => {
                let init_level = tr.header().digital_init_level;
                let edges = tr.digital_edges().expect("digital edges");
                assert!(
                    edges.len() <= 4 * 1280 + 4,
                    "decimated edges {} exceeded 4*1280+4 ceiling",
                    edges.len()
                );
                let final_level = init_level ^ ((edges.len() & 1) as u8);
                assert_eq!(
                    final_level, 0,
                    "digital edge decimation broke final level parity! init={init_level}, len={}",
                    edges.len()
                );
                let has_glitch_edge = edges
                    .iter()
                    .any(|&x| (x - glitch_norm).abs() <= 0.0025);
                assert!(
                    has_glitch_edge,
                    "isolated 1-sample digital glitch at x_norm={glitch_norm:.5} was lost!"
                );
                saw_digital = true;
            }
            Some(DdsfBlockKind::StateRibbons) => {
                let ribbons = tr.state_ribbons().expect("state ribbons");
                assert!(
                    ribbons.iter().any(|r| r.state_id == 2)
                        && ribbons.iter().any(|r| r.state_id == 3),
                    "expected CLOSED_LOOP (2) and OCP_FAULT (3) ribbons, got {ribbons:?}"
                );
                let fault = ribbons.iter().find(|r| r.state_id == 3).unwrap();
                assert!(
                    (fault.x0_norm - glitch_norm).abs() <= 0.0025,
                    "OCP_FAULT ribbon x0_norm={} misaligned with glitch_norm={glitch_norm}",
                    fault.x0_norm
                );
                saw_ribbon = true;
            }
            _ => {}
        }
    }

    assert!(
        saw_analog && saw_digital && saw_ribbon,
        "missing MSO lanes: analog={saw_analog}, digital={saw_digital}, ribbon={saw_ribbon}"
    );
    ws.shutdown().await.expect("workspace shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn m5_event_series_and_scalar_multi_round_quota_rollover_with_pinning() {
    const QUOTA_BYTES: usize = 4 * 1024 * 1024;
    let ws = CoreWorkspace::with_quota_bytes(Handle::current(), QUOTA_BYTES);
    let hub = ws.hub();

    let ch0 = hub
        .register_or_get(ChannelDescriptor::new_scalar(
            "roll0",
            "ch0",
            ScalarType::F32,
        ))
        .expect("register roll0/ch0");
    let ctrl_state = hub
        .register_or_get(ChannelDescriptor::new_state_machine(
            "roll0",
            "ctrl_state",
            sim_ctrl_state_labels(),
        ))
        .expect("register roll0/ctrl_state");

    const T_BASE: u64 = 10_000_000_000;
    // Trigger window: 2_000_000 ns (2 ms), 50% pre-trigger -> pins [t_trig - 1ms .. t_trig + 1ms].
    ws.upsert_scope(
        ScopeConfig::new(
            ScopeId(1),
            ScopeControllerConfig {
                scope_id: ScopeId(1),
                title: Some("Pinned Mixed Latch".into()),
                canvas_width_px: 1280,
                time_span_ns: 2_000_000,
                pre_trigger_ratio: 0.5,
                pan_offset_ns: 0,
                sweep_mode: TriggerSweepMode::Single,
                auto_timeout_ns: 100_000_000,
                trigger: Some(TriggerCondition::Edge {
                    channel: ch0.id(),
                    level: 60.0,
                    hysteresis: 2.0,
                    slope: EdgeSlope::Rising,
                }),
                sync_group_id: None,
                paused: false,
            },
        )
        .with_tracks(vec![
            ScopeTrackBinding::analog(ch0.id()),
            ScopeTrackBinding::state_ribbon(ctrl_state.id()),
        ])
        .with_width_px(1280)
        .with_auto_pin_on_latch(true),
    )
    .expect("upsert Single scope");

    // Seed initial batch: prime low at 10V, spike +100V & fault transition 2->3 at T_BASE + 2_000_000, then post-trigger up to +4_500_000.
    let mut init_ts = Vec::with_capacity(4_000);
    let mut init_vals = Vec::with_capacity(4_000);
    for i in 0..4_000_usize {
        let t = T_BASE + (i as u64) * 1_250; // 1.25 us step -> spans +5_000_000 ns
        let v = if i == 1_600 { 100.0 } else { 10.0 }; // index 1600 == T_BASE + 2_000_000 ns
        init_ts.push(t);
        init_vals.push(v);
    }
    let t_trig_expected = T_BASE + 2_000_000;
    ch0.append_scalar_batch(&init_ts, ScalarSlice::F32(&init_vals))
        .expect("append initial scalar");
    ctrl_state
        .append_state_events(
            &[T_BASE + 500_000, t_trig_expected],
            &[2, 3],
        )
        .expect("append initial events");

    let _ = ws.tick_scopes(T_BASE + 5_000_000);
    let latched = ws.scope_summary(&ScopeId(1)).unwrap();
    assert_eq!(
        latched.runtime_state,
        TriggerRuntimeState::LatchedPinned,
        "Scope 1 must latch onto initial +100V spike"
    );
    assert!(latched.is_pinned);
    assert!(latched.pinned_bytes > 0);
    let pinned_trig_ts = latched.trigger_time_ns.unwrap();

    // Push > 2x quota (> 8 MiB) of additional scalar AND > 200_000 state transitions across 30+ batches.
    let mut cursor_ts = T_BASE + 6_000_000;
    let mut scalar_batch_ts = vec![0_u64; CHUNK_LEN];
    let scalar_batch_val = vec![5.0_f32; CHUNK_LEN];
    let mut ev_ts = vec![0_u64; 10_000];
    let ev_codes: Vec<i32> = (0..10_000).map(|i| i % 2).collect();

    for round in 0..24_u64 {
        for (i, ts) in scalar_batch_ts.iter_mut().enumerate() {
            *ts = cursor_ts + (i as u64) * 1_000;
        }
        ch0.append_scalar_batch(&scalar_batch_ts, ScalarSlice::F32(&scalar_batch_val))
            .expect("append rollover scalar");
        for (i, ts) in ev_ts.iter_mut().enumerate() {
            *ts = cursor_ts + (i as u64) * 100;
        }
        ctrl_state
            .append_state_events(&ev_ts, &ev_codes)
            .expect("append rollover events");
        cursor_ts += (CHUNK_LEN as u64) * 1_000;
        if round % 2 == 1 {
            ws.enforce_quota();
        }
    }
    ws.enforce_quota();

    let stats_after_rollover = ws.quota_stats();
    assert!(
        stats_after_rollover.allocated_bytes <= QUOTA_BYTES,
        "allocated {} exceeded quota {}",
        stats_after_rollover.allocated_bytes,
        QUOTA_BYTES
    );
    let ev_evicted = ctrl_state
        .with_event_read(|ev| ev.evicted_samples())
        .unwrap();
    assert!(
        ev_evicted > 0,
        "expected EventSeries to evict oldest unpinned sealed chunks during rollover, got evicted={ev_evicted}"
    );
    assert!(
        ch0.with_scalar_read(|s| s.evicted_samples()).unwrap() > 0,
        "expected ScalarSeries to evict oldest unpinned chunks during rollover"
    );

    // Zoom 10,000x (2 ms -> 200 ns? Use 10_000ns around 200ms/etc, 2_000_000 / 10_000 = 200 ns; use 20_000 ns = 100x or 200 ns = 10_000x!)
    // Use 4_000 ns spanning T_trig +/- 2_000 ns (enough to cover >=1 sample at 1.25us step!)
    ws.set_scope_viewport(&ScopeId(1), 5_000, 0, Some(1280))
        .expect("zoom Scope 1");
    let frame = ws
        .pack_ddsf_frame_with(pinned_trig_ts, |b| b.to_vec())
        .expect("pack zoomed pinned frame");
    let reader = DdsfZeroCopyReader::new(&frame).expect("parse zoomed frame");
    let scope_view = reader.scopes().next().expect("scope 1");
    let mut found_spike = false;
    let mut found_fault_ribbon = false;
    for tr in scope_view.tracks() {
        if tr.header().channel_id == ch0.id().0 {
            if let Some(st) = tr.stats() {
                if st.max > 95.0 {
                    found_spike = true;
                }
            }
        } else if tr.header().channel_id == ctrl_state.id().0 {
            let ribbons = tr.state_ribbons().expect("pinned state ribbons");
            if ribbons.iter().any(|r| r.state_id == 3) {
                found_fault_ribbon = true;
            }
        }
    }
    assert!(found_spike, "pinned +100V analog spike was lost after rollover");
    assert!(
        found_fault_ribbon,
        "pinned OCP_FAULT (3) state ribbon was lost after rollover"
    );

    // Rearm Scope 1 -> all pinned bytes across Scalar + Event channels must release to 0.
    ws.rearm_scope(&ScopeId(1)).expect("rearm Scope 1");
    assert_eq!(
        ws.quota_stats().pinned_bytes,
        0,
        "rearm_scope must release all pinned bytes including EventSeries chunks"
    );
    ws.shutdown().await.expect("workspace shutdown");
}
