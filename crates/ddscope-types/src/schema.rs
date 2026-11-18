//! Channel descriptors, struct schemas, and multi-modal channel metadata.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::ids::{ChannelId, ChannelPath, SourceId};
use super::scalar::{Endianness, LinearScale, ScalarType};

/// Axis coordinate metadata for `FrameSeries` channels (e.g., FFT spectrum frequency axis).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AxisMeta {
    pub origin: f64,
    pub step: f64,
    pub unit: String,
}

/// Display metadata for an enumerated state machine value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateLabel {
    pub name: String,
    pub color_rgba: Option<u32>,
}

/// Mapping from numeric state codes to human-readable labels and optional ribbon colors.
pub type StateEnumMap = BTreeMap<i32, StateLabel>;

/// Bitfield extraction coordinates inside an integer carrier field.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BitfieldSpec {
    pub bit_offset: u8,
    pub bit_width: u8,
}

/// Field type definition inside a declarative or imported C [`StructSchema`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum StructFieldKind {
    Scalar {
        scalar_type: ScalarType,
        endian: Endianness,
        scale: LinearScale,
        bitfield: Option<BitfieldSpec>,
    },
    Enum {
        base_type: ScalarType,
        endian: Endianness,
        labels: StateEnumMap,
    },
    FixedArray {
        element: ScalarType,
        endian: Endianness,
        len: usize,
    },
    Nested(Arc<StructSchema>),
}

/// Single field entry within a [`StructSchema`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StructFieldDef {
    pub name: String,
    pub offset_bytes: Option<usize>,
    pub unit: Option<String>,
    pub description: Option<String>,
    pub kind: StructFieldKind,
}

/// Declarative structured message schema (forward-compatibility hook for `.ddproto` & C-struct import).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StructSchema {
    pub name: String,
    pub msg_id: Option<u32>,
    pub packed: bool,
    pub fields: Vec<StructFieldDef>,
}

/// Payload schema classification for an `EventSeries` channel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum EventSchemaKind {
    StateMachine { labels: StateEnumMap },
    TextLog,
    StructSnapshot(Arc<StructSchema>),
}

/// Orthogonal channel modality classification (`Scalar`, `Frame`, or `Event`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ChannelKind {
    /// Time-series of scalar samples or 1-bit digital logic signals.
    Scalar {
        scalar_type: ScalarType,
        calibration: LinearScale,
    },
    /// Reserved for Phase 2 array/spectrum frames (`FrameSeries` forward-compatibility hook).
    Frame {
        element_type: ScalarType,
        frame_len: usize,
        axis: AxisMeta,
    },
    /// Discrete state machine transitions, text logs, or struct snapshots.
    Event { schema: EventSchemaKind },
}

/// Static registration descriptor for a channel published by a protocol or virtual DAG.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChannelDescriptor {
    pub id: Option<ChannelId>,
    pub source_id: SourceId,
    pub local_path: String,
    pub display_name: Option<String>,
    pub unit: Option<String>,
    pub kind: ChannelKind,
}

impl ChannelDescriptor {
    /// Creates a scalar numeric channel descriptor with default identity calibration.
    pub fn new_scalar(
        source_id: impl Into<SourceId>,
        local_path: impl Into<String>,
        scalar_type: ScalarType,
    ) -> Self {
        Self {
            id: None,
            source_id: source_id.into(),
            local_path: local_path.into(),
            display_name: None,
            unit: None,
            kind: ChannelKind::Scalar {
                scalar_type,
                calibration: LinearScale::identity(),
            },
        }
    }

    /// Creates a 1-bit boolean digital logic channel descriptor.
    pub fn new_bool(source_id: impl Into<SourceId>, local_path: impl Into<String>) -> Self {
        Self::new_scalar(source_id, local_path, ScalarType::Bool)
    }

    /// Creates a state-machine event channel descriptor.
    pub fn new_state_machine(
        source_id: impl Into<SourceId>,
        local_path: impl Into<String>,
        labels: StateEnumMap,
    ) -> Self {
        Self {
            id: None,
            source_id: source_id.into(),
            local_path: local_path.into(),
            display_name: None,
            unit: None,
            kind: ChannelKind::Event {
                schema: EventSchemaKind::StateMachine { labels },
            },
        }
    }

    /// Builds the full global [`ChannelPath`] `<source_id>/<local_path>`.
    pub fn full_path(&self) -> ChannelPath {
        ChannelPath::new(&self.source_id, &self.local_path)
    }
}
