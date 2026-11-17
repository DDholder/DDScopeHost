//! Integration tests for the DDScope Virtual Channel Engine (`N Inputs -> M Outputs` DAG)
//! and Single-Phase Power Analyzer (`SinglePhasePower`) wired into `CoreWorkspace`.

use std::f64::consts::PI;
use std::time::{Duration, Instant};

use ddscope_core::dag::{
    AnalogToDigitalMode, CalculusMode, PowerOutputFlags, StateLabelSpec, StateRangeRule,
    StateValueEntry, ToAnalogMode, ToStateMachineMode, VirtualNodeConfig, VirtualOperatorConfig,
    WindowMetricKind, VIRTUAL_SOURCE_ID,
};
use ddscope_core::ddsf::{DdsfBlockKind, DdsfZeroCopyReader};
use ddscope_core::scope::{
    EdgeSlope, ScopeControllerConfig, TriggerCondition, TriggerRuntimeState, TriggerSweepMode,
};
use ddscope_core::storage::{ChannelEntry, ChannelStore};
use ddscope_core::transport::{host_monotonic_ns, SimConfig};
use ddscope_core::types::{
    ChannelDescriptor, CoreError, ScalarSlice, ScalarType, ScopeId, SourceId, TimestampNs,
};
use ddscope_core::workspace::{CoreWorkspace, ScopeConfig, ScopeTrackBinding, SourceConfig};
use tokio::runtime::Handle;

fn read_all_scalar_samples(entry: &ChannelEntry) -> Vec<(TimestampNs, f64)> {
    match entry.store() {
        ChannelStore::Scalar(lock) => {
            let series = lock.read();
            let start = series.evicted_samples();
            let end = series.total_samples();
            let mut out = Vec::with_capacity((end - start) as usize);
            for i in start..end {
                if let Some(pair) = series.get_sample_by_index(i) {
                    out.push(pair);
                }
            }
            out
        }
        other => panic!("expected scalar channel, got {other:?}"),
    }
}

fn read_all_event_transitions(entry: &ChannelEntry) -> Vec<(TimestampNs, i32)> {
    match entry.store() {
        ChannelStore::Event(lock) => {
            let series = lock.read();
            let start = series.evicted_samples();
            let end = series.total_samples();
            let mut out = Vec::with_capacity((end - start) as usize);
            for i in start..end {
                if let Some(pair) = series.get_transition_by_index(i) {
                    out.push(pair);
                }
            }
            out
        }
        other => panic!("expected event series channel, got {other:?}"),
    }
}

fn register_f32_channel(ws: &CoreWorkspace, source: &str, local: &str) -> std::sync::Arc<ChannelEntry> {
    let mut desc = ChannelDescriptor::new_scalar(SourceId::new(source), local, ScalarType::F32);
    desc.display_name = Some(local.to_string());
    ws.hub().register_or_get(desc).unwrap()
}

