//! Native scalar representations, endianness, and linear calibration helpers.

use serde::{Deserialize, Serialize};

/// Native storage width of a scalar channel sample.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ScalarType {
    Bool,
    U8,
    U16,
    U32,
    U64,
    I8,
    I16,
    I32,
    I64,
    F32,
    F64,
}

impl ScalarType {
    /// Returns the storage bit width per sample (`1` for `Bool`).
    pub const fn bit_width(self) -> usize {
        match self {
            Self::Bool => 1,
            Self::U8 | Self::I8 => 8,
            Self::U16 | Self::I16 => 16,
            Self::U32 | Self::I32 | Self::F32 => 32,
            Self::U64 | Self::I64 | Self::F64 => 64,
        }
    }

    /// Returns packed storage byte size for `sample_count` samples (`u64`-word aligned for `Bool`).
    pub const fn byte_size_for_len(self, sample_count: usize) -> usize {
        match self {
            Self::Bool => sample_count.div_ceil(64) * 8,
            _ => sample_count * (self.bit_width() / 8),
        }
    }

    /// Returns `true` if this type represents a 1-bit boolean logic signal.
    pub const fn is_boolean(self) -> bool {
        matches!(self, Self::Bool)
    }

    /// Returns `true` if this type is IEEE-754 floating point.
    pub const fn is_floating_point(self) -> bool {
        matches!(self, Self::F32 | Self::F64)
    }

    /// Returns `true` if this type can represent negative values.
    pub const fn is_signed(self) -> bool {
        matches!(
            self,
            Self::I8 | Self::I16 | Self::I32 | Self::I64 | Self::F32 | Self::F64
        )
    }
}

/// Byte order used when decoding or encoding multi-byte wire fields.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Endianness {
    #[default]
    Little,
    Big,
}

/// Linear physical unit calibration (`physical = raw * scale + offset`).
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LinearScale {
    pub scale: f64,
    pub offset: f64,
}

impl Default for LinearScale {
    fn default() -> Self {
        Self::identity()
    }
}

impl LinearScale {
    /// Creates a linear calibration transform `raw * scale + offset`.
    pub const fn new(scale: f64, offset: f64) -> Self {
        Self { scale, offset }
    }

    /// Identity transform (`scale = 1.0`, `offset = 0.0`).
    pub const fn identity() -> Self {
        Self {
            scale: 1.0,
            offset: 0.0,
        }
    }

    /// Returns `true` when this transform is an exact no-op.
    pub fn is_identity(&self) -> bool {
        self.scale == 1.0 && self.offset == 0.0
    }

    /// Converts a raw numeric value to physical `f64` units.
    #[inline]
    pub fn apply(&self, raw: f64) -> f64 {
        raw * self.scale + self.offset
    }

    /// Converts a raw numeric value to physical `f32` units for GPU/M4 rendering.
    #[inline]
    pub fn apply_f32(&self, raw: f64) -> f32 {
        self.apply(raw) as f32
    }
}

/// Owned single scalar value preserving native type.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ScalarValue {
    Bool(bool),
    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),
    I8(i8),
    I16(i16),
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
}

impl ScalarValue {
    /// Returns the corresponding [`ScalarType`].
    pub const fn scalar_type(&self) -> ScalarType {
        match self {
            Self::Bool(_) => ScalarType::Bool,
            Self::U8(_) => ScalarType::U8,
            Self::U16(_) => ScalarType::U16,
            Self::U32(_) => ScalarType::U32,
            Self::U64(_) => ScalarType::U64,
            Self::I8(_) => ScalarType::I8,
            Self::I16(_) => ScalarType::I16,
            Self::I32(_) => ScalarType::I32,
            Self::I64(_) => ScalarType::I64,
            Self::F32(_) => ScalarType::F32,
            Self::F64(_) => ScalarType::F64,
        }
    }

    /// Converts this value to `f64` for intermediate DSP or display calculations.
    #[inline]
    pub fn as_f64(&self) -> f64 {
        match *self {
            Self::Bool(v) => {
                if v {
                    1.0
                } else {
                    0.0
                }
            }
            Self::U8(v) => f64::from(v),
            Self::U16(v) => f64::from(v),
            Self::U32(v) => f64::from(v),
            Self::U64(v) => v as f64,
            Self::I8(v) => f64::from(v),
            Self::I16(v) => f64::from(v),
            Self::I32(v) => f64::from(v),
            Self::I64(v) => v as f64,
            Self::F32(v) => f64::from(v),
            Self::F64(v) => v,
        }
    }
}

/// Borrowed slice of native scalar samples for zero-copy batch ingestion.
#[derive(Copy, Clone, Debug)]
pub enum ScalarSlice<'a> {
    Bool(&'a [bool]),
    U8(&'a [u8]),
    U16(&'a [u16]),
    U32(&'a [u32]),
    U64(&'a [u64]),
    I8(&'a [i8]),
    I16(&'a [i16]),
    I32(&'a [i32]),
    I64(&'a [i64]),
    F32(&'a [f32]),
    F64(&'a [f64]),
}

impl<'a> ScalarSlice<'a> {
    /// Returns the number of samples in this slice.
    pub const fn len(&self) -> usize {
        match self {
            Self::Bool(s) => s.len(),
            Self::U8(s) => s.len(),
            Self::U16(s) => s.len(),
            Self::U32(s) => s.len(),
            Self::U64(s) => s.len(),
            Self::I8(s) => s.len(),
            Self::I16(s) => s.len(),
            Self::I32(s) => s.len(),
            Self::I64(s) => s.len(),
            Self::F32(s) => s.len(),
            Self::F64(s) => s.len(),
        }
    }

    /// Returns `true` if the slice contains zero samples.
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the [`ScalarType`] of elements in this slice.
    pub const fn scalar_type(&self) -> ScalarType {
        match self {
            Self::Bool(_) => ScalarType::Bool,
            Self::U8(_) => ScalarType::U8,
            Self::U16(_) => ScalarType::U16,
            Self::U32(_) => ScalarType::U32,
            Self::U64(_) => ScalarType::U64,
            Self::I8(_) => ScalarType::I8,
            Self::I16(_) => ScalarType::I16,
            Self::I32(_) => ScalarType::I32,
            Self::I64(_) => ScalarType::I64,
            Self::F32(_) => ScalarType::F32,
            Self::F64(_) => ScalarType::F64,
        }
    }

    /// Reads sample `idx` converted to `f64`, or `None` if out of bounds.
    #[inline]
    pub fn get_as_f64(&self, idx: usize) -> Option<f64> {
        match self {
            Self::Bool(s) => s.get(idx).map(|&v| if v { 1.0 } else { 0.0 }),
            Self::U8(s) => s.get(idx).copied().map(f64::from),
            Self::U16(s) => s.get(idx).copied().map(f64::from),
            Self::U32(s) => s.get(idx).copied().map(f64::from),
            Self::U64(s) => s.get(idx).copied().map(|v| v as f64),
            Self::I8(s) => s.get(idx).copied().map(f64::from),
            Self::I16(s) => s.get(idx).copied().map(f64::from),
            Self::I32(s) => s.get(idx).copied().map(f64::from),
            Self::I64(s) => s.get(idx).copied().map(|v| v as f64),
            Self::F32(s) => s.get(idx).copied().map(f64::from),
            Self::F64(s) => s.get(idx).copied(),
        }
    }
}
