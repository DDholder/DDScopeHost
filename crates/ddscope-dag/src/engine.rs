//! Lock-safe virtual DAG execution engine with topological ordering and retroactive backfill.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;

use ddscope_storage::{ChannelEntry, ChannelHub, ChannelStore, WatermarkEvent, WatermarkListener};
use ddscope_types::{
    ChannelDescriptor, ChannelId, CoreError, CoreResult, DecodedColumnBuf, ScalarSlice, TimestampNs,
};

use super::config::{
    AnalogToDigitalMode, CalculusMode, PowerOutputFlags, ToAnalogMode, ToStateMachineMode,
    VirtualNodeConfig, VirtualNodeSummary, VirtualOperatorConfig, WindowMetricKind,
    VIRTUAL_SOURCE_ID,
};
use super::formula::CompiledFormula;
use super::power::{SinglePhasePowerParams, SinglePhasePowerSample, SinglePhasePowerState};
use super::window_stats::SwagWindowTracker;

const DAG_BATCH_LIMIT: usize = 4_096;
const MULTI_SOURCE_SKEW_GRACE_NS: u64 = 100_000_000; // 100 ms
const VIRTUAL_MASK_WORDS: usize = 64; // covers ChannelId(0..4096) lock-free

/// Lock-free [`WatermarkListener`] that marks the DAG dirty only when non-virtual channels advance.
#[derive(Debug)]
pub struct DagWatermarkFlag {
    dirty: AtomicBool,
    virtual_mask: [AtomicU64; VIRTUAL_MASK_WORDS],
}

impl DagWatermarkFlag {
    fn new() -> Self {
        Self {
            dirty: AtomicBool::new(false),
            virtual_mask: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    fn mark_virtual(&self, id: ChannelId, is_virtual: bool) {
        let idx = id.0 as usize;
        let word = idx >> 6;
        if word >= self.virtual_mask.len() {
            return;
        }
        let bit = 1_u64 << (idx & 63);
        if is_virtual {
            self.virtual_mask[word].fetch_or(bit, Ordering::Release);
        } else {
            self.virtual_mask[word].fetch_and(!bit, Ordering::Release);
        }
    }

    #[inline]
    fn is_virtual(&self, id: ChannelId) -> bool {
        let idx = id.0 as usize;
        let word = idx >> 6;
        if word >= self.virtual_mask.len() {
            return false;
        }
        let bit = 1_u64 << (idx & 63);
        (self.virtual_mask[word].load(Ordering::Acquire) & bit) != 0
    }

    #[inline]
    fn set_dirty(&self) {
        self.dirty.store(true, Ordering::Release);
    }

    #[inline]
    fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::AcqRel)
    }
}

impl WatermarkListener for DagWatermarkFlag {
    #[inline]
    fn on_watermark_advanced(&self, events: &[WatermarkEvent]) {
        for ev in events {
            if !self.is_virtual(ev.channel_id) {
                self.dirty.store(true, Ordering::Release);
                break;
            }
        }
    }
}

#[derive(Copy, Clone, Debug, Default)]
struct SecondaryCursor {
    cached_id: Option<ChannelId>,
    next_idx: u64,
}

#[derive(Clone, Debug, Default)]
struct InputCursors {
    primary_id: Option<ChannelId>,
    primary_next_idx: u64,
    secondaries: Vec<SecondaryCursor>,
    last_emitted_ts: Option<TimestampNs>,
}

enum OperatorState {
    AnalogToDigital {
        mode: AnalogToDigitalMode,
        last_bool: Option<bool>,
    },
    ToStateMachineRange {
        hysteresis: f64,
        rules: Vec<(Option<f64>, Option<f64>, i32)>,
        default_code: i32,
        current_rule_idx: Option<usize>,
        has_prev: bool,
    },
    ToStateMachineBitCombine {
        threshold: f64,
        bit_slots: Vec<usize>,
    },
    ToAnalogLevel {
        low_value: f64,
        high_value: f64,
        threshold: f64,
        smoothing_tau_ms: f64,
        prev_ts_ns: Option<TimestampNs>,
        y_filt: f64,
    },
    ToAnalogBitDac {
        scale: f64,
        offset: f64,
        threshold: f64,
        bit_slots: Vec<usize>,
    },
    ToAnalogStateMap {
        entries: HashMap<i32, f64>,
        default_value: f64,
    },
    Formula {
        compiled: CompiledFormula,
        output_digital: bool,
        origin_ns: Option<TimestampNs>,
        stack: Vec<f64>,
    },
    WindowStats {
        metric: WindowMetricKind,
        tracker: SwagWindowTracker,
    },
    CalculusDerivative {
        scale: f64,
        filter_tau_ms: f64,
        prev_ts_ns: Option<TimestampNs>,
        prev_x: f64,
        y_filt: f64,
    },
    CalculusIntegral {
        scale: f64,
        dc_offset: f64,
        initial_value: f64,
        clamp_min: Option<f64>,
        clamp_max: Option<f64>,
        prev_ts_ns: Option<TimestampNs>,
        prev_x: f64,
        acc: f64,
    },
    SinglePhasePower {
        state: SinglePhasePowerState,
        flags: PowerOutputFlags,
        current_slot: usize,
        scratch: Vec<SinglePhasePowerSample>,
    },
}

fn resolve_slot_indices(unique_inputs: &[String], channels: &[String]) -> Vec<usize> {
    channels
        .iter()
        .map(|ch| {
            unique_inputs
                .iter()
                .position(|u| u == ch)
                .unwrap_or(0)
        })
        .collect()
}

