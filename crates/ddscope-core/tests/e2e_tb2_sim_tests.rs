use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use ddscope_core::transport::{
    create_sim_transport, host_monotonic_ns, OutboundFrame, RawByteTap, RawTapDirection,
    RxBackpressurePolicy, SimConfig, SimSignalTransport, SimWaveformKind, SimWireFormat,
    TransportKind, TransportRegistry, TransportRuntimeConfig, TransportScheduler, TransportState,
    TxPriority, CTRL_STATE_ALIGN, CTRL_STATE_CLOSED_LOOP, CTRL_STATE_IDLE, CTRL_STATE_OCP_FAULT, JUST_FLOAT_TAIL, SIM_FLAG_FAULT_PIN, SIM_FLAG_GLITCH_ACTIVE,
    SIM_FLAG_PWM_A, SIM_FLAG_PWM_B, SIM_PRESET_MAGIC, SIM_PRESET_TAIL,
};
use ddscope_core::types::{CoreError, TimestampNs};
use parking_lot::Mutex;

#[derive(Debug)]
struct DecodedSimFrame {
    analog_channels: u8,
    flags_bits: u8,
    hw_tick: u32,
    channels: Vec<f32>,
    ctrl_state: u8,
}

impl DecodedSimFrame {
    fn pwm_a(&self) -> bool {
        (self.flags_bits & SIM_FLAG_PWM_A) != 0
    }
    fn pwm_b(&self) -> bool {
        (self.flags_bits & SIM_FLAG_PWM_B) != 0
    }
    fn fault_pin(&self) -> bool {
        (self.flags_bits & SIM_FLAG_FAULT_PIN) != 0
    }
    fn glitch_active(&self) -> bool {
        (self.flags_bits & SIM_FLAG_GLITCH_ACTIVE) != 0
    }
}

fn decode_sim_preset_stream(bytes: &[u8], expected_channels: u8) -> Vec<DecodedSimFrame> {
    let a = expected_channels as usize;
    let frame_len = 12 + 4 * a;
    assert_eq!(
        bytes.len() % frame_len,
        0,
        "stream byte length {} must be an exact multiple of frame_len {}",
        bytes.len(),
        frame_len
    );
    let mut frames = Vec::with_capacity(bytes.len() / frame_len);
    for chunk in bytes.chunks_exact(frame_len) {
        assert_eq!(&chunk[0..2], &SIM_PRESET_MAGIC);
        assert_eq!(chunk[2], expected_channels);
        let flags_bits = chunk[3];
        let hw_tick = u32::from_le_bytes(chunk[4..8].try_into().unwrap());
        let mut channels = Vec::with_capacity(a);
        for i in 0..a {
            let off = 8 + i * 4;
            channels.push(f32::from_le_bytes(chunk[off..off + 4].try_into().unwrap()));
        }
        let ctrl_state = chunk[8 + 4 * a];
        assert_eq!(chunk[9 + 4 * a], 0);
        assert_eq!(&chunk[10 + 4 * a..12 + 4 * a], &SIM_PRESET_TAIL);
        frames.push(DecodedSimFrame {
            analog_channels: expected_channels,
            flags_bits,
            hw_tick,
            channels,
            ctrl_state,
        });
    }
    frames
}

#[derive(Default)]
struct RecordingTap {
    rx_bytes: Mutex<Vec<u8>>,
    tx_count: Mutex<usize>,
}

impl RawByteTap for RecordingTap {
    fn on_raw_bytes(&self, direction: RawTapDirection, _timestamp_ns: TimestampNs, bytes: &Bytes) {
        match direction {
            RawTapDirection::Rx => self.rx_bytes.lock().extend_from_slice(bytes),
            RawTapDirection::Tx => *self.tx_count.lock() += 1,
        }
    }
}

