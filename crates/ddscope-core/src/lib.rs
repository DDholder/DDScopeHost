//! Pure-Rust ultra-high-speed acquisition, storage, and oscilloscope core engine.
//!
//! Strictly decoupled from Tauri, WebView, and UI frameworks.

pub mod clock;
pub mod dag;
pub mod plugin;
pub mod protocol;
pub mod scope;
pub mod storage;
pub mod transport;
pub mod types;
pub mod workspace;

pub use scope::ddsf;

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