impl OperatorState {
    fn from_config(cfg: &VirtualNodeConfig, unique_inputs: &[String]) -> CoreResult<Self> {
        match &cfg.operator {
            VirtualOperatorConfig::AnalogToDigital { mode, .. } => Ok(Self::AnalogToDigital {
                mode: mode.clone(),
                last_bool: None,
            }),
            VirtualOperatorConfig::ToStateMachine { mode, .. } => match mode {
                ToStateMachineMode::RangeMap {
                    hysteresis,
                    rules,
                    default_state,
                    ..
                } => Ok(Self::ToStateMachineRange {
                    hysteresis: hysteresis.abs(),
                    rules: rules.iter().map(|r| (r.min, r.max, r.code)).collect(),
                    default_code: default_state.code,
                    current_rule_idx: None,
                    has_prev: false,
                }),
                ToStateMachineMode::BitCombine {
                    bit_channels,
                    threshold,
                    ..
                } => Ok(Self::ToStateMachineBitCombine {
                    threshold: *threshold,
                    bit_slots: resolve_slot_indices(unique_inputs, bit_channels),
                }),
            },
            VirtualOperatorConfig::ToAnalog { mode, .. } => match mode {
                ToAnalogMode::LevelMap {
                    low_value,
                    high_value,
                    threshold,
                    smoothing_tau_ms,
                    ..
                } => Ok(Self::ToAnalogLevel {
                    low_value: *low_value,
                    high_value: *high_value,
                    threshold: *threshold,
                    smoothing_tau_ms: *smoothing_tau_ms,
                    prev_ts_ns: None,
                    y_filt: *low_value,
                }),
                ToAnalogMode::BitWeightedDac {
                    bit_channels,
                    scale,
                    offset,
                    threshold,
                    ..
                } => Ok(Self::ToAnalogBitDac {
                    scale: *scale,
                    offset: *offset,
                    threshold: *threshold,
                    bit_slots: resolve_slot_indices(unique_inputs, bit_channels),
                }),
                ToAnalogMode::StateValueMap {
                    entries,
                    default_value,
                    ..
                } => Ok(Self::ToAnalogStateMap {
                    entries: entries.iter().map(|e| (e.code, e.value)).collect(),
                    default_value: *default_value,
                }),
            },
            VirtualOperatorConfig::Formula {
                expression,
                output_digital,
                ..
            } => Ok(Self::Formula {
                compiled: CompiledFormula::compile(expression)?,
                output_digital: *output_digital,
                origin_ns: None,
                stack: Vec::with_capacity(16),
            }),
            VirtualOperatorConfig::WindowStats {
                metric, window_ms, ..
            } => Ok(Self::WindowStats {
                metric: *metric,
                tracker: SwagWindowTracker::new(*window_ms),
            }),
            VirtualOperatorConfig::Calculus { mode, .. } => match mode {
                CalculusMode::Derivative {
                    scale,
                    filter_tau_ms,
                } => Ok(Self::CalculusDerivative {
                    scale: *scale,
                    filter_tau_ms: *filter_tau_ms,
                    prev_ts_ns: None,
                    prev_x: 0.0,
                    y_filt: 0.0,
                }),
                CalculusMode::Integral {
                    scale,
                    dc_offset,
                    initial_value,
                    clamp_min,
                    clamp_max,
                } => {
                    let mut acc = *initial_value;
                    if let Some(cmin) = clamp_min {
                        acc = acc.max(*cmin);
                    }
                    if let Some(cmax) = clamp_max {
                        acc = acc.min(*cmax);
                    }
                    Ok(Self::CalculusIntegral {
                        scale: *scale,
                        dc_offset: *dc_offset,
                        initial_value: *initial_value,
                        clamp_min: *clamp_min,
                        clamp_max: *clamp_max,
                        prev_ts_ns: None,
                        prev_x: 0.0,
                        acc,
                    })
                }
            },
            VirtualOperatorConfig::SinglePhasePower {
                current_channel,
                voltage_scale,
                voltage_offset,
                current_scale,
                current_offset,
                nominal_freq_hz,
                zero_cross_hysteresis_v,
                min_freq_hz,
                fallback_window_ms,
                outputs,
                ..
            } => {
                let current_slot = unique_inputs
                    .iter()
                    .position(|u| u == current_channel)
                    .unwrap_or(0);
                Ok(Self::SinglePhasePower {
                    state: SinglePhasePowerState::new(SinglePhasePowerParams {
                        voltage_scale: *voltage_scale,
                        voltage_offset: *voltage_offset,
                        current_scale: *current_scale,
                        current_offset: *current_offset,
                        nominal_freq_hz: *nominal_freq_hz,
                        zero_cross_hysteresis_v: *zero_cross_hysteresis_v,
                        min_freq_hz: *min_freq_hz,
                        fallback_window_ms: *fallback_window_ms,
                    }),
                    flags: *outputs,
                    current_slot,
                    scratch: Vec::with_capacity(64),
                })
            }
        }
    }

    fn reset(&mut self) {
        match self {
            Self::AnalogToDigital { last_bool, .. } => *last_bool = None,
            Self::ToStateMachineRange {
                current_rule_idx,
                has_prev,
                ..
            } => {
                *current_rule_idx = None;
                *has_prev = false;
            }
            Self::ToStateMachineBitCombine { .. } => {}
            Self::ToAnalogLevel {
                low_value,
                prev_ts_ns,
                y_filt,
                ..
            } => {
                *prev_ts_ns = None;
                *y_filt = *low_value;
            }
            Self::ToAnalogBitDac { .. } | Self::ToAnalogStateMap { .. } => {}
            Self::Formula { origin_ns, .. } => *origin_ns = None,
            Self::WindowStats { tracker, .. } => tracker.reset(),
            Self::CalculusDerivative {
                prev_ts_ns,
                prev_x,
                y_filt,
                ..
            } => {
                *prev_ts_ns = None;
                *prev_x = 0.0;
                *y_filt = 0.0;
            }
            Self::CalculusIntegral {
                initial_value,
                clamp_min,
                clamp_max,
                prev_ts_ns,
                prev_x,
                acc,
                ..
            } => {
                let mut init = *initial_value;
                if let Some(cmin) = clamp_min {
                    init = init.max(*cmin);
                }
                if let Some(cmax) = clamp_max {
                    init = init.min(*cmax);
                }
                *prev_ts_ns = None;
                *prev_x = 0.0;
                *acc = init;
            }
            Self::SinglePhasePower { state, scratch, .. } => {
                state.reset();
                scratch.clear();
            }
        }
    }
}

struct NodeRuntime {
    config: VirtualNodeConfig,
    input_paths: Vec<String>,
    output_descs: Vec<ChannelDescriptor>,
    outputs: Vec<Arc<ChannelEntry>>,
    cursors: InputCursors,
    op: OperatorState,
}

impl NodeRuntime {
    fn summary(&self) -> VirtualNodeSummary {
        VirtualNodeSummary {
            node_id: self.config.node_id.clone(),
            name: self.config.name.clone(),
            enabled: self.config.enabled,
            output_channels: self.outputs.iter().map(|e| e.id()).collect(),
            output_paths: self
                .outputs
                .iter()
                .map(|e| e.path().as_str().to_owned())
                .collect(),
        }
    }