fn register_bool_channel(ws: &CoreWorkspace, source: &str, local: &str) -> std::sync::Arc<ChannelEntry> {
    let mut desc = ChannelDescriptor::new_bool(SourceId::new(source), local);
    desc.display_name = Some(local.to_string());
    ws.hub().register_or_get(desc).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signal_type_conversions_end_to_end() {
    let ws = CoreWorkspace::new(Handle::current());
    let src_sig = register_f32_channel(&ws, "rig", "sig");
    let src_b0 = register_bool_channel(&ws, "rig", "b0");
    let src_b1 = register_bool_channel(&ws, "rig", "b1");

    // 1. Analog -> Digital (Schmitt hysteresis & inverted window comparator)
    ws.upsert_virtual_node(VirtualNodeConfig {
        node_id: "a2d_schmitt".into(),
        name: "Schmitt Trigger".into(),
        enabled: true,
        operator: VirtualOperatorConfig::AnalogToDigital {
            input_channel: "rig/sig".into(),
            output_path: "cmp_schmitt".into(),
            mode: AnalogToDigitalMode::Schmitt {
                threshold: 2.5,
                hysteresis: 0.5, // upper >= 3.0 -> high, lower <= 2.0 -> low
                invert: false,
            },
        },
    })
    .unwrap();

    ws.upsert_virtual_node(VirtualNodeConfig {
        node_id: "a2d_win".into(),
        name: "Out-of-Range Window".into(),
        enabled: true,
        operator: VirtualOperatorConfig::AnalogToDigital {
            input_channel: "rig/sig".into(),
            output_path: "cmp_win_inv".into(),
            mode: AnalogToDigitalMode::Window {
                low: 1.0,
                high: 4.0,
                invert: true,
            },
        },
    })
    .unwrap();

    // 2. Analog -> State Machine (RangeMap w/ hysteresis) & Digital -> State Machine (BitCombine)
    ws.upsert_virtual_node(VirtualNodeConfig {
        node_id: "fsm_range".into(),
        name: "Operating Band".into(),
        enabled: true,
        operator: VirtualOperatorConfig::ToStateMachine {
            output_path: "band_fsm".into(),
            mode: ToStateMachineMode::RangeMap {
                input_channel: "rig/sig".into(),
                hysteresis: 0.4,
                rules: vec![
                    StateRangeRule {
                        min: None,
                        max: Some(1.0),
                        code: 1,
                        name: "LOW".into(),
                        color_rgba: Some(0x38bdf8ff),
                    },
                    StateRangeRule {
                        min: Some(1.0),
                        max: Some(4.0),
                        code: 2,
                        name: "NOM".into(),
                        color_rgba: Some(0x34d399ff),
                    },
                    StateRangeRule {
                        min: Some(4.0),
                        max: None,
                        code: 3,
                        name: "OVP".into(),
                        color_rgba: Some(0xfb7185ff),
                    },
                ],
                default_state: StateLabelSpec {
                    code: 0,
                    name: "UNK".into(),
                    color_rgba: None,
                },
            },
        },
    })
    .unwrap();

    ws.upsert_virtual_node(VirtualNodeConfig {
        node_id: "fsm_bits".into(),
        name: "Bit Decode".into(),
        enabled: true,
        operator: VirtualOperatorConfig::ToStateMachine {
            output_path: "bit_state".into(),
            mode: ToStateMachineMode::BitCombine {
                bit_channels: vec!["rig/b0".into(), "rig/b1".into()],
                threshold: 0.5,
                labels: Vec::new(),
            },
        },
    })
    .unwrap();

    // 3. Digital / State -> Analog (LevelMap instant + RC LPF, BitWeightedDac, StateValueMap)
    ws.upsert_virtual_node(VirtualNodeConfig {
        node_id: "d2a_level".into(),
        name: "Level DAC Instant".into(),
        enabled: true,
        operator: VirtualOperatorConfig::ToAnalog {
            output_path: "dac_inst".into(),
            unit: Some("V".into()),
            mode: ToAnalogMode::LevelMap {
                input_channel: "virtual/cmp_schmitt".into(),
                low_value: -3.3,
                high_value: 3.3,
                threshold: 0.5,
                smoothing_tau_ms: 0.0,
            },
        },
    })
    .unwrap();

    ws.upsert_virtual_node(VirtualNodeConfig {
        node_id: "d2a_lpf".into(),
        name: "Level DAC RC".into(),
        enabled: true,
        operator: VirtualOperatorConfig::ToAnalog {
            output_path: "dac_rc".into(),
            unit: Some("V".into()),
            mode: ToAnalogMode::LevelMap {
                input_channel: "rig/b0".into(),
                low_value: 0.0,
                high_value: 5.0,
                threshold: 0.5,
                smoothing_tau_ms: 2.0,
            },
        },
    })
    .unwrap();

    ws.upsert_virtual_node(VirtualNodeConfig {
        node_id: "d2a_bits".into(),
        name: "2-bit Weighted DAC".into(),
        enabled: true,
        operator: VirtualOperatorConfig::ToAnalog {
            output_path: "dac_2bit".into(),
            unit: Some("V".into()),
            mode: ToAnalogMode::BitWeightedDac {
                bit_channels: vec!["rig/b0".into(), "rig/b1".into()],
                scale: 1.5,
                offset: 0.5,
                threshold: 0.5,
            },
        },
    })
    .unwrap();

    ws.upsert_virtual_node(VirtualNodeConfig {
        node_id: "fsm2a".into(),
        name: "FSM Voltage Table".into(),
        enabled: true,
        operator: VirtualOperatorConfig::ToAnalog {
            output_path: "fsm_volts".into(),
            unit: Some("V".into()),
            mode: ToAnalogMode::StateValueMap {
                input_channel: "virtual/band_fsm".into(),
                entries: vec![
                    StateValueEntry { code: 1, value: 10.0 },
                    StateValueEntry { code: 2, value: 24.0 },
                    StateValueEntry { code: 3, value: 48.0 },
                ],
                default_value: 0.0,
            },
        },
    })
    .unwrap();

    // Feed synchronized stimulus across 6 timestamps (1ms step)
    let ts: Vec<TimestampNs> = (1..=6).map(|k| k * 1_000_000).collect();
    // sig crosses above 3.0 at step 2, dips slightly to 2.2 at step 3 (must STAY high under Schmitt 2.5 +/- 0.5),
    // drops below 2.0 at step 4 (0.5 -> LOW), then spikes to 5.0 at step 5 (OVP & outside [1,4]), returns to 2.8 at step 6
    let sig_vals = [0.5_f32, 3.2, 2.2, 0.5, 5.0, 2.8];
    let b0_vals = [false, true, false, true, true, true];
    let b1_vals = [false, false, true, true, false, false];

    src_sig
        .append_scalar_batch(&ts, ScalarSlice::F32(&sig_vals))
        .unwrap();
    src_b0
        .append_scalar_batch(&ts, ScalarSlice::Bool(&b0_vals))
        .unwrap();
    src_b1
        .append_scalar_batch(&ts, ScalarSlice::Bool(&b1_vals))
        .unwrap();

    // Flushing via channel_tree guarantees full DAG pass
    let _tree = ws.channel_tree();
    let virt = SourceId::new(VIRTUAL_SOURCE_ID);

    // Check Schmitt output: [0, 1, 1, 0, 1, 1]
    let schmitt_samples = read_all_scalar_samples(&ws.find_channel(&virt, "cmp_schmitt").unwrap());
    let schmitt_bits: Vec<bool> = schmitt_samples.iter().map(|(_, v)| *v > 0.5).collect();
    assert_eq!(schmitt_bits, vec![false, true, true, false, true, true]);

    // Check inverted Window [1.0, 4.0]: sig = [0.5(out->true), 3.2(in->false), 2.2(false), 0.5(true), 5.0(true), 2.8(false)]
    let win_samples = read_all_scalar_samples(&ws.find_channel(&virt, "cmp_win_inv").unwrap());
    let win_bits: Vec<bool> = win_samples.iter().map(|(_, v)| *v > 0.5).collect();
    assert_eq!(win_bits, vec![true, false, false, true, true, false]);

    // Check RangeMap FSM + StateValueMap chained consumer
    let fsm_entry = ws.find_channel(&virt, "band_fsm").unwrap();
    let fsm_transitions = read_all_event_transitions(&fsm_entry);
    assert!(
        fsm_transitions.iter().any(|&(_, code)| code == 1)
            && fsm_transitions.iter().any(|&(_, code)| code == 2)
            && fsm_transitions.iter().any(|&(_, code)| code == 3),
        "expected LOW(1), NOM(2), OVP(3) state transitions, got {fsm_transitions:?}"
    );
    let fsm_volts = read_all_scalar_samples(&ws.find_channel(&virt, "fsm_volts").unwrap());
    let fsm_volt_vals: Vec<f32> = fsm_volts.iter().map(|(_, v)| *v as f32).collect();
    assert_eq!(fsm_transitions.len(), 5);
    assert_eq!(fsm_volt_vals, vec![10.0, 24.0, 10.0, 48.0, 24.0]);

    // Check BitCombine codes: (b1<<1 | b0) = [0, 1, 2, 3, 1, 1]
    let bit_fsm_entry = ws.find_channel(&virt, "bit_state").unwrap();
    if let ChannelStore::Event(lock) = bit_fsm_entry.store() {
        let series = lock.read();
        let observed: Vec<i32> = ts.iter().map(|&t| series.get_state_at(t).unwrap()).collect();
        assert_eq!(observed, vec![0, 1, 2, 3, 1, 1]);
    } else {
        panic!("bit_state must be EventSeries");
    }

    // Check Level DAC Instant (-3.3V / +3.3V driven by Schmitt output)
    let dac_inst = read_all_scalar_samples(&ws.find_channel(&virt, "dac_inst").unwrap());
    assert!((dac_inst[0].1 - (-3.3)).abs() < 1e-4);
    assert!((dac_inst[1].1 - 3.3).abs() < 1e-4);
    assert!((dac_inst[3].1 - (-3.3)).abs() < 1e-4);

    // Check RC LPF rises smoothly on step from 0 to 5V rather than jumping immediately to 5.0
    let dac_rc = read_all_scalar_samples(&ws.find_channel(&virt, "dac_rc").unwrap());
    assert!((dac_rc[0].1 - 0.0).abs() < 1e-4);
    assert!(
        dac_rc[1].1 > 0.5 && dac_rc[1].1 < 4.5,
        "expected RC step response strictly between 0 and 5V after 1ms with tau=2ms, got {}",
        dac_rc[1].1
    );

    // Check 2-bit Weighted DAC: offset 0.5 + 1.5 * [0, 1, 2, 3, 1, 1] = [0.5, 2.0, 3.5, 5.0, 2.0, 2.0]
    let dac_2bit = read_all_scalar_samples(&ws.find_channel(&virt, "dac_2bit").unwrap());
    let expected_dac = [0.5, 2.0, 3.5, 5.0, 2.0, 2.0];
    for (idx, &exp) in expected_dac.iter().enumerate() {
        assert!(
            (dac_2bit[idx].1 - exp).abs() < 1e-4,
            "dac_2bit[{idx}] = {}, expected {exp}",
            dac_2bit[idx].1
        );
    }

    ws.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn formula_chaining_window_stats_and_calculus() {
    let ws = CoreWorkspace::new(Handle::current());
    let ch_a = register_f32_channel(&ws, "rig", "a");
    let ch_b = register_f32_channel(&ws, "rig", "b");
    let ch_sin = register_f32_channel(&ws, "rig", "sinw");

    // Chained formulas + math builtins + safe zero-division
    ws.upsert_virtual_node(VirtualNodeConfig {
        node_id: "f_mag".into(),
        name: "Vector Mag".into(),
        enabled: true,
        operator: VirtualOperatorConfig::Formula {
            expression: "hypot([rig/a], [rig/b])".into(),
            output_path: "mag".into(),
            unit: None,
            output_digital: false,
        },
    })
    .unwrap();

    ws.upsert_virtual_node(VirtualNodeConfig {
        node_id: "f_chain".into(),
            name: "Chained Ratio & t".into(),
        enabled: true,
        operator: VirtualOperatorConfig::Formula {
            expression: "([virtual/mag] > 4.0) ? ([rig/a] / ([rig/b] - [rig/b])) + [virtual/mag] : t * 100.0".into(),
            output_path: "chain_out".into(),
            unit: None,
            output_digital: false,
        },
    })
    .unwrap();

    // Sliding Window Stats (PeakToPeak & StdDev on ch_sin)
    ws.upsert_virtual_node(VirtualNodeConfig {
        node_id: "w_vpp".into(),
        name: "Vpp".into(),
        enabled: true,
        operator: VirtualOperatorConfig::WindowStats {
            input_channel: "rig/sinw".into(),
            output_path: "sin_vpp".into(),
            unit: Some("V".into()),
            metric: WindowMetricKind::PeakToPeak,
            window_ms: 20.0,
        },
    })
    .unwrap();

    ws.upsert_virtual_node(VirtualNodeConfig {
        node_id: "w_mean_chain".into(),
        name: "Mag Mean".into(),
        enabled: true,
        operator: VirtualOperatorConfig::WindowStats {
            input_channel: "virtual/mag".into(),
            output_path: "mag_mean".into(),
            unit: None,
            metric: WindowMetricKind::Mean,
            window_ms: 50.0,
        },
    })
    .unwrap();

    // Calculus: Derivative of 10*sin(2*pi*50*t) -> peak 10*100*pi ≈ 3141.59 at t=0.02s
    ws.upsert_virtual_node(VirtualNodeConfig {
        node_id: "calc_diff".into(),
        name: "d(sinw)/dt".into(),
        enabled: true,
        operator: VirtualOperatorConfig::Calculus {
            input_channel: "rig/sinw".into(),
            output_path: "sin_deriv".into(),
            unit: Some("V/s".into()),
            mode: CalculusMode::Derivative {
                scale: 1.0,
                filter_tau_ms: 0.0,
            },
        },
    })
    .unwrap();

    // Integral of constant rig/a (=3.0) with dc_offset=1.0 -> integrand = 2.0, clamp [0, 0.05]
    ws.upsert_virtual_node(VirtualNodeConfig {
        node_id: "calc_int".into(),
        name: "Clamped Integral".into(),
        enabled: true,
        operator: VirtualOperatorConfig::Calculus {
            input_channel: "rig/a".into(),
            output_path: "a_int".into(),
            unit: Some("Vs".into()),
            mode: CalculusMode::Integral {
                scale: 1.0,
                dc_offset: 1.0,
                initial_value: 0.0,
                clamp_min: Some(0.0),
                clamp_max: Some(0.05),
            },
        },
    })
    .unwrap();

    // Bad formula syntax must fail cleanly without touching engine state
    let bad_err = ws.upsert_virtual_node(VirtualNodeConfig {
        node_id: "bad_f".into(),
        name: "Bad".into(),
        enabled: true,
        operator: VirtualOperatorConfig::Formula {
            expression: "unknown_fn([rig/a]) + * 3".into(),
            output_path: "bad".into(),
            unit: None,
            output_digital: false,
        },
    });
    assert!(bad_err.is_err());

    // Push 100ms @ 1kHz (101 samples from t=0 to 0.1s)
    let dt_ns: u64 = 1_000_000;
    let n = 101usize;
    let mut ts = Vec::with_capacity(n);
    let mut a_vals = Vec::with_capacity(n);
    let mut b_vals = Vec::with_capacity(n);
    let mut sin_vals = Vec::with_capacity(n);
    for k in 0..n {
        let t_s = (k as f64) * 0.001;
        ts.push(1_000_000_000 + (k as u64) * dt_ns);
        a_vals.push(3.0_f32);
        b_vals.push(4.0_f32); // hypot(3, 4) = 5.0 (> 4.0)
        sin_vals.push((10.0 * (2.0 * PI * 50.0 * t_s).sin()) as f32);
    }

    ch_a.append_scalar_batch(&ts, ScalarSlice::F32(&a_vals)).unwrap();
    ch_b.append_scalar_batch(&ts, ScalarSlice::F32(&b_vals)).unwrap();
    ch_sin.append_scalar_batch(&ts, ScalarSlice::F32(&sin_vals)).unwrap();

    let _ = ws.channel_tree();
    let virt = SourceId::new(VIRTUAL_SOURCE_ID);

    let mag = read_all_scalar_samples(&ws.find_channel(&virt, "mag").unwrap());
    assert_eq!(mag.len(), n);
    assert!((mag.last().unwrap().1 - 5.0).abs() < 1e-4);

    // Chained: mag == 5.0 > 4.0, so a/(b-b) sanitizes div-by-zero to 0, leaving 0 + 5.0 = 5.0
    let chain = read_all_scalar_samples(&ws.find_channel(&virt, "chain_out").unwrap());
    assert!((chain.last().unwrap().1 - 5.0).abs() < 1e-4);

    // Chained downstream WindowStats(Mean) on virtual/mag must equal 5.0
    let mag_mean = read_all_scalar_samples(&ws.find_channel(&virt, "mag_mean").unwrap());
    assert!((mag_mean.last().unwrap().1 - 5.0).abs() < 1e-4);

    // Peak-to-peak of 10*sin(2*pi*50*t) over full 20ms cycle is 20.0V (dt=1ms hits +-10V closely)
    let sin_vpp = read_all_scalar_samples(&ws.find_channel(&virt, "sin_vpp").unwrap());
    assert!(
        (sin_vpp.last().unwrap().1 - 20.0).abs() < 0.25,
        "expected Vpp ≈ 20V, got {}",
        sin_vpp.last().unwrap().1
    );

    // Derivative max near positive zero-crossing matches ~1000*pi ≈ 3141.6 V/s (discrete chord at 1kHz is 2*10*sin(2*pi*50*0.001)/0.001 ≈ 3090 V/s)
    let deriv = read_all_scalar_samples(&ws.find_channel(&virt, "sin_deriv").unwrap());
    let max_d = deriv.iter().map(|(_, v)| *v).fold(0.0_f64, f64::max);
    assert!(
        (max_d - 3090.0).abs() < 100.0,
        "expected derivative peak ~3090 V/s at 1kHz, got {max_d}"
    );

    // Integral of (3.0 - 1.0) over 0.1s would be 0.20, clamped cleanly to 0.05
    let integral = read_all_scalar_samples(&ws.find_channel(&virt, "a_int").unwrap());
    assert!((integral.last().unwrap().1 - 0.05).abs() < 1e-4);

    ws.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn single_phase_power_analyzer_ac_harmonic_and_dc() {
    // Part A: 50 Hz 220Vrms / 10Arms inductive (+30 deg lag)
    let ws = CoreWorkspace::new(Handle::current());
    let u_ch = register_f32_channel(&ws, "ac", "u");
    let i_ch = register_f32_channel(&ws, "ac", "i");

    ws.upsert_virtual_node(VirtualNodeConfig {
        node_id: "pwr_1p".into(),
        name: "Grid 1-Phase".into(),
        enabled: true,
        operator: VirtualOperatorConfig::SinglePhasePower {
            voltage_channel: "ac/u".into(),
            current_channel: "ac/i".into(),
            output_prefix: "pwr".into(),
            voltage_scale: 1.0,
            voltage_offset: 0.0,
            current_scale: 1.0,
            current_offset: 0.0,
            nominal_freq_hz: 50.0,
            zero_cross_hysteresis_v: 1.0,
            min_freq_hz: 5.0,
            fallback_window_ms: 100.0,
            outputs: PowerOutputFlags::default(),
        },
    })
    .unwrap();

    let fs = 10_000.0;
    let dt_ns: u64 = 100_000; // 100 us = 10 kHz
    let n = 2_000usize; // 200 ms = 10 cycles
    let omega = 2.0 * PI * 50.0;
    let u_pk_true = 220.0 * 2.0_f64.sqrt();
    let i_pk_true = 10.0 * 2.0_f64.sqrt();
    let phi = PI / 6.0; // +30 deg lag (inductive load)

    let mut ts = Vec::with_capacity(n);
    let mut u_vals = Vec::with_capacity(n);
    let mut i_vals = Vec::with_capacity(n);
    for k in 0..n {
        let t_s = (k as f64) / fs;
        ts.push(1_000_000_000 + (k as u64) * dt_ns);
        u_vals.push((u_pk_true * (omega * t_s).sin()) as f32);
        i_vals.push((i_pk_true * (omega * t_s - phi).sin()) as f32);
    }
    u_ch.append_scalar_batch(&ts, ScalarSlice::F32(&u_vals)).unwrap();
    i_ch.append_scalar_batch(&ts, ScalarSlice::F32(&i_vals)).unwrap();

    let _ = ws.channel_tree();
    let virt = SourceId::new(VIRTUAL_SOURCE_ID);

    let read_last = |name: &str| -> f64 {
        ws.find_channel(&virt, name)
            .unwrap_or_else(|| panic!("missing virtual/{name}"))
            .latest_value()
            .unwrap()
            .1
    };

    assert!((read_last("pwr.freq") - 50.0).abs() < 0.25);
    assert!((read_last("pwr.u_rms") - 220.0).abs() < 1.0);
    assert!((read_last("pwr.i_rms") - 10.0).abs() < 0.05);
    assert!((read_last("pwr.s") - 2200.0).abs() < 15.0);
    assert!((read_last("pwr.p") - 2200.0 * (PI / 6.0).cos()).abs() < 15.0);
    assert!((read_last("pwr.q1") - 2200.0 * (PI / 6.0).sin()).abs() < 20.0);
    assert!((read_last("pwr.pf") - (PI / 6.0).cos()).abs() < 0.01);
    assert!((read_last("pwr.phi_deg") - 30.0).abs() < 1.0);
    assert!((read_last("pwr.u_pk") - u_pk_true).abs() < 1.5);
    assert!((read_last("pwr.i_pk") - i_pk_true).abs() < 0.1);
    assert!(read_last("pwr.energy_wh") > 0.05);

    // Part B: Capacitive current (+pi/6 lead) & 3rd-harmonic distortion
    ws.upsert_virtual_node(VirtualNodeConfig {
        node_id: "pwr_1p".into(),
        name: "Grid Capacitive+Harm".into(),
        enabled: true,
        operator: VirtualOperatorConfig::SinglePhasePower {
            voltage_channel: "ac2/u".into(),
            current_channel: "ac2/i".into(),
            output_prefix: "pwr2".into(),
            voltage_scale: 1.0,
            voltage_offset: 0.0,
            current_scale: 1.0,
            current_offset: 0.0,
            nominal_freq_hz: 50.0,
            zero_cross_hysteresis_v: 1.0,
            min_freq_hz: 5.0,
            fallback_window_ms: 100.0,
            outputs: PowerOutputFlags::default(),
        },
    })
    .unwrap();
    let u2 = register_f32_channel(&ws, "ac2", "u");
    let i2 = register_f32_channel(&ws, "ac2", "i");
    let mut u2_vals = Vec::with_capacity(n);
    let mut i2_vals = Vec::with_capacity(n);
    for k in 0..n {
        let t_s = (k as f64) / fs;
        u2_vals.push((u_pk_true * (omega * t_s).sin()) as f32);
        // Leading fundamental + strong 3rd harmonic current (creates distortion reactive power Q_tot > |Q1|)
        let i_val = i_pk_true * (omega * t_s + phi).sin() + (5.0 * 2.0_f64.sqrt()) * (3.0 * omega * t_s).sin();
        i2_vals.push(i_val as f32);
    }
    u2.append_scalar_batch(&ts, ScalarSlice::F32(&u2_vals)).unwrap();
    i2.append_scalar_batch(&ts, ScalarSlice::F32(&i2_vals)).unwrap();

    let _ = ws.channel_tree();
    let q1_cap = read_last("pwr2.q1");
    let phi_cap = read_last("pwr2.phi_deg");
    let q_tot = read_last("pwr2.q_tot");
    assert!(q1_cap < -1050.0, "capacitive Q1 must be negative (~ -1100 var), got {q1_cap}");
    assert!(phi_cap < -29.0, "capacitive phase angle must be ~ -30 deg, got {phi_cap}");
    assert!(
        q_tot > q1_cap.abs() + 100.0,
        "3rd harmonic distortion must make total Q ({q_tot}) > |Q1| ({})",
        q1_cap.abs()
    );

    // Part C: Pure DC bus (48V, 5A -> P=240W, freq=0 via fallback window)
    let u_dc = register_f32_channel(&ws, "dc", "u");
    let i_dc = register_f32_channel(&ws, "dc", "i");
    ws.upsert_virtual_node(VirtualNodeConfig {
        node_id: "pwr_dc".into(),
        name: "DC Bus".into(),
        enabled: true,
        operator: VirtualOperatorConfig::SinglePhasePower {
            voltage_channel: "dc/u".into(),
            current_channel: "dc/i".into(),
            output_prefix: "dc_pwr".into(),
            voltage_scale: 1.0,
            voltage_offset: 0.0,
            current_scale: 1.0,
            current_offset: 0.0,
            nominal_freq_hz: 50.0,
            zero_cross_hysteresis_v: 0.5,
            min_freq_hz: 5.0,
            fallback_window_ms: 100.0,
            outputs: PowerOutputFlags::default(),
        },
    })
    .unwrap();
    let dc_ts: Vec<TimestampNs> = (0..300).map(|k| 1_000_000_000 + k * 1_000_000).collect(); // 300ms @ 1kHz
    u_dc.append_scalar_batch(&dc_ts, ScalarSlice::F32(&vec![48.0_f32; 300]))
        .unwrap();
    i_dc.append_scalar_batch(&dc_ts, ScalarSlice::F32(&vec![5.0_f32; 300]))
        .unwrap();
    let _ = ws.channel_tree();

    assert_eq!(read_last("dc_pwr.freq"), 0.0);
    assert!((read_last("dc_pwr.u_rms") - 48.0).abs() < 1e-3);
    assert!((read_last("dc_pwr.i_rms") - 5.0).abs() < 1e-3);
    assert!((read_last("dc_pwr.p") - 240.0).abs() < 1e-2);

    ws.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cycle_rejection_backfill_scope_ddsf_and_live_sim() {
    let ws = CoreWorkspace::new(Handle::current());

    // 1. Reserved "virtual" source ID must be rejected for physical/sim sources
    let reserved_err = ws.create_source(SourceConfig::sim(
        SourceId::new(VIRTUAL_SOURCE_ID),
        SimConfig::default(),
    ));
    assert!(matches!(reserved_err, Err(CoreError::InvalidConfig(_))));

    // 2. Retroactive backfill on pre-populated rig samples before virtual node is even created
    let sig = register_f32_channel(&ws, "rig", "v");
    let ts: Vec<TimestampNs> = (1..=400).map(|k| 2_000_000_000 + k * 1_000_000).collect(); // 400ms
    let vals: Vec<f32> = (1..=400)
        .map(|k| (10.0 * (2.0 * PI * 25.0 * ((k as f64) * 0.001)).sin()) as f32)
        .collect();
    sig.append_scalar_batch(&ts, ScalarSlice::F32(&vals)).unwrap();

    let sum_a = ws
        .upsert_virtual_node(VirtualNodeConfig {
            node_id: "va".into(),
            name: "Gain A".into(),
            enabled: true,
            operator: VirtualOperatorConfig::Formula {
                expression: "[rig/v] * 2.0".into(),
                output_path: "v_gain".into(),
                unit: Some("V".into()),
                output_digital: false,
            },
        })
        .unwrap();
    let orig_ch_id = sum_a.output_channels[0];
    let virt = SourceId::new(VIRTUAL_SOURCE_ID);
    assert_eq!(
        ws.find_channel(&virt, "v_gain").unwrap().total_samples(),
        400,
        "retroactive backfill must process existing ring buffer history immediately"
    );

    // Editing va with same output path & modality preserves ChannelId
    let sum_a_edited = ws
        .upsert_virtual_node(VirtualNodeConfig {
            node_id: "va".into(),
            name: "Gain A Edited".into(),
            enabled: true,
            operator: VirtualOperatorConfig::Formula {
                expression: "[rig/v] * 3.0".into(),
                output_path: "v_gain".into(),
                unit: Some("V".into()),
                output_digital: false,
            },
        })
        .unwrap();
    assert_eq!(sum_a_edited.output_channels[0], orig_ch_id);
    assert!(
        (ws.find_channel(&virt, "v_gain").unwrap().latest_value().unwrap().1
            - (vals[399] as f64) * 3.0)
            .abs()
            < 1e-3
    );

    // 3. Direct self-loop & 2-hop cycle rejection must keep prior DAG intact
    assert!(ws
        .upsert_virtual_node(VirtualNodeConfig {
            node_id: "self_loop".into(),
            name: "Self Loop".into(),
            enabled: true,
            operator: VirtualOperatorConfig::Formula {
                expression: "[virtual/loop_out] + 1.0".into(),
                output_path: "loop_out".into(),
                unit: None,
                output_digital: false,
            },
        })
        .is_err());

    ws.upsert_virtual_node(VirtualNodeConfig {
        node_id: "vb".into(),
        name: "Downstream B".into(),
        enabled: true,
        operator: VirtualOperatorConfig::Formula {
            expression: "[virtual/v_gain] + 1.0".into(),
            output_path: "v_b".into(),
            unit: None,
            output_digital: false,
        },
    })
    .unwrap();

    // Turning va into a consumer of virtual/v_b creates va -> vb -> va cycle and must fail cleanly
    let cycle_err = ws.upsert_virtual_node(VirtualNodeConfig {
        node_id: "va".into(),
        name: "Cycle Attempt".into(),
        enabled: true,
        operator: VirtualOperatorConfig::Formula {
            expression: "[virtual/v_b] * 2.0".into(),
            output_path: "v_gain".into(),
            unit: None,
            output_digital: false,
        },
    });
    assert!(cycle_err.is_err());
    assert_eq!(ws.list_virtual_nodes().len(), 2);

    // 4. Bind virtual analog + digital + state-machine outputs to an MSO scope & pack DDSF v1
    ws.upsert_virtual_node(VirtualNodeConfig {
        node_id: "v_dig".into(),
        name: "Zero Cross".into(),
        enabled: true,
        operator: VirtualOperatorConfig::AnalogToDigital {
            input_channel: "rig/v".into(),
            output_path: "v_zc".into(),
            mode: AnalogToDigitalMode::Schmitt {
                threshold: 0.0,
                hysteresis: 0.5,
                invert: false,
            },
        },
    })
    .unwrap();
    ws.upsert_virtual_node(VirtualNodeConfig {
        node_id: "v_fsm".into(),
        name: "Sign Band".into(),
        enabled: true,
        operator: VirtualOperatorConfig::ToStateMachine {
            output_path: "v_sign".into(),
            mode: ToStateMachineMode::RangeMap {
                input_channel: "rig/v".into(),
                hysteresis: 0.2,
                rules: vec![
                    StateRangeRule {
                        min: None,
                        max: Some(0.0),
                        code: 1,
                        name: "NEG".into(),
                        color_rgba: None,
                    },
                    StateRangeRule {
                        min: Some(0.0),
                        max: None,
                        code: 2,
                        name: "POS".into(),
                        color_rgba: None,
                    },
                ],
                default_state: StateLabelSpec {
                    code: 0,
                    name: "ZERO".into(),
                    color_rgba: None,
                },
            },
        },
    })
    .unwrap();

    let v_gain_id = ws.find_channel(&virt, "v_gain").unwrap().id();
    let v_zc_id = ws.find_channel(&virt, "v_zc").unwrap().id();
    let v_sign_id = ws.find_channel(&virt, "v_sign").unwrap().id();
    let scope_id = ScopeId(42);
    ws.upsert_scope(
        ScopeConfig::new(
            scope_id,
            ScopeControllerConfig {
                scope_id,
                title: Some("Virtual Scope".into()),
                canvas_width_px: 1024,
                time_span_ns: 100_000_000,
                pre_trigger_ratio: 0.25,
                pan_offset_ns: 0,
                sweep_mode: TriggerSweepMode::Single,
                auto_timeout_ns: 50_000_000,
                trigger: Some(TriggerCondition::Edge {
                    channel: v_gain_id,
                    level: 0.0,
                    hysteresis: 0.5,
                    slope: EdgeSlope::Rising,
                }),
                sync_group_id: None,
                paused: false,
            },
        )
        .with_tracks(vec![
            ScopeTrackBinding::analog(v_gain_id),
            ScopeTrackBinding::digital(v_zc_id),
            ScopeTrackBinding::state_ribbon(v_sign_id),
        ])
        .with_width_px(1024),
    )
    .unwrap();

    let ddsf = ws.step_and_pack_ddsf(host_monotonic_ns()).unwrap();
    assert_eq!(
        ws.scope_summary(&scope_id).unwrap().runtime_state,
        TriggerRuntimeState::LatchedPinned
    );
    let reader = DdsfZeroCopyReader::new(&ddsf).unwrap();
    let mut saw_a = false;
    let mut saw_d = false;
    let mut saw_s = false;
    for sc in reader.scopes() {
        for tr in sc.tracks() {
            let hdr = tr.header();
            match DdsfBlockKind::from_u8(hdr.block_kind) {
                Some(DdsfBlockKind::AnalogRawXY | DdsfBlockKind::AnalogM4Column)
                    if hdr.channel_id == v_gain_id.0 =>
                {
                    saw_a = true;
                }
                Some(DdsfBlockKind::DigitalEdges) if hdr.channel_id == v_zc_id.0 => {
                    saw_d = true;
                }
                Some(DdsfBlockKind::StateRibbons) if hdr.channel_id == v_sign_id.0 => {
                    saw_s = true;
                }
                _ => {}
            }
        }
    }
    assert!(saw_a && saw_d && saw_s, "expected analog+digital+state tracks in DDSF frame");

    // 5. Live async Sim source streaming feeds virtual channels automatically
    let sim0 = SourceId::new("sim_live");
    ws.create_source(SourceConfig::sim(
        sim0.clone(),
        SimConfig {
            sample_rate_hz: 5_000.0,
            analog_channels: 2,
            enable_glitch: false,
            paced: true,
            ..SimConfig::default()
        },
    ))
    .unwrap();
    ws.upsert_virtual_node(VirtualNodeConfig {
        node_id: "live_pwr".into(),
        name: "Live Power".into(),
        enabled: true,
        operator: VirtualOperatorConfig::Formula {
            expression: "[sim_live/ch0] * [sim_live/ch1]".into(),
            output_path: "live_p".into(),
            unit: Some("W".into()),
            output_digital: false,
        },
    })
    .unwrap();
    ws.start_source(&sim0).unwrap();

    let live_p = ws.find_channel(&virt, "live_p").unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while live_p.total_samples() < 200 {
        assert!(Instant::now() < deadline, "timed out waiting for live virtual channel samples");
        let _ = ws.step_and_pack_ddsf(host_monotonic_ns()).unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert!(ws.remove_virtual_node("live_pwr").unwrap());
    assert!(ws.clear_virtual_nodes() >= 3);
    assert!(ws.list_virtual_nodes().is_empty());

    ws.shutdown().await.unwrap();
}
