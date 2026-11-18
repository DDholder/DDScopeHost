//! Declarative virtual channel node configuration, validation, and output descriptor expansion.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use ddscope_types::{
    ChannelDescriptor, ChannelId, CoreError, CoreResult, ScalarType, SourceId, StateEnumMap,
    StateLabel,
};

use super::formula::CompiledFormula;

/// Canonical virtual source identifier under which all derived virtual channels live (`virtual/<path>`).
pub const VIRTUAL_SOURCE_ID: &str = "virtual";

/// Complete configuration for a single virtual DAG operator node (`N Inputs -> M Outputs`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VirtualNodeConfig {
    pub node_id: String,
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub operator: VirtualOperatorConfig,
}

fn default_true() -> bool {
    true
}

/// Summary returned by [`crate::VirtualDagEngine`] and `CoreWorkspace` upon node registration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VirtualNodeSummary {
    pub node_id: String,
    pub name: String,
    pub enabled: bool,
    pub output_channels: Vec<ChannelId>,
    pub output_paths: Vec<String>,
}

/// Supported virtual operator definitions.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VirtualOperatorConfig {
    AnalogToDigital {
        input_channel: String,
        output_path: String,
        mode: AnalogToDigitalMode,
    },
    ToStateMachine {
        output_path: String,
        mode: ToStateMachineMode,
    },
    ToAnalog {
        output_path: String,
        #[serde(default)]
        unit: Option<String>,
        mode: ToAnalogMode,
    },
    Formula {
        expression: String,
        output_path: String,
        #[serde(default)]
        unit: Option<String>,
        #[serde(default)]
        output_digital: bool,
    },
    WindowStats {
        input_channel: String,
        output_path: String,
        #[serde(default)]
        unit: Option<String>,
        metric: WindowMetricKind,
        window_ms: f64,
    },
    Calculus {
        input_channel: String,
        output_path: String,
        #[serde(default)]
        unit: Option<String>,
        mode: CalculusMode,
    },
    SinglePhasePower {
        voltage_channel: String,
        current_channel: String,
        output_prefix: String,
        #[serde(default = "default_one")]
        voltage_scale: f64,
        #[serde(default)]
        voltage_offset: f64,
        #[serde(default = "default_one")]
        current_scale: f64,
        #[serde(default)]
        current_offset: f64,
        #[serde(default = "default_nominal_freq_hz")]
        nominal_freq_hz: f64,
        #[serde(default)]
        zero_cross_hysteresis_v: f64,
        #[serde(default = "default_min_freq_hz")]
        min_freq_hz: f64,
        #[serde(default = "default_fallback_window_ms")]
        fallback_window_ms: f64,
        #[serde(default)]
        outputs: PowerOutputFlags,
    },
}

fn default_one() -> f64 {
    1.0
}

fn default_nominal_freq_hz() -> f64 {
    50.0
}

fn default_min_freq_hz() -> f64 {
    5.0
}

fn default_fallback_window_ms() -> f64 {
    100.0
}

/// Threshold/window modes for Analog -> Digital conversion (`Bool` output).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum AnalogToDigitalMode {
    Schmitt {
        threshold: f64,
        hysteresis: f64,
        #[serde(default)]
        invert: bool,
    },
    Window {
        low: f64,
        high: f64,
        #[serde(default)]
        invert: bool,
    },
}

/// Display metadata entry for a discrete state code.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateLabelSpec {
    pub code: i32,
    pub name: String,
    #[serde(default)]
    pub color_rgba: Option<u32>,
}

/// Numeric interval-to-state mapping rule.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StateRangeRule {
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    pub code: i32,
    pub name: String,
    #[serde(default)]
    pub color_rgba: Option<u32>,
}

/// Conversion modes for Analog / Digital -> State Machine (`EventSeries`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ToStateMachineMode {
    RangeMap {
        input_channel: String,
        #[serde(default)]
        hysteresis: f64,
        rules: Vec<StateRangeRule>,
        default_state: StateLabelSpec,
    },
    BitCombine {
        bit_channels: Vec<String>,
        #[serde(default = "default_bit_threshold")]
        threshold: f64,
        #[serde(default)]
        labels: Vec<StateLabelSpec>,
    },
}

