//! Engine plugin registries and external descriptor / C-ABI plugin host re-exports.

pub use ddscope_plugin_api::{
    DDSCOPE_C_ABI_VERSION, PLUGIN_API_VERSION, PROTOCOL_PLUGIN_SCHEMA_V1,
    WIDGET_PLUGIN_SCHEMA_V1,
};
pub use ddscope_protocol::plugin_host::{
    discover_and_register_external_plugins, discover_external_plugins, read_widget_plugin_bundle,
    resolve_plugin_search_dirs, ExternalPluginCatalog, ExternalProtocolPluginEntry,
    ExternalWidgetPluginEntry, LoadedWidgetBundle,
};
pub use ddscope_protocol::{ProtocolCodecFactory, ProtocolCodecFactoryFn};
pub use ddscope_transport::TransportFactoryFn;