#[test]
fn sim_config_json_compat_and_validation_bounds() {
    let dto_json = r#"{
        "sample_rate_hz": 1000000.0,
        "analog_channels": 4,
        "enable_glitch": true,
        "glitch_period_samples": 100000
    }"#;
    let cfg: SimConfig = serde_json::from_str(dto_json).expect("DTO JSON parses");
    cfg.validate().expect("valid config");

    // Tagged TransportConfigDto::Sim JSON should also deserialize cleanly.
    let tagged_dto = r#"{
        "kind": "sim",
        "sample_rate_hz": 1000.0,
        "analog_channels": 1,
        "enable_glitch": false,
        "glitch_period_samples": 0
    }"#;
    let t_min = create_sim_transport(tagged_dto).expect("1 kHz, 1 ch is valid");
    let caps = t_min.capabilities();
    assert_eq!(caps.kind, TransportKind::Sim);
    assert!(caps.supports_tx);
    assert!(!caps.supports_seek);
    assert!(caps.supports_raw_record);
    assert!(!caps.supports_auto_reconnect);

    let max_cfg = r#"{"sample_rate_hz": 10000000.0, "analog_channels": 16, "enable_glitch": true, "glitch_period_samples": 10}"#;
    create_sim_transport(max_cfg).expect("10 MHz, 16 ch is valid");

    let mut registry = TransportRegistry::new();
    registry.register(TransportKind::Sim, create_sim_transport);
    let via_reg = registry
        .create(TransportKind::Sim, dto_json)
        .expect("registry creates SimSignalTransport");
    assert_eq!(via_reg.capabilities().kind, TransportKind::Sim);

    // Invalid bounds
    for bad_json in [
        r#"{"sample_rate_hz": 999.9, "analog_channels": 4, "enable_glitch": true, "glitch_period_samples": 10}"#,
        r#"{"sample_rate_hz": 10000000.1, "analog_channels": 4, "enable_glitch": true, "glitch_period_samples": 10}"#,
        r#"{"sample_rate_hz": 10000.0, "analog_channels": 0, "enable_glitch": true, "glitch_period_samples": 10}"#,
        r#"{"sample_rate_hz": 10000.0, "analog_channels": 17, "enable_glitch": true, "glitch_period_samples": 10}"#,
        r#"{"sample_rate_hz": 10000.0, "analog_channels": 4, "noise_amplitude": -0.1}"#,
        r#"{not json"#,
    ] {
        let err = create_sim_transport(bad_json).err().expect("must fail");
        assert!(
            matches!(err, CoreError::InvalidConfig(_)),
            "expected InvalidConfig for {bad_json}, got {err:?}"
        );
    }
}