    fn reset_runtime(&mut self) {
        self.cursors = InputCursors {
            primary_id: None,
            primary_next_idx: 0,
            secondaries: vec![SecondaryCursor::default(); self.input_paths.len().saturating_sub(1)],
            last_emitted_ts: None,
        };
        self.op.reset();
    }
}

struct ScratchBufs {
    ts: Vec<TimestampNs>,
    primary_vals: Vec<f64>,
    secondary_cols: Vec<Vec<f64>>,
    formula_row: Vec<f64>,
    out_bool: Vec<bool>,
    out_f32: Vec<f32>,
    out_state_enum: DecodedColumnBuf,
    power_ts: Vec<TimestampNs>,
    power_cols: [Vec<f32>; 12],
}

impl ScratchBufs {
    fn new() -> Self {
        Self {
            ts: Vec::with_capacity(DAG_BATCH_LIMIT),
            primary_vals: Vec::with_capacity(DAG_BATCH_LIMIT),
            secondary_cols: Vec::new(),
            formula_row: Vec::new(),
            out_bool: Vec::with_capacity(DAG_BATCH_LIMIT),
            out_f32: Vec::with_capacity(DAG_BATCH_LIMIT),
            out_state_enum: DecodedColumnBuf::StateEnum(Vec::with_capacity(DAG_BATCH_LIMIT)),
            power_ts: Vec::with_capacity(64),
            power_cols: std::array::from_fn(|_| Vec::with_capacity(64)),
        }
    }
}

struct DagInner {
    nodes: HashMap<String, NodeRuntime>,
    topo_order: Vec<String>,
    scratch: ScratchBufs,
}

