//! Strongly-typed identifiers and global channel path definitions.

use std::fmt;

use bytemuck::{Pod, Zeroable};
use serde::{Deserialize, Serialize};

/// Monotonic timestamp in nanoseconds relative to the unified workspace clock.
pub type TimestampNs = u64;

/// Unique identifier of a data source session (e.g., `"sim0"`, `"uart1"`, `"udp0"`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SourceId(pub String);

impl SourceId {
    /// Creates a new `SourceId`.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Returns the underlying string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for SourceId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl From<String> for SourceId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl AsRef<str> for SourceId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Compact numeric identifier assigned to a registered channel in `ChannelHub`.
#[repr(transparent)]
#[derive(
    Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Pod, Zeroable,
)]
pub struct ChannelId(pub u32);

impl fmt::Display for ChannelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ch#{}", self.0)
    }
}

/// Compact numeric identifier of a frontend oscilloscope viewport controller.
#[repr(transparent)]
#[derive(
    Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Pod, Zeroable,
)]
pub struct ScopeId(pub u32);

impl fmt::Display for ScopeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "scope#{}", self.0)
    }
}

/// Compact numeric identifier of an oscilloscope timebase/cursor synchronization group.
#[repr(transparent)]
#[derive(
    Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Pod, Zeroable,
)]
pub struct SyncGroupId(pub u32);

impl fmt::Display for SyncGroupId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sync#{}", self.0)
    }
}

/// Hierarchical global channel path formatted as `<source_id>/<local_path>`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChannelPath(pub String);

impl ChannelPath {
    /// Constructs a canonical global channel path `<source_id>/<local_path>`.
    pub fn new(source_id: &SourceId, local_path: &str) -> Self {
        let trimmed = local_path.trim_start_matches('/');
        Self(format!("{}/{}", source_id.as_str(), trimmed))
    }

    /// Splits the path into `(source_id, local_path)` on the first `'/'` separator.
    pub fn split(&self) -> Option<(&str, &str)> {
        self.0.split_once('/')
    }

    /// Returns the full channel path string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ChannelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