fn default_bit_threshold() -> f64 {
    0.5
}

/// Numeric value assignment for a discrete state code.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StateValueEntry {
    pub code: i32,
    pub value: f64,
}

/// Conversion modes for Digital / State -> Analog (`Scalar F32`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ToAnalogMode {
    LevelMap {
        input_channel: String,
        low_value: f64,
        high_value: f64,
        #[serde(default = "default_bit_threshold")]
        threshold: f64,
        #[serde(default)]
        smoothing_tau_ms: f64,
    },
    BitWeightedDac {
        bit_channels: Vec<String>,
        #[serde(default = "default_one")]
        scale: f64,
        #[serde(default)]
        offset: f64,
        #[serde(default = "default_bit_threshold")]
        threshold: f64,
    },
    StateValueMap {
        input_channel: String,
        entries: Vec<StateValueEntry>,
        #[serde(default)]
        default_value: f64,
    },
}

/// Supported sliding time-window metrics.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowMetricKind {
    Rms,
    Mean,
    PeakToPeak,
    Min,
    Max,
    StdDev,
}

/// Calculus operator variants (`d/dt` and trapezoidal integral).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum CalculusMode {
    Derivative {
        #[serde(default = "default_one")]
        scale: f64,
        #[serde(default)]
        filter_tau_ms: f64,
    },
    Integral {
        #[serde(default = "default_one")]
        scale: f64,
        #[serde(default)]
        dc_offset: f64,
        #[serde(default)]
        initial_value: f64,
        #[serde(default)]
        clamp_min: Option<f64>,
        #[serde(default)]
        clamp_max: Option<f64>,
    },
}

/// Output channel selection flags for [`VirtualOperatorConfig::SinglePhasePower`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PowerOutputFlags {
    #[serde(default = "default_true")]
    pub freq: bool,
    #[serde(default = "default_true")]
    pub u_rms: bool,
    #[serde(default = "default_true")]
    pub i_rms: bool,
    #[serde(default = "default_true")]
    pub p: bool,
    #[serde(default = "default_true")]
    pub q1: bool,
    #[serde(default = "default_true")]
    pub q_tot: bool,
    #[serde(default = "default_true")]
    pub s: bool,
    #[serde(default = "default_true")]
    pub pf: bool,
    #[serde(default = "default_true")]
    pub phi_deg: bool,
    #[serde(default = "default_true")]
    pub u_pk: bool,
    #[serde(default = "default_true")]
    pub i_pk: bool,
    #[serde(default = "default_true")]
    pub energy_wh: bool,
}

impl Default for PowerOutputFlags {
    fn default() -> Self {
        Self {
            freq: true,
            u_rms: true,
            i_rms: true,
            p: true,
            q1: true,
            q_tot: true,
            s: true,
            pf: true,
            phi_deg: true,
            u_pk: true,
            i_pk: true,
            energy_wh: true,
        }
    }
}

impl PowerOutputFlags {
    /// Returns `true` if at least one metric output channel is enabled.
    pub fn any_enabled(&self) -> bool {
        self.freq
            || self.u_rms
            || self.i_rms
            || self.p
            || self.q1
            || self.q_tot
            || self.s
            || self.pf
            || self.phi_deg
            || self.u_pk
            || self.i_pk
            || self.energy_wh
    }
}

fn validate_local_segment(name: &str, label: &str) -> CoreResult<()> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(CoreError::InvalidConfig(format!(
            "{label} must not be empty"
        )));
    }
    if trimmed.contains('/') {
        return Err(CoreError::InvalidConfig(format!(
            "{label} (`{trimmed}`) must not contain '/'"
        )));
    }
    if trimmed != name {
        return Err(CoreError::InvalidConfig(format!(
            "{label} (`{name}`) must not have leading or trailing whitespace"
        )));
    }
    Ok(())
}

fn validate_input_channel_path(path: &str, label: &str) -> CoreResult<()> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(CoreError::InvalidConfig(format!(
            "{label} must not be empty"
        )));
    }
    if !trimmed.contains('/') {
        return Err(CoreError::InvalidConfig(format!(
            "{label} (`{trimmed}`) must be formatted as `<source_id>/<local_path>`"
        )));
    }
    Ok(())
}

