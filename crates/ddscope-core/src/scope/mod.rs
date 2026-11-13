//! Per-scope independent trigger state machine, synchronization group, and DDSF v1 wire contract.

pub mod ddsf;

use serde::{Deserialize, Serialize};

use crate::types::ChannelId;

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
