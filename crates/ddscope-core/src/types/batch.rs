//! Reusable Structure-of-Arrays (SoA) batch buffer emitted by `ProtocolCodec::feed_bytes`.

use super::ids::TimestampNs;
use super::scalar::{ScalarSlice, ScalarType};
use super::schema::{ChannelDescriptor, ChannelKind, EventSchemaKind};

/// Inbound acknowledgment event extracted from an uplink stream (Phase 2 ACK tracker hook `H1`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InboundAckEvent {
    pub msg_id: Option<u32>,
    pub echo_seq: Option<u64>,
    pub status_code: i64,
    pub recv_ns: TimestampNs,
}

/// Column buffer inside [`DecodedBatch`] that retains its heap allocation across `clear()` calls.
#[derive(Clone, Debug, PartialEq)]
pub enum DecodedColumnBuf {
    Bool(Vec<bool>),
    U8(Vec<u8>),
    U16(Vec<u16>),
    U32(Vec<u32>),
    U64(Vec<u64>),
    I8(Vec<i8>),
    I16(Vec<i16>),
    I32(Vec<i32>),
    I64(Vec<i64>),
    F32(Vec<f32>),
    F64(Vec<f64>),
    StateEnum(Vec<i32>),
}

impl DecodedColumnBuf {
    /// Allocates a column buffer appropriate for the given [`ChannelKind`].
    pub fn for_channel_kind(kind: &ChannelKind, cap: usize) -> Self {
        match kind {
            ChannelKind::Scalar { scalar_type, .. } => match scalar_type {
                ScalarType::Bool => Self::Bool(Vec::with_capacity(cap)),
                ScalarType::U8 => Self::U8(Vec::with_capacity(cap)),
                ScalarType::U16 => Self::U16(Vec::with_capacity(cap)),
                ScalarType::U32 => Self::U32(Vec::with_capacity(cap)),
                ScalarType::U64 => Self::U64(Vec::with_capacity(cap)),
                ScalarType::I8 => Self::I8(Vec::with_capacity(cap)),
                ScalarType::I16 => Self::I16(Vec::with_capacity(cap)),
                ScalarType::I32 => Self::I32(Vec::with_capacity(cap)),
                ScalarType::I64 => Self::I64(Vec::with_capacity(cap)),
                ScalarType::F32 => Self::F32(Vec::with_capacity(cap)),
                ScalarType::F64 => Self::F64(Vec::with_capacity(cap)),
            },
            ChannelKind::Frame { element_type, frame_len, .. } => {
                let total = cap.saturating_mul(*frame_len);
                match element_type {
                    ScalarType::Bool => Self::Bool(Vec::with_capacity(total)),
                    ScalarType::U8 => Self::U8(Vec::with_capacity(total)),
                    ScalarType::U16 => Self::U16(Vec::with_capacity(total)),
                    ScalarType::U32 => Self::U32(Vec::with_capacity(total)),
                    ScalarType::U64 => Self::U64(Vec::with_capacity(total)),
                    ScalarType::I8 => Self::I8(Vec::with_capacity(total)),
                    ScalarType::I16 => Self::I16(Vec::with_capacity(total)),
                    ScalarType::I32 => Self::I32(Vec::with_capacity(total)),
                    ScalarType::I64 => Self::I64(Vec::with_capacity(total)),
                    ScalarType::F32 => Self::F32(Vec::with_capacity(total)),
                    ScalarType::F64 => Self::F64(Vec::with_capacity(total)),
                }
            }
            ChannelKind::Event {
                schema: EventSchemaKind::StateMachine { .. },
            } => Self::StateEnum(Vec::with_capacity(cap)),
            ChannelKind::Event { .. } => Self::I32(Vec::with_capacity(cap)),
        }
    }