impl DagInner {
    fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            topo_order: Vec::new(),
            scratch: ScratchBufs::new(),
        }
    }

    /// Computes topological execution order and rejects cycles or output path collisions.
    fn compute_topo_order(
        nodes: &HashMap<String, NodeRuntime>,
    ) -> CoreResult<Vec<String>> {
        let mut producer_by_full_path: HashMap<String, String> = HashMap::new();
        for (id, rt) in nodes {
            for desc in &rt.output_descs {
                let fp = desc.full_path().as_str().to_owned();
                if let Some(other) = producer_by_full_path.insert(fp.clone(), id.clone()) {
                    return Err(CoreError::InvalidConfig(format!(
                        "virtual output path `{fp}` conflicts between node `{other}` and `{id}`"
                    )));
                }
            }
        }

        let mut in_degree: HashMap<String, usize> =
            nodes.keys().map(|id| (id.clone(), 0)).collect();
        let mut edges: HashMap<String, Vec<String>> =
            nodes.keys().map(|id| (id.clone(), Vec::new())).collect();

        for (consumer_id, rt) in nodes {
            let mut deps_seen = HashSet::new();
            for in_path in &rt.input_paths {
                if let Some(producer_id) = producer_by_full_path.get(in_path) {
                    if producer_id == consumer_id {
                        return Err(CoreError::InvalidConfig(format!(
                            "virtual node `{consumer_id}` cannot depend on its own output `{in_path}`"
                        )));
                    }
                    if deps_seen.insert(producer_id.clone()) {
                        edges.get_mut(producer_id).unwrap().push(consumer_id.clone());
                        *in_degree.get_mut(consumer_id).unwrap() += 1;
                    }
                }
            }
        }

        let mut ready: Vec<String> = in_degree
            .iter()
            .filter_map(|(id, &deg)| (deg == 0).then_some(id.clone()))
            .collect();
        ready.sort();
        let mut queue: VecDeque<String> = ready.into_iter().collect();
        let mut order = Vec::with_capacity(nodes.len());

        while let Some(nid) = queue.pop_front() {
            order.push(nid.clone());
            if let Some(children) = edges.get(&nid) {
                let mut newly_ready = Vec::new();
                for child in children {
                    let deg = in_degree.get_mut(child).unwrap();
                    *deg -= 1;
                    if *deg == 0 {
                        newly_ready.push(child.clone());
                    }
                }
                newly_ready.sort();
                for ch in newly_ready {
                    queue.push_back(ch);
                }
            }
        }

        if order.len() != nodes.len() {
            return Err(CoreError::InvalidConfig(
                "cycle detected in virtual channel DAG".into(),
            ));
        }

        Ok(order)
    }

    fn collect_descendants(
        &self,
        root_ids: &[String],
    ) -> HashSet<String> {
        let mut producer_by_path: HashMap<String, String> = HashMap::new();
        for (id, rt) in &self.nodes {
            for desc in &rt.output_descs {
                producer_by_path.insert(desc.full_path().as_str().to_owned(), id.clone());
            }
        }

        let mut affected: HashSet<String> = root_ids.iter().cloned().collect();
        for nid in &self.topo_order {
            if affected.contains(nid) {
                continue;
            }
            if let Some(rt) = self.nodes.get(nid) {
                for inp in &rt.input_paths {
                    if let Some(prod) = producer_by_path.get(inp) {
                        if affected.contains(prod) {
                            affected.insert(nid.clone());
                            break;
                        }
                    }
                }
            }
        }
        affected
    }

    fn process_all(&mut self, hub: &ChannelHub, flag: &DagWatermarkFlag, force_align: bool) {
        let order = self.topo_order.clone();
        for nid in order {
            loop {
                let processed = self.process_node_batch(&nid, hub, flag, force_align);
                if !processed {
                    break;
                }
            }
        }
    }

    fn process_node_batch(
        &mut self,
        node_id: &str,
        hub: &ChannelHub,
        flag: &DagWatermarkFlag,
        force_align: bool,
    ) -> bool {
        let Some(rt) = self.nodes.get_mut(node_id) else {
            return false;
        };
        if !rt.config.enabled || rt.outputs.is_empty() {
            return false;
        }

        // Resolve primary input channel (for 0-input formulas, pick first non-virtual scalar channel as clock carrier)
        let primary_entry = if let Some(primary_path) = rt.input_paths.first() {
            match hub.get_by_path_str(primary_path) {
                Some(e) => e,
                None => return false,
            }
        } else {
            match first_non_virtual_scalar_channel(hub) {
                Some(e) => e,
                None => return false,
            }
        };

        if rt.cursors.primary_id != Some(primary_entry.id()) {
            rt.cursors.primary_id = Some(primary_entry.id());
            rt.cursors.primary_next_idx = 0;
        }

        let sec_paths = if rt.input_paths.len() > 1 {
            &rt.input_paths[1..]
        } else {
            &[]
        };
        if rt.cursors.secondaries.len() != sec_paths.len() {
            rt.cursors.secondaries = vec![SecondaryCursor::default(); sec_paths.len()];
        }

        let mut sec_entries = Vec::with_capacity(sec_paths.len());
        for (idx, path) in sec_paths.iter().enumerate() {
            let Some(e) = hub.get_by_path_str(path) else {
                return false;
            };
            if rt.cursors.secondaries[idx].cached_id != Some(e.id()) {
                rt.cursors.secondaries[idx] = SecondaryCursor {
                    cached_id: Some(e.id()),
                    next_idx: 0,
                };
            }
            sec_entries.push(e);
        }

        // For multi-input scalar nodes (e.g. SinglePhasePower, Formula, BitCombine, BitWeightedDac):
        // if any secondary scalar input has 0 samples yet, wait rather than advancing cursor with fake 0s.
        let mut max_aligned_ts: Option<TimestampNs> = None;
        for sec in &sec_entries {
            if matches!(sec.store(), ChannelStore::Scalar(_)) {
                if sec.total_samples() == 0 {
                    return false;
                }
                if !force_align
                    && sec.source_id() != primary_entry.source_id()
                {
                    if let (Some((p_ts, _)), Some((s_ts, _))) =
                        (primary_entry.latest_value(), sec.latest_value())
                    {
                        if s_ts < p_ts && p_ts.saturating_sub(s_ts) <= MULTI_SOURCE_SKEW_GRACE_NS {
                            max_aligned_ts = Some(max_aligned_ts.map_or(s_ts, |m| m.min(s_ts)));
                        }
                    }
                }
            }
        }

        let scratch = &mut self.scratch;
        scratch.ts.clear();
        scratch.primary_vals.clear();

        // Phase 1: Read primary input mini-batch and immediately release its RwLock guard
        let next_idx = read_primary_batch(
            &primary_entry,
            rt.cursors.primary_next_idx,
            DAG_BATCH_LIMIT,
            max_aligned_ts,
            &mut scratch.ts,
            &mut scratch.primary_vals,
        );
        rt.cursors.primary_next_idx = next_idx;

        let batch_len = scratch.ts.len();
        if batch_len == 0 {
            return false;
        }

        // Phase 2: Align secondary inputs at `scratch.ts` and immediately release their RwLock guards
        if scratch.secondary_cols.len() < sec_entries.len() {
            scratch
                .secondary_cols
                .resize_with(sec_entries.len(), || Vec::with_capacity(DAG_BATCH_LIMIT));
        }
        for (i, sec_entry) in sec_entries.iter().enumerate() {
            let col_buf = &mut scratch.secondary_cols[i];
            col_buf.clear();
            align_secondary_batch(
                sec_entry,
                &scratch.ts,
                &mut rt.cursors.secondaries[i],
                col_buf,
            );
        }

        // Phase 3: Compute operator outputs in f64 (no ChannelStore locks held)
        // Enforce monotonic non-decreasing timestamps against any prior output on this node
        if let Some(last_ts) = rt.cursors.last_emitted_ts {
            for t in &mut scratch.ts {
                if *t < last_ts {
                    *t = last_ts;
                }
            }
        }

        match &mut rt.op {
            OperatorState::AnalogToDigital { mode, last_bool } => {
                scratch.out_bool.clear();
                scratch.out_bool.reserve(batch_len);
                match mode {
                    AnalogToDigitalMode::Schmitt {
                        threshold,
                        hysteresis,
                        invert,
                    } => {
                        let h = hysteresis.abs();
                        let v_low = *threshold - h;
                        let v_high = *threshold + h;
                        for &val in &scratch.primary_vals {
                            let safe = if val.is_finite() { val } else { 0.0 };
                            let raw = match *last_bool {
                                None => safe >= *threshold,
                                Some(false) => safe >= v_high,
                                Some(true) => safe > v_low,
                            };
                            *last_bool = Some(raw);
                            scratch.out_bool.push(if *invert { !raw } else { raw });
                        }
                    }
                    AnalogToDigitalMode::Window { low, high, invert } => {
                        for &val in &scratch.primary_vals {
                            let safe = if val.is_finite() { val } else { 0.0 };
                            let inside = safe >= *low && safe <= *high;
                            scratch.out_bool.push(if *invert { !inside } else { inside });
                        }
                    }
                }
                if let Some(&last_t) = scratch.ts.last() {
                    rt.cursors.last_emitted_ts = Some(last_t);
                }
                let _ = rt.outputs[0]
                    .append_scalar_batch(&scratch.ts, ScalarSlice::Bool(&scratch.out_bool));
            }
            OperatorState::ToStateMachineRange {
                hysteresis,
                rules,
                default_code,
                current_rule_idx,
                has_prev,
            } => {
                let DecodedColumnBuf::StateEnum(ref mut out_states) = scratch.out_state_enum else {
                    unreachable!("out_state_enum is always StateEnum");
                };
                out_states.clear();
                out_states.reserve(batch_len);
                for &val in &scratch.primary_vals {
                    let safe = if val.is_finite() { val } else { 0.0 };
                    let mut chosen_code = *default_code;
                    if *has_prev {
                        if let Some(idx) = *current_rule_idx {
                            let (min_b, max_b, code) = rules[idx];
                            if matches_rule(safe, min_b, max_b, *hysteresis) {
                                out_states.push(code);
                                continue;
                            }
                        }
                    }
                    let mut matched = None;
                    for (idx, &(min_b, max_b, code)) in rules.iter().enumerate() {
                        if matches_rule(safe, min_b, max_b, 0.0) {
                            matched = Some((idx, code));
                            break;
                        }
                    }
                    if let Some((idx, code)) = matched {
                        *current_rule_idx = Some(idx);
                        chosen_code = code;
                    } else {
                        *current_rule_idx = None;
                    }
                    *has_prev = true;
                    out_states.push(chosen_code);
                }
                if let Some(&last_t) = scratch.ts.last() {
                    rt.cursors.last_emitted_ts = Some(last_t);
                }
                let _ = rt.outputs[0].append_decoded_column(&scratch.ts, &scratch.out_state_enum);
            }
            OperatorState::ToStateMachineBitCombine {
                threshold,
                bit_slots,
            } => {
                let DecodedColumnBuf::StateEnum(ref mut out_states) = scratch.out_state_enum else {
                    unreachable!("out_state_enum is always StateEnum");
                };
                out_states.clear();
                out_states.reserve(batch_len);
                for row in 0..batch_len {
                    let mut code: i32 = 0;
                    for (bit_idx, &slot) in bit_slots.iter().enumerate() {
                        let v = if slot == 0 {
                            scratch.primary_vals[row]
                        } else {
                            scratch.secondary_cols[slot - 1][row]
                        };
                        if v >= *threshold {
                            code |= 1_i32 << bit_idx;
                        }
                    }
                    out_states.push(code);
                }
                if let Some(&last_t) = scratch.ts.last() {
                    rt.cursors.last_emitted_ts = Some(last_t);
                }
                let _ = rt.outputs[0].append_decoded_column(&scratch.ts, &scratch.out_state_enum);
            }
            OperatorState::ToAnalogLevel {
                low_value,
                high_value,
                threshold,
                smoothing_tau_ms,
                prev_ts_ns,
                y_filt,
            } => {
                scratch.out_f32.clear();
                scratch.out_f32.reserve(batch_len);
                let tau_sec = *smoothing_tau_ms * 1e-3;
                for i in 0..batch_len {
                    let ts = scratch.ts[i];
                    let val = scratch.primary_vals[i];
                    let target = if val >= *threshold {
                        *high_value
                    } else {
                        *low_value
                    };
                    if tau_sec <= 0.0 {
                        *y_filt = target;
                    } else if let Some(prev) = *prev_ts_ns {
                        let dt_sec = ts.saturating_sub(prev) as f64 * 1e-9;
                        if dt_sec > 0.0 {
                            let alpha = dt_sec / (tau_sec + dt_sec);
                            *y_filt += alpha * (target - *y_filt);
                        }
                    } else {
                        *y_filt = target;
                    }
                    *prev_ts_ns = Some(ts);
                    scratch.out_f32.push(*y_filt as f32);
                }
                if let Some(&last_t) = scratch.ts.last() {
                    rt.cursors.last_emitted_ts = Some(last_t);
                }
                let _ = rt.outputs[0]
                    .append_scalar_batch(&scratch.ts, ScalarSlice::F32(&scratch.out_f32));
            }
            OperatorState::ToAnalogBitDac {
                scale,
                offset,
                threshold,
                bit_slots,
            } => {
                scratch.out_f32.clear();
                scratch.out_f32.reserve(batch_len);
                for row in 0..batch_len {
                    let mut code: u32 = 0;
                    for (bit_idx, &slot) in bit_slots.iter().enumerate() {
                        let v = if slot == 0 {
                            scratch.primary_vals[row]
                        } else {
                            scratch.secondary_cols[slot - 1][row]
                        };
                        if v >= *threshold {
                            code |= 1_u32 << bit_idx;
                        }
                    }
                    scratch
                        .out_f32
                        .push(((code as f64) * (*scale) + (*offset)) as f32);
                }
                if let Some(&last_t) = scratch.ts.last() {
                    rt.cursors.last_emitted_ts = Some(last_t);
                }
                let _ = rt.outputs[0]
                    .append_scalar_batch(&scratch.ts, ScalarSlice::F32(&scratch.out_f32));
            }
            OperatorState::ToAnalogStateMap {
                entries,
                default_value,
            } => {
                scratch.out_f32.clear();
                scratch.out_f32.reserve(batch_len);
                for &val in &scratch.primary_vals {
                    let code = val.round() as i32;
                    let mapped = entries.get(&code).copied().unwrap_or(*default_value);
                    scratch.out_f32.push(mapped as f32);
                }
                if let Some(&last_t) = scratch.ts.last() {
                    rt.cursors.last_emitted_ts = Some(last_t);
                }
                let _ = rt.outputs[0]
                    .append_scalar_batch(&scratch.ts, ScalarSlice::F32(&scratch.out_f32));
            }
            OperatorState::Formula {
                compiled,
                output_digital,
                origin_ns,
                stack,
            } => {
                let num_ch = compiled.input_channels().len();
                if scratch.formula_row.len() < num_ch {
                    scratch.formula_row.resize(num_ch, 0.0);
                }
                let orig = *origin_ns.get_or_insert(scratch.ts[0]);
                if *output_digital {
                    scratch.out_bool.clear();
                    scratch.out_bool.reserve(batch_len);
                } else {
                    scratch.out_f32.clear();
                    scratch.out_f32.reserve(batch_len);
                }
                for row in 0..batch_len {
                    if num_ch > 0 {
                        scratch.formula_row[0] = scratch.primary_vals[row];
                        for (sec_i, col) in
                            scratch.secondary_cols[..sec_entries.len()].iter().enumerate()
                        {
                            scratch.formula_row[sec_i + 1] = col[row];
                        }
                    }
                    let y = compiled.eval(
                        scratch.ts[row],
                        orig,
                        &scratch.formula_row[..num_ch],
                        stack,
                    );
                    if *output_digital {
                        scratch.out_bool.push(y >= 0.5);
                    } else {
                        scratch.out_f32.push(y as f32);
                    }
                }
                if let Some(&last_t) = scratch.ts.last() {
                    rt.cursors.last_emitted_ts = Some(last_t);
                }
                if *output_digital {
                    let _ = rt.outputs[0]
                        .append_scalar_batch(&scratch.ts, ScalarSlice::Bool(&scratch.out_bool));
                } else {
                    let _ = rt.outputs[0]
                        .append_scalar_batch(&scratch.ts, ScalarSlice::F32(&scratch.out_f32));
                }
            }
            OperatorState::WindowStats { metric, tracker } => {
                scratch.out_f32.clear();
                scratch.out_f32.reserve(batch_len);
                for i in 0..batch_len {
                    let y = tracker.push_and_eval(scratch.ts[i], scratch.primary_vals[i], *metric);
                    scratch.out_f32.push(y as f32);
                }
                if let Some(&last_t) = scratch.ts.last() {
                    rt.cursors.last_emitted_ts = Some(last_t);
                }
                let _ = rt.outputs[0]
                    .append_scalar_batch(&scratch.ts, ScalarSlice::F32(&scratch.out_f32));
            }
            OperatorState::CalculusDerivative {
                scale,
                filter_tau_ms,
                prev_ts_ns,
                prev_x,
                y_filt,
            } => {
                scratch.out_f32.clear();
                scratch.out_f32.reserve(batch_len);
                let tau_sec = *filter_tau_ms * 1e-3;
                for i in 0..batch_len {
                    let ts = scratch.ts[i];
                    let x = if scratch.primary_vals[i].is_finite() {
                        scratch.primary_vals[i]
                    } else {
                        0.0
                    };
                    if let Some(prev_t) = *prev_ts_ns {
                        let dt_sec = ts.saturating_sub(prev_t) as f64 * 1e-9;
                        if dt_sec > 0.0 {
                            let raw = ((x - *prev_x) / dt_sec) * (*scale);
                            if tau_sec > 0.0 {
                                let alpha = dt_sec / (tau_sec + dt_sec);
                                *y_filt += alpha * (raw - *y_filt);
                            } else {
                                *y_filt = raw;
                            }
                        }
                    } else {
                        *y_filt = 0.0;
                    }
                    *prev_x = x;
                    *prev_ts_ns = Some(ts);
                    scratch.out_f32.push(*y_filt as f32);
                }
                if let Some(&last_t) = scratch.ts.last() {
                    rt.cursors.last_emitted_ts = Some(last_t);
                }
                let _ = rt.outputs[0]
                    .append_scalar_batch(&scratch.ts, ScalarSlice::F32(&scratch.out_f32));
            }
            OperatorState::CalculusIntegral {
                scale,
                dc_offset,
                clamp_min,
                clamp_max,
                prev_ts_ns,
                prev_x,
                acc,
                ..
            } => {
                scratch.out_f32.clear();
                scratch.out_f32.reserve(batch_len);
                for i in 0..batch_len {
                    let ts = scratch.ts[i];
                    let raw = if scratch.primary_vals[i].is_finite() {
                        scratch.primary_vals[i]
                    } else {
                        0.0
                    };
                    let x = raw - *dc_offset;
                    if let Some(prev_t) = *prev_ts_ns {
                        let dt_sec = ts.saturating_sub(prev_t) as f64 * 1e-9;
                        if dt_sec > 0.0 {
                            *acc += 0.5 * (*prev_x + x) * dt_sec * (*scale);
                            if let Some(cmin) = clamp_min {
                                *acc = acc.max(*cmin);
                            }
                            if let Some(cmax) = clamp_max {
                                *acc = acc.min(*cmax);
                            }
                        }
                    }
                    *prev_x = x;
                    *prev_ts_ns = Some(ts);
                    scratch.out_f32.push(*acc as f32);
                }
                if let Some(&last_t) = scratch.ts.last() {
                    rt.cursors.last_emitted_ts = Some(last_t);
                }
                let _ = rt.outputs[0]
                    .append_scalar_batch(&scratch.ts, ScalarSlice::F32(&scratch.out_f32));
            }
            OperatorState::SinglePhasePower {
                state,
                flags,
                current_slot,
                scratch: pwr_out,
            } => {
                pwr_out.clear();
                let i_col: &[f64] = if *current_slot == 0 {
                    &scratch.primary_vals
                } else {
                    &scratch.secondary_cols[*current_slot - 1]
                };
                for row in 0..batch_len {
                    state.process_sample(
                        scratch.ts[row],
                        scratch.primary_vals[row],
                        i_col[row],
                        pwr_out,
                    );
                }
                if !pwr_out.is_empty() {
                    scratch.power_ts.clear();
                    for col in &mut scratch.power_cols {
                        col.clear();
                    }
                    let floor = rt.cursors.last_emitted_ts.unwrap_or(0);
                    let mut mono_prev = floor;
                    for s in pwr_out.iter() {
                        let t = s.ts_ns.max(mono_prev);
                        mono_prev = t;
                        scratch.power_ts.push(t);
                        if flags.freq {
                            scratch.power_cols[0].push(s.freq as f32);
                        }
                        if flags.u_rms {
                            scratch.power_cols[1].push(s.u_rms as f32);
                        }
                        if flags.i_rms {
                            scratch.power_cols[2].push(s.i_rms as f32);
                        }
                        if flags.p {
                            scratch.power_cols[3].push(s.p as f32);
                        }
                        if flags.q1 {
                            scratch.power_cols[4].push(s.q1 as f32);
                        }
                        if flags.q_tot {
                            scratch.power_cols[5].push(s.q_tot as f32);
                        }
                        if flags.s {
                            scratch.power_cols[6].push(s.s as f32);
                        }
                        if flags.pf {
                            scratch.power_cols[7].push(s.pf as f32);
                        }
                        if flags.phi_deg {
                            scratch.power_cols[8].push(s.phi_deg as f32);
                        }
                        if flags.u_pk {
                            scratch.power_cols[9].push(s.u_pk as f32);
                        }
                        if flags.i_pk {
                            scratch.power_cols[10].push(s.i_pk as f32);
                        }
                        if flags.energy_wh {
                            scratch.power_cols[11].push(s.energy_wh as f32);
                        }
                    }
                    rt.cursors.last_emitted_ts = scratch.power_ts.last().copied();
                    let active_slots = [
                        flags.freq,
                        flags.u_rms,
                        flags.i_rms,
                        flags.p,
                        flags.q1,
                        flags.q_tot,
                        flags.s,
                        flags.pf,
                        flags.phi_deg,
                        flags.u_pk,
                        flags.i_pk,
                        flags.energy_wh,
                    ];
                    let mut out_idx = 0usize;
                    for (slot_idx, enabled) in active_slots.iter().enumerate() {
                        if *enabled {
                            if let Some(entry) = rt.outputs.get(out_idx) {
                                let _ = entry.append_scalar_batch(
                                    &scratch.power_ts,
                                    ScalarSlice::F32(&scratch.power_cols[slot_idx]),
                                );
                            }
                            out_idx += 1;
                        }
                    }
                }
            }
        }

        let _ = flag;
        batch_len == DAG_BATCH_LIMIT
    }
}

