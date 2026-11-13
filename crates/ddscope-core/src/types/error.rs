//! Unified core engine error types.

use thiserror::Error;

use super::ids::{ScopeId, SourceId};

/// Errors produced across `ddscope-core` subsystems.
#[derive(Debug, Error)]
pub enum CoreError {
    /// Standard I/O failure.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Transport connection or read/write failure.
    #[error("transport error: {0}")]
    Transport(String),

    /// Protocol framing, decoding, or encoding failure.
    #[error("protocol error (recoverable={recoverable}): {message}")]
    Protocol {
        message: String,
        recoverable: bool,
    },

    /// Clock synchronization or PLL failure.
    #[error("clock error: {0}")]
    Clock(String),

    /// Columnar storage or chunk indexing failure.
    #[error("storage error: {0}")]
    Storage(String),

    /// Allocation blocked by the global memory quota limit.
    #[error("memory quota exceeded: requested {requested_bytes} bytes against limit {limit_bytes} bytes")]
    QuotaExceeded {
        requested_bytes: usize,
        limit_bytes: usize,
    },

    /// Requested channel was not registered.
    #[error("channel not found: {0}")]
    ChannelNotFound(String),

    /// Requested oscilloscope viewport controller does not exist.
    #[error("scope not found: {0}")]
    ScopeNotFound(ScopeId),

    /// Requested data source does not exist.
    #[error("source not found: {0}")]
    SourceNotFound(SourceId),

    /// Data source identifier is already registered.
    #[error("source already exists: {0}")]
    SourceAlreadyExists(SourceId),

    /// Invalid configuration parameters.
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    /// Internal lock-free or async channel was closed unexpectedly.
    #[error("internal pipeline channel closed")]
    ChannelClosed,

    /// Operation is unsupported on this transport, codec, or platform.
    #[error("unsupported operation: {0}")]
    Unsupported(String),
}

/// Convenient result alias for `ddscope-core` operations.
pub type CoreResult<T> = Result<T, CoreError>;