#[tokio::test]
async fn sim_preset_motor_3phase_waveforms_digital_logic_and_state_machine() {
    let cfg = SimConfig {
        sample_rate_hz: 100_000.0,
        analog_channels: 4,
        enable_glitch: true,
        glitch_period_samples: 200,
        wire_format: SimWireFormat::SimPreset,
        noise_amplitude: 0.0,
        glitch_amplitude: 80.0,
        paced: false,
        max_samples: Some(1_200),
        rx_backpressure: RxBackpressurePolicy::Block,
        ..Default::default()
    };
    let transport = SimSignalTransport::new(cfg).unwrap();
    let control = transport.control_handle();
    let mut scheduler =
        TransportScheduler::new(Box::new(transport), TransportRuntimeConfig::default());

    let rx = scheduler.start().unwrap();
    let collected = tokio::task::spawn_blocking(move || {
        let mut all = Vec::new();
        while let Ok(pkt) = rx.recv_timeout(Duration::from_secs(3)) {
            all.extend_from_slice(&pkt.bytes);
        }
        all
    })
    .await
    .unwrap();
    scheduler.stop().await.unwrap();

    assert_eq!(control.emitted_samples(), 1_200);
    assert_eq!(control.injected_glitches(), 6);

    let frames = decode_sim_preset_stream(&collected, 4);
    assert_eq!(frames.len(), 1_200);

    let mut seen_states = [false; 4];
    let mut glitch_indices = Vec::new();
    let mut saw_pwm_a_high = false;
    let mut saw_pwm_b_high = false;

    for (i, f) in frames.iter().enumerate() {
        assert_eq!(f.analog_channels, 4);
        assert_eq!(f.hw_tick, i as u32);
        seen_states[f.ctrl_state as usize] = true;

        // Balanced 3-phase motor currents sum to ~0 with 0 noise.
        let sum_3ph = f.channels[0] + f.channels[1] + f.channels[2];
        assert!(
            sum_3ph.abs() < 1e-3,
            "sample {i}: 3-phase currents sum={sum_3ph}"
        );

        let expected_glitch = (i + 1) % 200 == 0;
        assert_eq!(f.glitch_active(), expected_glitch, "sample {i}");
        if expected_glitch {
            glitch_indices.push(i);
            assert!((f.channels[3] - 104.0).abs() < 1e-3);
            assert!(f.fault_pin());
        } else {
            assert!((f.channels[3] - 24.0).abs() < 1e-3);
        }

        match f.ctrl_state {
            CTRL_STATE_IDLE => {
                assert!(!f.pwm_a());
                assert!(!f.pwm_b());
                assert_eq!(f.fault_pin(), expected_glitch);
            }
            CTRL_STATE_ALIGN => {
                assert!(f.pwm_a());
                assert!(!f.pwm_b());
                assert_eq!(f.fault_pin(), expected_glitch);
            }
            CTRL_STATE_CLOSED_LOOP => {
                assert_ne!(f.pwm_a(), f.pwm_b());
                saw_pwm_a_high |= f.pwm_a();
                saw_pwm_b_high |= f.pwm_b();
                assert_eq!(f.fault_pin(), expected_glitch);
            }
            CTRL_STATE_OCP_FAULT => {
                assert!(!f.pwm_a());
                assert!(!f.pwm_b());
                assert!(f.fault_pin());
            }
            other => panic!("unexpected ctrl_state {other}"),
        }
    }

    assert_eq!(seen_states, [true, true, true, true]);
    assert!(saw_pwm_a_high && saw_pwm_b_high);
    assert_eq!(glitch_indices, vec![199, 399, 599, 799, 999, 1199]);
}