#[inline]
fn matches_rule(val: f64, min_b: Option<f64>, max_b: Option<f64>, margin: f64) -> bool {
    if let (Some(lo), Some(hi)) = (min_b, max_b) {
        if (lo - hi).abs() <= 1e-12 {
            return (val - lo).abs() <= margin.max(1e-12);
        }
    }
    let ge_min = min_b.is_none_or(|m| val >= m - margin);
    let lt_max = max_b.is_none_or(|m| val < m + margin);
    ge_min && lt_max
}

fn first_non_virtual_scalar_channel(hub: &ChannelHub) -> Option<Arc<ChannelEntry>> {
    hub.all_channels().into_iter().find(|entry| {
        entry.source_id().as_str() != VIRTUAL_SOURCE_ID
            && matches!(entry.store(), ChannelStore::Scalar(_))
    })
}

fn read_primary_batch(
    entry: &ChannelEntry,
    mut cursor: u64,
    limit: usize,
    max_ts: Option<TimestampNs>,
    ts_out: &mut Vec<TimestampNs>,
    val_out: &mut Vec<f64>,
) -> u64 {
    match entry.store() {
        ChannelStore::Scalar(lock) => {
            let series = lock.read();
            let total = series.total_samples();
            let evicted = series.evicted_samples();
            if cursor < evicted {
                cursor = evicted;
            }
            while cursor < total && ts_out.len() < limit {
                let Some((ts, val)) = series.get_sample_by_index(cursor) else {
                    break;
                };
                if max_ts.is_some_and(|cap| ts > cap) {
                    break;
                }
                ts_out.push(ts);
                val_out.push(val);
                cursor += 1;
            }
        }
        ChannelStore::Event(lock) => {
            let series = lock.read();
            let total = series.total_samples();
            let evicted = series.evicted_samples();
            if cursor < evicted {
                cursor = evicted;
            }
            while cursor < total && ts_out.len() < limit {
                let Some((ts, code)) = series.get_transition_by_index(cursor) else {
                    break;
                };
                if max_ts.is_some_and(|cap| ts > cap) {
                    break;
                }
                ts_out.push(ts);
                val_out.push(f64::from(code));
                cursor += 1;
            }
        }
        ChannelStore::Frame { .. } => {}
    }
    cursor
}