fn ensure_finite(val: f64, label: &str) -> CoreResult<()> {
    if !val.is_finite() {
        return Err(CoreError::InvalidConfig(format!(
            "{label} must be finite (got {val})"
        )));
    }
    Ok(())
}

fn ensure_positive_finite(val: f64, label: &str) -> CoreResult<()> {
    ensure_finite(val, label)?;
    if val <= 0.0 {
        return Err(CoreError::InvalidConfig(format!(
            "{label} must be > 0 (got {val})"
        )));
    }
    Ok(())
}

impl VirtualNodeConfig {
    /// Validates configuration invariants and formula compilation before registering in `VirtualDagEngine`.
    pub fn validate(&self) -> CoreResult<()> {
        if self.node_id.trim().is_empty() {
            return Err(CoreError::InvalidConfig(
                "virtual node `node_id` must not be empty".into(),
            ));
        }

        match &self.operator {
            VirtualOperatorConfig::AnalogToDigital {
                input_channel,
                output_path,
                mode,
            } => {
                validate_input_channel_path(input_channel, "input_channel")?;
                validate_local_segment(output_path, "output_path")?;
                match mode {
                    AnalogToDigitalMode::Schmitt {
                        threshold,
                        hysteresis,
                        ..
                    } => {
                        ensure_finite(*threshold, "threshold")?;
                        ensure_finite(*hysteresis, "hysteresis")?;
                        if *hysteresis < 0.0 {
                            return Err(CoreError::InvalidConfig(
                                "hysteresis must be >= 0".into(),
                            ));
                        }
                    }
                    AnalogToDigitalMode::Window { low, high, .. } => {
                        ensure_finite(*low, "window.low")?;
                        ensure_finite(*high, "window.high")?;
                        if low > high {
                            return Err(CoreError::InvalidConfig(format!(
                                "window low ({low}) must be <= high ({high})"
                            )));
                        }
                    }
                }
            }
            VirtualOperatorConfig::ToStateMachine { output_path, mode } => {
                validate_local_segment(output_path, "output_path")?;
                match mode {
                    ToStateMachineMode::RangeMap {
                        input_channel,
                        hysteresis,
                        rules,
                        default_state,
                    } => {
                        validate_input_channel_path(input_channel, "input_channel")?;
                        ensure_finite(*hysteresis, "hysteresis")?;
                        if *hysteresis < 0.0 {
                            return Err(CoreError::InvalidConfig(
                                "hysteresis must be >= 0".into(),
                            ));
                        }
                        if default_state.name.trim().is_empty() {
                            return Err(CoreError::InvalidConfig(
                                "default_state.name must not be empty".into(),
                            ));
                        }
                        for (idx, r) in rules.iter().enumerate() {
                            if let Some(min) = r.min {
                                ensure_finite(min, &format!("rules[{idx}].min"))?;
                            }
                            if let Some(max) = r.max {
                                ensure_finite(max, &format!("rules[{idx}].max"))?;
                            }
                            if let (Some(min), Some(max)) = (r.min, r.max) {
                                if min > max {
                                    return Err(CoreError::InvalidConfig(format!(
                                        "rules[{idx}].min ({min}) must be <= max ({max})"
                                    )));
                                }
                            }
                            if r.name.trim().is_empty() {
                                return Err(CoreError::InvalidConfig(format!(
                                    "rules[{idx}].name must not be empty"
                                )));
                            }
                        }
                    }
                    ToStateMachineMode::BitCombine {
                        bit_channels,
                        threshold,
                        ..
                    } => {
                        if bit_channels.is_empty() || bit_channels.len() > 8 {
                            return Err(CoreError::InvalidConfig(
                                "BitCombine requires 1..=8 bit_channels".into(),
                            ));
                        }
                        ensure_finite(*threshold, "threshold")?;
                        for (i, ch) in bit_channels.iter().enumerate() {
                            validate_input_channel_path(ch, &format!("bit_channels[{i}]"))?;
                        }
                    }
                }
            }
            VirtualOperatorConfig::ToAnalog {
                output_path, mode, ..
            } => {
                validate_local_segment(output_path, "output_path")?;
                match mode {
                    ToAnalogMode::LevelMap {
                        input_channel,
                        low_value,
                        high_value,
                        threshold,
                        smoothing_tau_ms,
                    } => {
                        validate_input_channel_path(input_channel, "input_channel")?;
                        ensure_finite(*low_value, "low_value")?;
                        ensure_finite(*high_value, "high_value")?;
                        ensure_finite(*threshold, "threshold")?;
                        ensure_finite(*smoothing_tau_ms, "smoothing_tau_ms")?;
                        if *smoothing_tau_ms < 0.0 {
                            return Err(CoreError::InvalidConfig(
                                "smoothing_tau_ms must be >= 0".into(),
                            ));
                        }
                    }
                    ToAnalogMode::BitWeightedDac {
                        bit_channels,
                        scale,
                        offset,
                        threshold,
                    } => {
                        if bit_channels.is_empty() || bit_channels.len() > 16 {
                            return Err(CoreError::InvalidConfig(
                                "BitWeightedDac requires 1..=16 bit_channels".into(),
                            ));
                        }
                        ensure_finite(*scale, "scale")?;
                        ensure_finite(*offset, "offset")?;
                        ensure_finite(*threshold, "threshold")?;
                        for (i, ch) in bit_channels.iter().enumerate() {
                            validate_input_channel_path(ch, &format!("bit_channels[{i}]"))?;
                        }
                    }
                    ToAnalogMode::StateValueMap {
                        input_channel,
                        entries,
                        default_value,
                    } => {
                        validate_input_channel_path(input_channel, "input_channel")?;
                        ensure_finite(*default_value, "default_value")?;
                        for (i, entry) in entries.iter().enumerate() {
                            ensure_finite(entry.value, &format!("entries[{i}].value"))?;
                        }
                    }
                }
            }
            VirtualOperatorConfig::Formula {
                expression,
                output_path,
                ..
            } => {
                validate_local_segment(output_path, "output_path")?;
                CompiledFormula::compile(expression)?;
            }
            VirtualOperatorConfig::WindowStats {
                input_channel,
                output_path,
                window_ms,
                ..
            } => {
                validate_input_channel_path(input_channel, "input_channel")?;
                validate_local_segment(output_path, "output_path")?;
                ensure_positive_finite(*window_ms, "window_ms")?;
            }
            VirtualOperatorConfig::Calculus {
                input_channel,
                output_path,
                mode,
                ..
            } => {
                validate_input_channel_path(input_channel, "input_channel")?;
                validate_local_segment(output_path, "output_path")?;
                match mode {
                    CalculusMode::Derivative {
                        scale,
                        filter_tau_ms,
                    } => {
                        ensure_finite(*scale, "scale")?;
                        ensure_finite(*filter_tau_ms, "filter_tau_ms")?;
                        if *filter_tau_ms < 0.0 {
                            return Err(CoreError::InvalidConfig(
                                "filter_tau_ms must be >= 0".into(),
                            ));
                        }
                    }
                    CalculusMode::Integral {
                        scale,
                        dc_offset,
                        initial_value,
                        clamp_min,
                        clamp_max,
                    } => {
                        ensure_finite(*scale, "scale")?;
                        ensure_finite(*dc_offset, "dc_offset")?;
                        ensure_finite(*initial_value, "initial_value")?;
                        if let Some(cmin) = clamp_min {
                            ensure_finite(*cmin, "clamp_min")?;
                        }
                        if let Some(cmax) = clamp_max {
                            ensure_finite(*cmax, "clamp_max")?;
                        }
                        if let (Some(cmin), Some(cmax)) = (clamp_min, clamp_max) {
                            if cmin > cmax {
                                return Err(CoreError::InvalidConfig(format!(
                                    "clamp_min ({cmin}) must be <= clamp_max ({cmax})"
                                )));
                            }
                        }
                    }
                }
            }
            VirtualOperatorConfig::SinglePhasePower {
                voltage_channel,
                current_channel,
                output_prefix,
                voltage_scale,
                voltage_offset,
                current_scale,
                current_offset,
                nominal_freq_hz,
                zero_cross_hysteresis_v,
                min_freq_hz,
                fallback_window_ms,
                outputs,
            } => {
                validate_input_channel_path(voltage_channel, "voltage_channel")?;
                validate_input_channel_path(current_channel, "current_channel")?;
                validate_local_segment(output_prefix, "output_prefix")?;
                ensure_finite(*voltage_scale, "voltage_scale")?;
                ensure_finite(*voltage_offset, "voltage_offset")?;
                ensure_finite(*current_scale, "current_scale")?;
                ensure_finite(*current_offset, "current_offset")?;
                ensure_positive_finite(*nominal_freq_hz, "nominal_freq_hz")?;
                ensure_finite(*zero_cross_hysteresis_v, "zero_cross_hysteresis_v")?;
                if *zero_cross_hysteresis_v < 0.0 {
                    return Err(CoreError::InvalidConfig(
                        "zero_cross_hysteresis_v must be >= 0".into(),
                    ));
                }
                ensure_positive_finite(*min_freq_hz, "min_freq_hz")?;
                ensure_positive_finite(*fallback_window_ms, "fallback_window_ms")?;
                if !outputs.any_enabled() {
                    return Err(CoreError::InvalidConfig(
                        "SinglePhasePower must enable at least one output channel".into(),
                    ));
                }
            }
        }

        Ok(())
    }

