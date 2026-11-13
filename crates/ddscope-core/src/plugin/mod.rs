//! Static engine registry and future WASM / C-ABI plugin host boundary.

pub use ddscope_plugin_api::PLUGIN_API_VERSION;

use crate::protocol::ProtocolCodec;
use crate::transport::Transport;
use crate::types::CoreResult;

/// Static factory signature for creating a [`Transport`] instance from serialized JSON config.
pub type TransportFactoryFn = fn(&str) -> CoreResult<Box<dyn Transport>>;

/// Static factory signature for creating a [`ProtocolCodec`] instance from serialized JSON config.
pub type ProtocolCodecFactoryFn = fn(&str) -> CoreResult<Box<dyn ProtocolCodec>>;
