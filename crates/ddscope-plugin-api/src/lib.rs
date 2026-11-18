//! Stable plugin SDK contract definitions for DDScopeHost.
//!
//! Covers both C-ABI native dynamic library (`cdylib`) protocol plugins and
//! external `plugin.json` manifest descriptors for protocols and widgets.

pub mod cabi;
pub mod manifest;

pub use cabi::{
    CBatchSink, CByteSlice, CChannelDesc, CChannelSink, CErrorSink, CScalarType,
    ProtocolPluginEntryFnV1, ProtocolPluginVTableV1, DDSCOPE_C_ABI_VERSION,
    DDSCOPE_PROTOCOL_SYMBOL_V1,
};
pub use manifest::{
    is_safe_relative_asset_path, is_valid_plugin_id, DeclarativeBinaryField,
    DeclarativeProtocolSpec, DeclarativeTypedChannel, LocalizedText, PlatformLibrarySpec,
    PluginChannelPreview, PluginParamSpec, PluginParamType, PluginSelectOption,
    ProtocolPluginBackend, ProtocolPluginManifest, WidgetBindingMode, WidgetPluginManifest,
    PROTOCOL_PLUGIN_SCHEMA_V1, WIDGET_PLUGIN_SCHEMA_V1,
};

/// Current version of the `ddscope-plugin-api` crate.
pub const PLUGIN_API_VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_api_version_and_id_validation() {
        assert!(!PLUGIN_API_VERSION.is_empty());
        assert!(is_valid_plugin_id("bar_level_monitor"));
        assert!(is_valid_plugin_id("scaled-int16-frame"));
        assert!(!is_valid_plugin_id(""));
        assert!(!is_valid_plugin_id("../evil"));
        assert!(!is_valid_plugin_id("bad/id"));
        assert!(!is_valid_plugin_id("UpperCase"));
        assert!(is_safe_relative_asset_path("index.js"));
        assert!(is_safe_relative_asset_path("assets/style.css"));
        assert!(!is_safe_relative_asset_path("../secret.js"));
        assert!(!is_safe_relative_asset_path("/etc/passwd"));
        assert!(!is_safe_relative_asset_path("C:\\evil.js"));
    }

    #[test]
    fn protocol_and_widget_manifests_deserialize() {
        let proto_json = r#"{
          "schema": "ddscope.protocol-plugin.v1",
          "id": "demo_proto",
          "name": { "en": "Demo Proto", "zh-CN": "示例协议" },
          "version": "1.0.0",
          "params": [
            {
              "key": "scale",
              "label": { "en": "Scale", "zh-CN": "比例" },
              "type": "number",
              "default": 0.01
            }
          ],
          "backend": {
            "mode": "declarative",
            "framing": "fixed_binary",
            "header_hex": "AA 55",
            "fields": [
              { "name": "v", "scalar_type": "i16", "scale_param": "scale" }
            ]
          }
        }"#;
        let proto: ProtocolPluginManifest = serde_json::from_str(proto_json).unwrap();
        assert_eq!(proto.id, "demo_proto");
        assert_eq!(proto.name.resolve("zh-CN"), "示例协议");
        assert_eq!(proto.backend.mode_tag(), "declarative");

        let lib_spec = PlatformLibrarySpec::Stem("ddscope_plugin_xor_demo".to_string());
        assert!(lib_spec.filename_for_current_os().is_some());
    }
}