#[tokio::test]
async fn sim_preset_channel_counts_and_waveform_kinds() {
    // 1-channel and 3-channel presets place bus voltage + glitch on ch{A-1}.
    for a in [1_u8, 3_u8, 16_u8] {
        let cfg = SimConfig {
            sample_rate_hz: 100_000.0,
            analog_channels: a,
            enable_glitch: true,
            glitch_period_samples: 25,
            noise_amplitude: 0.0,
            glitch_amplitude: 50.0,
            paced: false,
            max_samples: Some(60),
            rx_backpressure: RxBackpressurePolicy::Block,
            ..Default::default()
        };
        let mut sched = TransportScheduler::new(
            Box::new(SimSignalTransport::new(cfg).unwrap()),
            TransportRuntimeConfig::default(),
        );
        let rx = sched.start().unwrap();
        let bytes = tokio::task::spawn_blocking(move || {
            let mut v = Vec::new();
            while let Ok(pkt) = rx.recv_timeout(Duration::from_secs(2)) {
                v.extend_from_slice(&pkt.bytes);
            }
            v
        })
        .await
        .unwrap();
        sched.stop().await.unwrap();

        let frames = decode_sim_preset_stream(&bytes, a);
        assert_eq!(frames.len(), 60);
        let glitch_ch = if a >= 4 { 3 } else { (a - 1) as usize };
        assert!((frames[24].channels[glitch_ch] - 74.0).abs() < 1e-3);
        assert!((frames[25].channels[glitch_ch] - 24.0).abs() < 1e-3);
    }

    // Explicit waveform kinds: Sine, Square, Triangle, WhiteNoise
    let run_custom = |seed: u64| async move {
        let cfg = SimConfig {
            sample_rate_hz: 50_000.0,
            analog_channels: 4,
            enable_glitch: false,
            glitch_period_samples: 0,
            channel_waveforms: vec![
                SimWaveformKind::Sine,
                SimWaveformKind::Square,
                SimWaveformKind::Triangle,
                SimWaveformKind::WhiteNoise,
            ],
            noise_amplitude: 0.25,
            paced: false,
            max_samples: Some(400),
            seed,
            rx_backpressure: RxBackpressurePolicy::Block,
            ..Default::default()
        };
        let mut sched = TransportScheduler::new(
            Box::new(SimSignalTransport::new(cfg).unwrap()),
            TransportRuntimeConfig::default(),
        );
        let rx = sched.start().unwrap();
        let bytes = tokio::task::spawn_blocking(move || {
            let mut v = Vec::new();
            while let Ok(pkt) = rx.recv_timeout(Duration::from_secs(2)) {
                v.extend_from_slice(&pkt.bytes);
            }
            v
        })
        .await
        .unwrap();
        sched.stop().await.unwrap();
        decode_sim_preset_stream(&bytes, 4)
    };

    let f_a = run_custom(777).await;
    let f_b = run_custom(777).await;
    assert_eq!(f_a.len(), 400);
    for (x, y) in f_a.iter().zip(f_b.iter()) {
        assert_eq!(x.channels, y.channels);
    }

    let min_sine = f_a.iter().map(|f| f.channels[0]).fold(f32::INFINITY, f32::min);
    let max_sine = f_a.iter().map(|f| f.channels[0]).fold(f32::NEG_INFINITY, f32::max);
    assert!(min_sine < -8.0 && max_sine > 8.0);

    let min_sq = f_a.iter().map(|f| f.channels[1]).fold(f32::INFINITY, f32::min);
    let max_sq = f_a.iter().map(|f| f.channels[1]).fold(f32::NEG_INFINITY, f32::max);
    assert!(min_sq < -4.5 && max_sq > 4.5);

    let min_tri = f_a.iter().map(|f| f.channels[2]).fold(f32::INFINITY, f32::min);
    let max_tri = f_a.iter().map(|f| f.channels[2]).fold(f32::NEG_INFINITY, f32::max);
    assert!(min_tri < -4.0 && max_tri > 4.0);

    let noise_vals: Vec<f32> = f_a.iter().map(|f| f.channels[3]).collect();
    assert!(noise_vals.iter().any(|&v| v > 0.05) && noise_vals.iter().any(|&v| v < -0.05));
}

