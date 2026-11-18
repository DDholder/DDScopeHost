//! Per-scope independent trigger state machine, synchronization group, and DDSF v1 wire contract.

pub mod controller;
pub mod ddsf;
pub mod sync;

pub use controller::{
    calculate_subsample_offset, HysteresisBandState, ScopeController, ScopeControllerConfig,
    ScopeStepOutcome, ScopeViewportWindow, TriggerEngine, TriggerHit,
};
pub use ddsf::{
    align_up_64, align_up_8, DdsfAnalogPoint, DdsfBlockKind, DdsfFrameHeader, DdsfFramePacker,
    DdsfM4Column, DdsfScopeIter, DdsfScopePacketHeader, DdsfScopePacketView, DdsfSnapshotEntry,
    DdsfSnapshotKind, DdsfStateRibbon, DdsfTrackBlockHeader, DdsfTrackBlockView, DdsfTrackIter,
    DdsfTrackStats, DdsfZeroCopyReader, ScopePacketView, ScopeViewportPayload, TrackBlockView,
    TrackPayload, TrackPayloadSlice, DDSF_FLAG_PINNED_ACTIVE, DDSF_FLAG_QUOTA_WARNING,
    DDSF_HEADER_BYTES, DDSF_MAGIC, DDSF_SCOPE_HEADER_BYTES, DDSF_SNAPSHOT_ENTRY_BYTES,
    DDSF_TRACK_FLAG_CLIPPED, DDSF_TRACK_FLAG_HAS_STATS, DDSF_TRACK_HEADER_BYTES,
    DDSF_TRACK_STATS_BYTES, DDSF_VERSION_V1,
};
pub use sync::{SyncGroupCoordinator, SyncGroupState};

use serde::{Deserialize, Serialize};

use ddscope_types::ChannelId;

/// Sweep and trigger capture mode of an individual oscilloscope viewport.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TriggerSweepMode {
    #[default]
    Roll = 0,
    Auto = 1,
    Normal = 2,
    Single = 3,
}

impl TriggerSweepMode {
    /// Returns the `u8` wire discriminant defined in `DDSF v1`.
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Parses a `u8` wire discriminant into [`TriggerSweepMode`].
    pub const fn from_u8(raw: u8) -> Option<Self> {
        match raw {
            0 => Some(Self::Roll),
            1 => Some(Self::Auto),
            2 => Some(Self::Normal),
            3 => Some(Self::Single),
            _ => None,
        }
    }
}

/// Runtime state of a `ScopeController` matching `DDSF v1` `trigger_state` wire encoding.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TriggerRuntimeState {
    #[default]
    Roll = 0,
    Armed = 1,
    AutoFreerun = 2,
    Triggered = 3,
    LatchedPinned = 4,
}

impl TriggerRuntimeState {
    /// Returns the `u8` wire discriminant defined in `DDSF v1`.
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Parses a `u8` wire discriminant into [`TriggerRuntimeState`].
    pub const fn from_u8(raw: u8) -> Option<Self> {
        match raw {
            0 => Some(Self::Roll),
            1 => Some(Self::Armed),
            2 => Some(Self::AutoFreerun),
            3 => Some(Self::Triggered),
            4 => Some(Self::LatchedPinned),
            _ => None,
        }
    }
}

/// Edge polarity for threshold-crossing triggers.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EdgeSlope {
    #[default]
    Rising,
    Falling,
    Either,
}

/// Trigger evaluation condition executed in `ScopeController`.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum TriggerCondition {
    Edge {
        channel: ChannelId,
        level: f64,
        hysteresis: f64,
        slope: EdgeSlope,
    },
    StateEquals {
        channel: ChannelId,
        state_code: i32,
    },
}
