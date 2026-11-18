//! Virtual channel DAG (`N Inputs -> M Outputs`), signal converters, formula compiler,
//! sliding window statistics, calculus, and IEEE 1459 single-phase power analysis.

pub mod config;
pub mod engine;
pub mod formula;
pub mod power;
pub mod window_stats;

pub use config::{
    AnalogToDigitalMode, CalculusMode, PowerOutputFlags, StateLabelSpec, StateRangeRule,
    StateValueEntry, ToAnalogMode, ToStateMachineMode, VirtualNodeConfig, VirtualNodeSummary,
    VirtualOperatorConfig, WindowMetricKind, VIRTUAL_SOURCE_ID,
};
pub use engine::{DagWatermarkFlag, VirtualDagEngine};
pub use formula::CompiledFormula;
pub use power::{SinglePhasePowerParams, SinglePhasePowerSample, SinglePhasePowerState};
pub use window_stats::SwagWindowTracker;