fn align_secondary_batch(
    entry: &ChannelEntry,
    target_ts: &[TimestampNs],
    cursor: &mut SecondaryCursor,
    val_out: &mut Vec<f64>,
) {
    match entry.store() {
        ChannelStore::Scalar(lock) => {
            let series = lock.read();
            let total = series.total_samples();
            let evicted = series.evicted_samples();
            if total <= evicted {
                val_out.resize(target_ts.len(), 0.0);
                return;
            }

            if cursor.next_idx < evicted {
                cursor.next_idx = evicted;
            }
            if cursor.next_idx >= total {
                cursor.next_idx = total - 1;
            }

            if let Some(first_t) = target_ts.first().copied() {
                if let Some((cur_t, _)) = series.get_sample_by_index(cursor.next_idx) {
                    if cur_t > first_t {
                        if let Some((idx, _, _)) = series.get_sample_by_timestamp(first_t) {
                            cursor.next_idx = idx.max(evicted);
                        } else {
                            cursor.next_idx = evicted;
                        }
                    }
                }
            }

            let mut cur_pair = series.get_sample_by_index(cursor.next_idx);
            let mut next_pair = if cursor.next_idx + 1 < total {
                series.get_sample_by_index(cursor.next_idx + 1)
            } else {
                None
            };

            for &t in target_ts {
                while let Some((t1, v1)) = next_pair {
                    if t1 <= t {
                        cursor.next_idx += 1;
                        cur_pair = Some((t1, v1));
                        next_pair = if cursor.next_idx + 1 < total {
                            series.get_sample_by_index(cursor.next_idx + 1)
                        } else {
                            None
                        };
                    } else {
                        break;
                    }
                }

                let aligned = match (cur_pair, next_pair) {
                    (Some((t0, v0)), Some((t1, v1))) => {
                        if t <= t0 {
                            v0
                        } else if t >= t1 || t1 <= t0 {
                            v1
                        } else {
                            let frac = ((t - t0) as f64) / ((t1 - t0) as f64);
                            v0 + (v1 - v0) * frac
                        }
                    }
                    (Some((_, v0)), None) => v0,
                    _ => 0.0,
                };
                val_out.push(aligned);
            }
        }
        ChannelStore::Event(lock) => {
            let series = lock.read();
            let total = series.total_samples();
            let evicted = series.evicted_samples();
            if total <= evicted {
                val_out.resize(target_ts.len(), 0.0);
                return;
            }
            if cursor.next_idx < evicted {
                cursor.next_idx = evicted;
            }
            if cursor.next_idx >= total {
                cursor.next_idx = total - 1;
            }
            if let Some(first_t) = target_ts.first().copied() {
                if let Some((cur_t, _)) = series.get_transition_by_index(cursor.next_idx) {
                    if cur_t > first_t {
                        cursor.next_idx = evicted;
                    }
                }
            }

            let mut cur_pair = series.get_transition_by_index(cursor.next_idx);
            let mut next_pair = if cursor.next_idx + 1 < total {
                series.get_transition_by_index(cursor.next_idx + 1)
            } else {
                None
            };

            for &t in target_ts {
                while let Some((t1, c1)) = next_pair {
                    if t1 <= t {
                        cursor.next_idx += 1;
                        cur_pair = Some((t1, c1));
                        next_pair = if cursor.next_idx + 1 < total {
                            series.get_transition_by_index(cursor.next_idx + 1)
                        } else {
                            None
                        };
                    } else {
                        break;
                    }
                }
                let code = match cur_pair {
                    Some((t0, c0)) if t >= t0 => f64::from(c0),
                    _ => series.get_state_at(t).map_or(0.0, f64::from),
                };
                val_out.push(code);
            }
        }
        ChannelStore::Frame { .. } => {
            val_out.resize(target_ts.len(), 0.0);
        }
    }
}