#[tokio::test]
async fn just_float_and_firewater_csv_exact_encoding() {
    // JustFloat
    let jf_cfg = SimConfig {
        sample_rate_hz: 100_000.0,
        analog_channels: 4,
        enable_glitch: true,
        glitch_period_samples: 100,
        wire_format: SimWireFormat::JustFloat,
        noise_amplitude: 0.0,
        glitch_amplitude: 80.0,
        paced: false,
        max_samples: Some(500),
        rx_backpressure: RxBackpressurePolicy::Block,
        ..Default::default()
    };
    let mut jf_sched = TransportScheduler::new(
        Box::new(SimSignalTransport::new(jf_cfg).unwrap()),
        TransportRuntimeConfig::default(),
    );
    let rx = jf_sched.start().unwrap();
    let jf_bytes = tokio::task::spawn_blocking(move || {
        let mut v = Vec::new();
        while let Ok(pkt) = rx.recv_timeout(Duration::from_secs(2)) {
            v.extend_from_slice(&pkt.bytes);
        }
        v
    })
    .await
    .unwrap();
    jf_sched.stop().await.unwrap();

    assert_eq!(jf_bytes.len(), 500 * 20);
    for (idx, frame) in jf_bytes.chunks_exact(20).enumerate() {
        assert_eq!(&frame[16..20], &JUST_FLOAT_TAIL);
        let ch3 = f32::from_le_bytes(frame[12..16].try_into().unwrap());
        let expected = if (idx + 1) % 100 == 0 { 104.0 } else { 24.0 };
        assert!((ch3 - expected).abs() < 1e-3, "JustFloat idx={idx} ch3={ch3}");
    }

    // FireWaterCsv
    let fw_cfg = SimConfig {
        sample_rate_hz: 100_000.0,
        analog_channels: 4,
        enable_glitch: true,
        glitch_period_samples: 50,
        wire_format: SimWireFormat::FireWaterCsv,
        noise_amplitude: 0.0,
        glitch_amplitude: 80.0,
        paced: false,
        max_samples: Some(300),
        rx_backpressure: RxBackpressurePolicy::Block,
        ..Default::default()
    };
    let mut fw_sched = TransportScheduler::new(
        Box::new(SimSignalTransport::new(fw_cfg).unwrap()),
        TransportRuntimeConfig::default(),
    );
    let rx = fw_sched.start().unwrap();
    let fw_bytes = tokio::task::spawn_blocking(move || {
        let mut v = Vec::new();
        while let Ok(pkt) = rx.recv_timeout(Duration::from_secs(2)) {
            v.extend_from_slice(&pkt.bytes);
        }
        v
    })
    .await
    .unwrap();
    fw_sched.stop().await.unwrap();

    let text = std::str::from_utf8(&fw_bytes).expect("FireWaterCsv is valid UTF-8");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 300);
    for (idx, line) in lines.iter().enumerate() {
        let cols: Vec<f32> = line
            .split(',')
            .map(|s| s.parse::<f32>().expect("valid float token"))
            .collect();
        assert_eq!(cols.len(), 4);
        let expected = if (idx + 1) % 50 == 0 { 104.0 } else { 24.0 };
        assert!((cols[3] - expected).abs() < 1e-3, "FireWater idx={idx} col3={}", cols[3]);
    }
}