    /// Returns all input channel paths `<source_id>/<local_path>` consumed by this node (deduplicated in order).
    pub fn input_paths(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut push_unique = |path: &str| {
            if !out.iter().any(|existing: &String| existing == path) {
                out.push(path.to_owned());
            }
        };

        match &self.operator {
            VirtualOperatorConfig::AnalogToDigital { input_channel, .. } => {
                push_unique(input_channel);
            }
            VirtualOperatorConfig::ToStateMachine { mode, .. } => match mode {
                ToStateMachineMode::RangeMap { input_channel, .. } => push_unique(input_channel),
                ToStateMachineMode::BitCombine { bit_channels, .. } => {
                    for ch in bit_channels {
                        push_unique(ch);
                    }
                }
            },
            VirtualOperatorConfig::ToAnalog { mode, .. } => match mode {
                ToAnalogMode::LevelMap { input_channel, .. }
                | ToAnalogMode::StateValueMap { input_channel, .. } => push_unique(input_channel),
                ToAnalogMode::BitWeightedDac { bit_channels, .. } => {
                    for ch in bit_channels {
                        push_unique(ch);
                    }
                }
            },
            VirtualOperatorConfig::Formula { expression, .. } => {
                if let Ok(compiled) = CompiledFormula::compile(expression) {
                    for ch in compiled.input_channels() {
                        push_unique(ch);
                    }
                }
            }
            VirtualOperatorConfig::WindowStats { input_channel, .. }
            | VirtualOperatorConfig::Calculus { input_channel, .. } => {
                push_unique(input_channel);
            }
            VirtualOperatorConfig::SinglePhasePower {
                voltage_channel,
                current_channel,
                ..
            } => {
                push_unique(voltage_channel);
                push_unique(current_channel);
            }
        }

        out
    }