    /// Returns the number of elements currently buffered in this column.
    pub fn len(&self) -> usize {
        match self {
            Self::Bool(v) => v.len(),
            Self::U8(v) => v.len(),
            Self::U16(v) => v.len(),
            Self::U32(v) => v.len(),
            Self::U64(v) => v.len(),
            Self::I8(v) => v.len(),
            Self::I16(v) => v.len(),
            Self::I32(v) => v.len(),
            Self::I64(v) => v.len(),
            Self::F32(v) => v.len(),
            Self::F64(v) => v.len(),
            Self::StateEnum(v) => v.len(),
        }
    }

    /// Returns `true` if this column currently holds zero elements.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Clears buffered values while preserving heap capacity.
    pub fn clear(&mut self) {
        match self {
            Self::Bool(v) => v.clear(),
            Self::U8(v) => v.clear(),
            Self::U16(v) => v.clear(),
            Self::U32(v) => v.clear(),
            Self::U64(v) => v.clear(),
            Self::I8(v) => v.clear(),
            Self::I16(v) => v.clear(),
            Self::I32(v) => v.clear(),
            Self::I64(v) => v.clear(),
            Self::F32(v) => v.clear(),
            Self::F64(v) => v.clear(),
            Self::StateEnum(v) => v.clear(),
        }
    }

    /// Borrows this column as a [`ScalarSlice`], or `None` for `StateEnum`.
    pub fn as_scalar_slice(&self) -> Option<ScalarSlice<'_>> {
        match self {
            Self::Bool(v) => Some(ScalarSlice::Bool(v)),
            Self::U8(v) => Some(ScalarSlice::U8(v)),
            Self::U16(v) => Some(ScalarSlice::U16(v)),
            Self::U32(v) => Some(ScalarSlice::U32(v)),
            Self::U64(v) => Some(ScalarSlice::U64(v)),
            Self::I8(v) => Some(ScalarSlice::I8(v)),
            Self::I16(v) => Some(ScalarSlice::I16(v)),
            Self::I32(v) => Some(ScalarSlice::I32(v)),
            Self::I64(v) => Some(ScalarSlice::I64(v)),
            Self::F32(v) => Some(ScalarSlice::F32(v)),
            Self::F64(v) => Some(ScalarSlice::F64(v)),
            Self::StateEnum(_) => None,
        }
    }

    /// Borrows this column as state-machine enum codes when variant is `StateEnum`.
    pub fn as_state_slice(&self) -> Option<&[i32]> {
        match self {
            Self::StateEnum(v) => Some(v),
            _ => None,
        }
    }
}

/// Reusable column-oriented batch populated by `ProtocolCodec::feed_bytes`.
#[derive(Clone, Debug, Default)]
pub struct DecodedBatch {
    pub sample_rows: usize,
    pub packet_recv_ns: TimestampNs,
    pub hw_ticks: Vec<u64>,
    pub timestamps_ns: Vec<u64>,
    pub columns: Vec<DecodedColumnBuf>,
    pub ack_events: Vec<InboundAckEvent>,
}

impl DecodedBatch {
    /// Pre-allocates column buffers matching `descs` for `row_cap` rows.
    pub fn from_descriptors(descs: &[ChannelDescriptor], row_cap: usize) -> Self {
        let columns = descs
            .iter()
            .map(|d| DecodedColumnBuf::for_channel_kind(&d.kind, row_cap))
            .collect();
        Self {
            sample_rows: 0,
            packet_recv_ns: 0,
            hw_ticks: Vec::with_capacity(row_cap),
            timestamps_ns: Vec::with_capacity(row_cap),
            columns,
            ack_events: Vec::new(),
        }
    }

    /// Resets row counters and clears all column vectors while retaining allocated capacity.
    pub fn clear(&mut self) {
        self.sample_rows = 0;
        self.packet_recv_ns = 0;
        self.hw_ticks.clear();
        self.timestamps_ns.clear();
        for col in &mut self.columns {
            col.clear();
        }
        self.ack_events.clear();
    }

    /// Returns `true` when zero sample rows and zero ACK events were decoded.
    pub fn is_empty(&self) -> bool {
        self.sample_rows == 0 && self.ack_events.is_empty()
    }
}
