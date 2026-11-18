//! Pure-Rust ultra-high-speed acquisition, storage, and oscilloscope core engine.
//!
//! Strictly decoupled from Tauri, WebView, and UI frameworks.

pub use ddscope_clock as clock;
pub use ddscope_dag as dag;
pub use ddscope_protocol as protocol;
pub use ddscope_scope as scope;
pub use ddscope_storage as storage;
pub use ddscope_transport as transport;
pub use ddscope_types as types;

pub mod plugin;
pub mod workspace;

pub use dag::{
    AnalogToDigitalMode, CalculusMode, CompiledFormula, PowerOutputFlags, SinglePhasePowerParams,
    SinglePhasePowerSample, SinglePhasePowerState, StateLabelSpec, StateRangeRule, StateValueEntry,
    SwagWindowTracker, ToAnalogMode, ToStateMachineMode, VirtualDagEngine, VirtualNodeConfig,
    VirtualNodeSummary, VirtualOperatorConfig, WindowMetricKind, VIRTUAL_SOURCE_ID,
};
pub use scope::ddsf;
pub use workspace::{
    ChannelTreeSnapshot, CoreWorkspace, RegisteredChannelInfo, ScopeConfig, ScopeLaneKind,
    ScopeSummary, ScopeTrackBinding, SourceChannelGroup, SourceConfig, SourceProtocolConfig,
    SourceRuntimeState, SourceSession, SourceSummary, SourceTelemetrySnapshot,
    SourceTransportConfig, SyncGroupUpdate, WorkspaceTelemetrySnapshot,
};

use serde::{Deserialize, Serialize};

/// Build and capability metadata for `ddscope-core`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoreBuildInfo {
    /// Crate package name.
    pub name: &'static str,
    /// Crate semantic version.
    pub version: &'static str,
    /// Plugin API contract version compiled into this core.
    pub plugin_api_version: &'static str,
}

/// Returns build metadata for `ddscope-core`.
pub fn build_info() -> CoreBuildInfo {
    CoreBuildInfo {
        name: env!("CARGO_PKG_NAME"),
        version: env!("CARGO_PKG_VERSION"),
        plugin_api_version: ddscope_plugin_api::PLUGIN_API_VERSION,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_info_matches_package_metadata() {
        let info = build_info();
        assert_eq!(info.name, "ddscope-core");
        assert!(!info.version.is_empty());
        assert_eq!(info.plugin_api_version, ddscope_plugin_api::PLUGIN_API_VERSION);
    }
}