    /// Expands this node into [`ChannelDescriptor`] entries registered under `virtual/<local_path>`.
    pub fn output_descriptors(&self) -> CoreResult<Vec<ChannelDescriptor>> {
        self.validate()?;
        let label_prefix = if self.name.trim().is_empty() {
            self.node_id.as_str()
        } else {
            self.name.trim()
        };

        let make_scalar = |local_path: String, display_name: String, unit: Option<String>| {
            let mut desc = ChannelDescriptor::new_scalar(
                SourceId::new(VIRTUAL_SOURCE_ID),
                local_path,
                ScalarType::F32,
            );
            desc.display_name = Some(display_name);
            desc.unit = unit.filter(|u| !u.trim().is_empty());
            desc
        };

        let make_bool = |local_path: String, display_name: String| {
            let mut desc = ChannelDescriptor::new_bool(SourceId::new(VIRTUAL_SOURCE_ID), local_path);
            desc.display_name = Some(display_name);
            desc
        };

        match &self.operator {
            VirtualOperatorConfig::AnalogToDigital { output_path, .. } => {
                Ok(vec![make_bool(output_path.clone(), label_prefix.to_owned())])
            }
            VirtualOperatorConfig::ToStateMachine { output_path, mode } => {
                let mut labels: StateEnumMap = BTreeMap::new();
                match mode {
                    ToStateMachineMode::RangeMap {
                        rules,
                        default_state,
                        ..
                    } => {
                        labels.insert(
                            default_state.code,
                            StateLabel {
                                name: default_state.name.clone(),
                                color_rgba: default_state.color_rgba,
                            },
                        );
                        for r in rules {
                            labels.insert(
                                r.code,
                                StateLabel {
                                    name: r.name.clone(),
                                    color_rgba: r.color_rgba,
                                },
                            );
                        }
                    }
                    ToStateMachineMode::BitCombine {
                        bit_channels,
                        labels: spec_labels,
                        ..
                    } => {
                        for spec in spec_labels {
                            labels.insert(
                                spec.code,
                                StateLabel {
                                    name: spec.name.clone(),
                                    color_rgba: spec.color_rgba,
                                },
                            );
                        }
                        if labels.is_empty() && bit_channels.len() <= 4 {
                            let width = bit_channels.len();
                            let count = 1_i32 << width;
                            for code in 0..count {
                                labels.insert(
                                    code,
                                    StateLabel {
                                        name: format!("0b{code:0width$b}"),
                                        color_rgba: None,
                                    },
                                );
                            }
                        }
                    }
                }
                let mut desc = ChannelDescriptor::new_state_machine(
                    SourceId::new(VIRTUAL_SOURCE_ID),
                    output_path.clone(),
                    labels,
                );
                desc.display_name = Some(label_prefix.to_owned());
                Ok(vec![desc])
            }
            VirtualOperatorConfig::ToAnalog {
                output_path, unit, ..
            } => Ok(vec![make_scalar(
                output_path.clone(),
                label_prefix.to_owned(),
                unit.clone(),
            )]),
            VirtualOperatorConfig::Formula {
                output_path,
                unit,
                output_digital,
                ..
            } => {
                if *output_digital {
                    Ok(vec![make_bool(output_path.clone(), label_prefix.to_owned())])
                } else {
                    Ok(vec![make_scalar(
                        output_path.clone(),
                        label_prefix.to_owned(),
                        unit.clone(),
                    )])
                }
            }
            VirtualOperatorConfig::WindowStats {
                output_path, unit, ..
            }
            | VirtualOperatorConfig::Calculus {
                output_path, unit, ..
            } => Ok(vec![make_scalar(
                output_path.clone(),
                label_prefix.to_owned(),
                unit.clone(),
            )]),
            VirtualOperatorConfig::SinglePhasePower {
                output_prefix,
                outputs,
                ..
            } => {
                let mut descs = Vec::with_capacity(12);
                let mut push_metric =
                    |enabled: bool, suffix: &str, title_suffix: &str, unit: Option<&str>| {
                        if enabled {
                            descs.push(make_scalar(
                                format!("{output_prefix}.{suffix}"),
                                format!("{label_prefix} {title_suffix}"),
                                unit.map(str::to_owned),
                            ));
                        }
                    };
                push_metric(outputs.freq, "freq", "Freq", Some("Hz"));
                push_metric(outputs.u_rms, "u_rms", "Urms", Some("V"));
                push_metric(outputs.i_rms, "i_rms", "Irms", Some("A"));
                push_metric(outputs.p, "p", "Active P", Some("W"));
                push_metric(outputs.q1, "q1", "Fund. Q1", Some("var"));
                push_metric(outputs.q_tot, "q_tot", "Total Q", Some("var"));
                push_metric(outputs.s, "s", "Apparent S", Some("VA"));
                push_metric(outputs.pf, "pf", "PF", None);
                push_metric(outputs.phi_deg, "phi_deg", "Phase φ", Some("deg"));
                push_metric(outputs.u_pk, "u_pk", "Upk", Some("V"));
                push_metric(outputs.i_pk, "i_pk", "Ipk", Some("A"));
                push_metric(outputs.energy_wh, "energy_wh", "Energy", Some("Wh"));
                Ok(descs)
            }
        }
    }
}