#[tokio::test]
async fn manual_glitch_and_tx_commands_via_transport_scheduler() {
    let cfg = SimConfig {
        sample_rate_hz: 20_000.0,
        analog_channels: 4,
        enable_glitch: false,
        glitch_period_samples: 0,
        noise_amplitude: 0.0,
        glitch_amplitude: 80.0,
        paced: true,
        max_samples: None,
        rx_backpressure: RxBackpressurePolicy::DropNewest,
        ..Default::default()
    };
    let transport = SimSignalTransport::new(cfg).unwrap();
    let control = transport.control_handle();
    let mut scheduler =
        TransportScheduler::new(Box::new(transport), TransportRuntimeConfig::default());
    let tap = Arc::new(RecordingTap::default());
    scheduler.set_raw_tap(Some(tap.clone())).unwrap();
    assert_eq!(scheduler.state(), TransportState::Stopped);

    let rx = scheduler.start().unwrap();
    assert_eq!(scheduler.state(), TransportState::Running);

    // 1. Inject 3 manual single-sample glitches via control handle.
    control.inject_manual_glitch(3);
    let mut collected = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(3);
    while control.injected_glitches() < 3 && Instant::now() < deadline {
        while let Ok(pkt) = rx.try_recv() {
            collected.extend_from_slice(&pkt.bytes);
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // Drain one more cycle to ensure all 3 glitches are in `collected`.
    tokio::time::sleep(Duration::from_millis(10)).await;
    while let Ok(pkt) = rx.try_recv() {
        collected.extend_from_slice(&pkt.bytes);
    }
    let frames = decode_sim_preset_stream(&collected, 4);
    let manual_indices: Vec<usize> = frames
        .iter()
        .enumerate()
        .filter_map(|(i, f)| f.glitch_active().then_some(i))
        .collect();
    assert_eq!(manual_indices.len(), 3, "expected 3 manual glitches");
    // Each manual glitch must be strictly 1 sample wide (no adjacent glitch samples).
    for win in manual_indices.windows(2) {
        assert!(win[1] - win[0] >= 2, "glitches must be isolated: {manual_indices:?}");
    }

    // 2. Send TX commands through TransportScheduler: RATE=, STATE=, GLITCH, and invalid cmds.
    scheduler
        .submit_tx(OutboundFrame::fire_and_forget(
            Bytes::from_static(b"RATE=50000\nSTATE=3\nGLITCH\n"),
            TxPriority::High,
        ))
        .unwrap();
    scheduler
        .submit_tx(OutboundFrame::fire_and_forget(
            Bytes::from_static(b"RATE=500\nSTATE=99\n"),
            TxPriority::Normal,
        ))
        .unwrap();

    let wait_tx = Instant::now() + Duration::from_secs(3);
    while (control.sample_rate_hz() != 50_000.0 || control.injected_glitches() < 4)
        && Instant::now() < wait_tx
    {
        while rx.try_recv().is_ok() {}
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(control.sample_rate_hz(), 50_000.0);
    assert!(control.injected_glitches() >= 4);

    // Drain any buffered packets before verifying STATE=3 takes effect cleanly.
    while rx.try_recv().is_ok() {}
    let next_pkt = tokio::task::spawn_blocking({
        let rx = rx.clone();
        move || rx.recv_timeout(Duration::from_secs(2))
    })
    .await
    .unwrap()
    .expect("post-STATE packet arrives");
    let post_frames = decode_sim_preset_stream(&next_pkt.bytes, 4);
    assert!(!post_frames.is_empty());
    let last = post_frames.last().unwrap();
    assert_eq!(last.ctrl_state, CTRL_STATE_OCP_FAULT);
    assert!(last.fault_pin());
    assert!(!last.pwm_a() && !last.pwm_b());

    scheduler.stop().await.unwrap();
    assert_eq!(scheduler.state(), TransportState::Stopped);
    assert!(*tap.tx_count.lock() >= 2);
}

#[tokio::test]
async fn paced_mode_respects_1khz_and_10mhz_rates() {
    // 1 kHz paced test: ~100 ms should emit roughly 50..=250 samples (never millions).
    let cfg_1k = SimConfig {
        sample_rate_hz: 1_000.0,
        analog_channels: 4,
        enable_glitch: false,
        glitch_period_samples: 0,
        paced: true,
        max_samples: None,
        ..Default::default()
    };
    let t_1k = SimSignalTransport::new(cfg_1k).unwrap();
    let c_1k = t_1k.control_handle();
    let mut sched_1k =
        TransportScheduler::new(Box::new(t_1k), TransportRuntimeConfig::default());
    let rx_1k = sched_1k.start().unwrap();
    let drain_1k = tokio::task::spawn_blocking(move || {
        let mut total_bytes = 0usize;
        while let Ok(pkt) = rx_1k.recv_timeout(Duration::from_millis(250)) {
            total_bytes += pkt.bytes.len();
        }
        total_bytes
    });
    tokio::time::sleep(Duration::from_millis(120)).await;
    sched_1k.stop().await.unwrap();
    let bytes_1k = drain_1k.await.unwrap();
    let samples_1k = c_1k.emitted_samples();
    assert!(
        (30..=300).contains(&samples_1k),
        "1 kHz over ~120ms emitted {samples_1k} samples"
    );
    assert_eq!(bytes_1k as u64, samples_1k * 28);

    // 10 MHz paced test with finite max_samples: 500_000 samples = 50 ms nominal.
    let cfg_10m = SimConfig {
        sample_rate_hz: 10_000_000.0,
        analog_channels: 4,
        enable_glitch: true,
        glitch_period_samples: 100_000,
        paced: true,
        max_samples: Some(500_000),
        rx_backpressure: RxBackpressurePolicy::Block,
        ..Default::default()
    };
    let t_10m = SimSignalTransport::new(cfg_10m).unwrap();
    let c_10m = t_10m.control_handle();
    let mut sched_10m =
        TransportScheduler::new(Box::new(t_10m), TransportRuntimeConfig::default());
    let t0 = Instant::now();
    let rx_10m = sched_10m.start().unwrap();
    let total_bytes = tokio::task::spawn_blocking(move || {
        let mut sum = 0usize;
        while let Ok(pkt) = rx_10m.recv_timeout(Duration::from_secs(3)) {
            sum += pkt.bytes.len();
        }
        sum
    })
    .await
    .unwrap();
    let elapsed = t0.elapsed();
    sched_10m.stop().await.unwrap();

    assert_eq!(c_10m.emitted_samples(), 500_000);
    assert_eq!(c_10m.injected_glitches(), 5);
    assert_eq!(total_bytes, 500_000 * 28);
    assert!(
        elapsed >= Duration::from_millis(30) && elapsed < Duration::from_secs(2),
        "10 MHz paced 500k samples took {elapsed:?}"
    );
}

#[tokio::test]
async fn slab_reclamation_and_unpaced_high_rate_throughput() {
    let target_samples: u64 = if cfg!(debug_assertions) {
        2_000_000
    } else {
        10_000_000
    };
    let cfg = SimConfig {
        sample_rate_hz: 10_000_000.0,
        analog_channels: 4,
        enable_glitch: true,
        glitch_period_samples: 100_000,
        paced: false,
        max_samples: Some(target_samples),
        rx_backpressure: RxBackpressurePolicy::Block,
        ..Default::default()
    };
    let transport = SimSignalTransport::new(cfg).unwrap();
    let control = transport.control_handle();
    let mut scheduler =
        TransportScheduler::new(Box::new(transport), TransportRuntimeConfig::default());

    let start = Instant::now();
    let rx = scheduler.start().unwrap();
    let (total_packets, total_bytes, prev_mono) = tokio::task::spawn_blocking(move || {
        let mut pkts = 0u64;
        let mut bytes = 0u64;
        let mut last_ts = host_monotonic_ns();
        let mut mono_ok = true;
        while let Ok(pkt) = rx.recv_timeout(Duration::from_secs(10)) {
            pkts += 1;
            bytes += pkt.bytes.len() as u64;
            if pkt.host_recv_ns < last_ts {
                // Allow initial timestamp comparison only against first packet.
                if pkts > 1 {
                    mono_ok = false;
                }
            }
            last_ts = pkt.host_recv_ns;
            // Validate packet header/tail integrity on first and last frame of each slab.
            assert!(pkt.bytes.len() >= 28 && pkt.bytes.len() % 28 == 0);
            assert_eq!(&pkt.bytes[0..2], &SIM_PRESET_MAGIC);
            assert_eq!(&pkt.bytes[pkt.bytes.len() - 2..], &SIM_PRESET_TAIL);
            drop(pkt);
        }
        (pkts, bytes, mono_ok)
    })
    .await
    .unwrap();
    let elapsed = start.elapsed();
    scheduler.stop().await.unwrap();

    assert!(prev_mono, "host_recv_ns must be monotonic");
    assert_eq!(control.emitted_samples(), target_samples);
    assert_eq!(total_bytes, target_samples * 28);
    assert!(total_packets > 100);

    let pool_stats = control.pool_stats();
    assert_eq!(
        pool_stats.fallback_allocations, 0,
        "expected zero fallback allocations when consumer drops packets: {pool_stats:?}"
    );
    assert!(
        pool_stats.slabs_reclaimed > 10,
        "expected slab reclamation across multi-MB stream: {pool_stats:?}"
    );

    let frames_per_sec = target_samples as f64 / elapsed.as_secs_f64();
    eprintln!(
        "SimSignalTransport unpaced throughput: {:.2}M frames/sec ({} frames in {:?}, reclaimed {} slabs)",
        frames_per_sec / 1_000_000.0,
        target_samples,
        elapsed,
        pool_stats.slabs_reclaimed
    );
    if !cfg!(debug_assertions) {
        assert!(
            frames_per_sec > 5_000_000.0,
            "release unpaced throughput {:.0} frames/sec must exceed 5M frames/sec",
            frames_per_sec
        );
    }
}
