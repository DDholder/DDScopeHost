//! Stable plugin SDK contract definitions for DDScopeHost.
//!
//! Phase 1 uses static engine registration; future phases will build
//! WASM (WIT) and C-ABI dynamic plugin hosts on top of these boundaries.

/// Current version of the `ddscope-plugin-api` crate.
pub const PLUGIN_API_VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_api_version_is_non_empty() {
        assert!(!PLUGIN_API_VERSION.is_empty());
    }
}