/// Top-level virtual channel DAG coordinator wired into `CoreWorkspace`.
pub struct VirtualDagEngine {
    inner: Mutex<DagInner>,
    flag: Arc<DagWatermarkFlag>,
    node_count: AtomicUsize,
}

impl std::fmt::Debug for VirtualDagEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VirtualDagEngine")
            .field("node_count", &self.node_count.load(Ordering::Relaxed))
            .finish()
    }
}

impl Default for VirtualDagEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl VirtualDagEngine {
    /// Creates an empty [`VirtualDagEngine`].
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(DagInner::new()),
            flag: Arc::new(DagWatermarkFlag::new()),
            node_count: AtomicUsize::new(0),
        }
    }

    /// Returns the lock-free [`WatermarkListener`] to register on [`ChannelHub`].
    pub fn watermark_listener(&self) -> Arc<dyn WatermarkListener> {
        Arc::clone(&self.flag) as Arc<dyn WatermarkListener>
    }

    /// Fast non-blocking incremental step called by acquisition worker threads after ingest.
    pub fn step_if_active(&self, hub: &ChannelHub) {
        if self.node_count.load(Ordering::Acquire) == 0 {
            return;
        }
        if !self.flag.take_dirty() {
            return;
        }
        let Some(mut guard) = self.inner.try_lock() else {
            self.flag.set_dirty();
            return;
        };
        guard.process_all(hub, &self.flag, false);
    }

    /// Blocking flush called prior to frame packing, trigger scans, or channel tree snapshots.
    pub fn flush_if_dirty(&self, hub: &ChannelHub) {
        if self.node_count.load(Ordering::Acquire) == 0 {
            return;
        }
        if !self.flag.take_dirty() {
            return;
        }
        let mut guard = self.inner.lock();
        guard.process_all(hub, &self.flag, true);
    }

    /// Lists all configured virtual nodes sorted deterministically by `node_id`.
    pub fn list_nodes(&self) -> Vec<VirtualNodeConfig> {
        let guard = self.inner.lock();
        let mut list: Vec<VirtualNodeConfig> =
            guard.nodes.values().map(|rt| rt.config.clone()).collect();
        list.sort_by(|a, b| a.node_id.cmp(&b.node_id));
        list
    }

    /// Registers or replaces a virtual operator node, validates topology, preserves compatible
    /// `ChannelId`s on edit, resets transitive descendants, and performs retroactive backfill.
    ///
    /// Returns `(summary, reset_channel_ids)`.
    pub fn upsert_node(
        &self,
        hub: &ChannelHub,
        config: VirtualNodeConfig,
    ) -> CoreResult<(VirtualNodeSummary, Vec<ChannelId>)> {
        let output_descs = config.output_descriptors()?;
        let input_paths = config.input_paths();
        let op = OperatorState::from_config(&config, &input_paths)?;

        let mut guard = self.inner.lock();
        let node_id = config.node_id.clone();

        let previous_ids_by_path: HashMap<String, ChannelId> = guard
            .nodes
            .get(&node_id)
            .map(|prev| {
                prev.outputs
                    .iter()
                    .map(|e| (e.descriptor().local_path.clone(), e.id()))
                    .collect()
            })
            .unwrap_or_default();

        // Tentatively build candidate node map to verify conflict-free outputs & acyclic DAG
        let candidate_rt = NodeRuntime {
            config: config.clone(),
            input_paths,
            output_descs: output_descs.clone(),
            outputs: Vec::new(),
            cursors: InputCursors::default(),
            op,
        };
        let old_rt = guard.nodes.insert(node_id.clone(), candidate_rt);
        let new_topo = match DagInner::compute_topo_order(&guard.nodes) {
            Ok(order) => order,
            Err(err) => {
                if let Some(prev) = old_rt {
                    guard.nodes.insert(node_id, prev);
                } else {
                    guard.nodes.remove(&node_id);
                }
                return Err(err);
            }
        };
        guard.topo_order = new_topo;

        // Collect all transitive dependents (including this node) before resetting channels
        let affected_nodes = guard.collect_descendants(std::slice::from_ref(&node_id));

        // Remove previous outputs for this node so series start clean & preserve ChannelIds by local_path
        if let Some(prev) = old_rt {
            for entry in prev.outputs {
                self.flag.mark_virtual(entry.id(), false);
                hub.remove_channel(entry.id());
            }
        }

        let mut new_outputs = Vec::with_capacity(output_descs.len());
        let mut reset_channel_ids = Vec::new();
        for mut desc in output_descs {
            if let Some(&prev_id) = previous_ids_by_path.get(&desc.local_path) {
                desc.id = Some(prev_id);
            }
            let entry = hub.register_or_get(desc)?;
            self.flag.mark_virtual(entry.id(), true);
            reset_channel_ids.push(entry.id());
            new_outputs.push(entry);
        }

        if let Some(rt) = guard.nodes.get_mut(&node_id) {
            rt.outputs = new_outputs;
            rt.reset_runtime();
        }

        // Also clear and reset all downstream virtual nodes so they backfill from updated data
        let affected_ordered: Vec<String> = guard
            .topo_order
            .iter()
            .filter(|id| *id != &node_id && affected_nodes.contains(*id))
            .cloned()
            .collect();

        for dep_id in affected_ordered {
            if let Some(dep_rt) = guard.nodes.get_mut(&dep_id) {
                let prev_entries = std::mem::take(&mut dep_rt.outputs);
                let mut preserved_ids = HashMap::new();
                for entry in prev_entries {
                    preserved_ids.insert(entry.descriptor().local_path.clone(), entry.id());
                    self.flag.mark_virtual(entry.id(), false);
                    hub.remove_channel(entry.id());
                }
                let mut re_registered = Vec::with_capacity(dep_rt.output_descs.len());
                for mut desc in dep_rt.output_descs.clone() {
                    if let Some(&old_id) = preserved_ids.get(&desc.local_path) {
                        desc.id = Some(old_id);
                    }
                    let entry = hub.register_or_get(desc)?;
                    self.flag.mark_virtual(entry.id(), true);
                    reset_channel_ids.push(entry.id());
                    re_registered.push(entry);
                }
                dep_rt.outputs = re_registered;
                dep_rt.reset_runtime();
            }
        }

        self.node_count.store(guard.nodes.len(), Ordering::Release);
        guard.process_all(hub, &self.flag, true);

        let summary = guard
            .nodes
            .get(&node_id)
            .expect("inserted node must exist")
            .summary();
        Ok((summary, reset_channel_ids))
    }

    /// Removes a virtual node by `node_id` and cleans up its output channels in [`ChannelHub`].
    pub fn remove_node(&self, hub: &ChannelHub, node_id: &str) -> CoreResult<bool> {
        let mut guard = self.inner.lock();
        let Some(removed) = guard.nodes.remove(node_id) else {
            return Ok(false);
        };
        for entry in removed.outputs {
            self.flag.mark_virtual(entry.id(), false);
            hub.remove_channel(entry.id());
        }
        guard.topo_order = DagInner::compute_topo_order(&guard.nodes)?;
        self.node_count.store(guard.nodes.len(), Ordering::Release);
        Ok(true)
    }

    /// Removes all virtual nodes and their registered channels from [`ChannelHub`].
    pub fn clear_nodes(&self, hub: &ChannelHub) -> usize {
        let mut guard = self.inner.lock();
        let count = guard.nodes.len();
        for (_id, rt) in guard.nodes.drain() {
            for entry in rt.outputs {
                self.flag.mark_virtual(entry.id(), false);
                hub.remove_channel(entry.id());
            }
        }
        guard.topo_order.clear();
        self.node_count.store(0, Ordering::Release);
        self.flag.take_dirty();
        count
    }
}
