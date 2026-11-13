//! Cache-line aligned columnar buffers, native-width chunks, M4 pyramids, and chunk references.

use std::marker::PhantomData;
use std::mem::{align_of, size_of};
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use bytemuck::{cast_slice, cast_slice_mut, Pod, Zeroable};
use serde::{Deserialize, Serialize};

use super::error::CoreError;
use super::ids::TimestampNs;
use super::scalar::{LinearScale, ScalarSlice, ScalarType, ScalarValue};

/// Fixed sample capacity per sealed scalar chunk (`2^16`).
pub const CHUNK_LEN: usize = 65_536;

/// Cache-line and DDSF frame alignment boundary in bytes.
pub const CACHE_LINE_BYTES: usize = 64;

/// Branching factor for the multi-level M4 envelope pyramid.
pub const M4_BRANCH_FACTOR: usize = 64;

/// Maximum span in nanoseconds representable by 32-bit relative offsets (~4.29s).
pub const MAX_REL_U32_NS: u64 = u32::MAX as u64;

/// 64-byte cache-line aligned storage unit used to construct safe aligned buffers without `unsafe`.
#[repr(C, align(64))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct CacheLineBlock(pub [u64; 8]);

/// Byte buffer guaranteed to be aligned on a 64-byte cache-line boundary.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AlignedBuffer {
    blocks: Vec<CacheLineBlock>,
    len: usize,
}

impl AlignedBuffer {
    /// Creates an empty 64-byte aligned buffer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a buffer with capacity for at least `byte_cap` bytes.
    pub fn with_capacity(byte_cap: usize) -> Self {
        let block_cap = byte_cap.div_ceil(CACHE_LINE_BYTES);
        Self {
            blocks: Vec::with_capacity(block_cap),
            len: 0,
        }
    }

    /// Returns the number of initialized bytes.
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` when the buffer contains zero bytes.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns total allocated capacity in bytes.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.blocks.capacity() * CACHE_LINE_BYTES
    }

    /// Clears the byte length to zero without releasing capacity.
    #[inline]
    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// Resizes the buffer to `new_len` bytes, filling any newly exposed bytes with `fill`.
    pub fn resize(&mut self, new_len: usize, fill: u8) {
        let needed_blocks = new_len.div_ceil(CACHE_LINE_BYTES);
        if needed_blocks > self.blocks.len() {
            let fill_word = u64::from_ne_bytes([fill; 8]);
            self.blocks.resize(needed_blocks, CacheLineBlock([fill_word; 8]));
        }
        if new_len > self.len {
            let old_len = self.len;
            self.len = new_len;
            let bytes = self.as_mut_slice();
            bytes[old_len..new_len].fill(fill);
        } else {
            self.len = new_len;
        }
    }

    /// Appends a slice of bytes to the buffer.
    pub fn extend_from_slice(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        let start = self.len;
        let new_len = start + data.len();
        self.ensure_block_len(new_len);
        self.len = new_len;
        self.as_mut_slice()[start..new_len].copy_from_slice(data);
    }

    /// Returns an immutable byte slice view.
    #[inline]
    pub fn as_slice(&self) -> &[u8] {
        let all_bytes: &[u8] = cast_slice(&self.blocks);
        &all_bytes[..self.len]
    }

    /// Returns a mutable byte slice view.
    #[inline]
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        let all_bytes: &mut [u8] = cast_slice_mut(&mut self.blocks);
        &mut all_bytes[..self.len]
    }

    fn ensure_block_len(&mut self, byte_len: usize) {
        let needed_blocks = byte_len.div_ceil(CACHE_LINE_BYTES);
        if needed_blocks > self.blocks.len() {
            self.blocks.resize(needed_blocks, CacheLineBlock::default());
        }
    }
}

impl Deref for AlignedBuffer {
    type Target = [u8];

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl DerefMut for AlignedBuffer {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.as_mut_slice()
    }
}

/// Strongly-typed vector whose backing allocation is always 64-byte aligned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlignedVec<T: Pod> {
    blocks: Vec<CacheLineBlock>,
    len: usize,
    _marker: PhantomData<T>,
}

impl<T: Pod> Default for AlignedVec<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Pod> AlignedVec<T> {
    const _VALID_LAYOUT: () = {
        assert!(size_of::<T>() > 0, "AlignedVec does not support ZSTs");
        #[allow(clippy::manual_is_multiple_of)]
        let divides_cache_line = CACHE_LINE_BYTES % size_of::<T>() == 0;
        assert!(
            divides_cache_line,
            "element size must divide 64-byte cache line cleanly"
        );
        assert!(
            align_of::<T>() <= CACHE_LINE_BYTES,
            "element alignment must not exceed 64 bytes"
        );
    };

    /// Creates an empty 64-byte aligned typed vector.
    pub fn new() -> Self {
        let () = Self::_VALID_LAYOUT;
        Self {
            blocks: Vec::new(),
            len: 0,
            _marker: PhantomData,
        }
    }

    /// Creates an empty vector pre-sized for `elem_cap` elements.
    pub fn with_capacity(elem_cap: usize) -> Self {
        let () = Self::_VALID_LAYOUT;
        let byte_cap = elem_cap.saturating_mul(size_of::<T>());
        let block_cap = byte_cap.div_ceil(CACHE_LINE_BYTES);
        Self {
            blocks: Vec::with_capacity(block_cap),
            len: 0,
            _marker: PhantomData,
        }
    }

    /// Returns the number of stored elements.
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` when zero elements are stored.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns total allocated capacity in elements of `T`.
    #[inline]
    pub fn capacity(&self) -> usize {
        (self.blocks.capacity() * CACHE_LINE_BYTES) / size_of::<T>()
    }

    /// Returns the heap byte footprint of the backing allocation.
    #[inline]
    pub fn byte_len(&self) -> usize {
        self.blocks.capacity() * CACHE_LINE_BYTES
    }

    /// Clears the element count without releasing backing memory.
    #[inline]
    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// Removes the first `count` elements by shifting remaining elements forward.
    pub fn drain_prefix(&mut self, count: usize) {
        if count == 0 {
            return;
        }
        if count >= self.len {
            self.len = 0;
            return;
        }
        let cur_len = self.len;
        let remaining = cur_len - count;
        self.as_mut_slice().copy_within(count..cur_len, 0);
        self.len = remaining;
    }

    /// Appends a single element.
    #[inline]
    pub fn push(&mut self, value: T) {
        let idx = self.len;
        let needed_bytes = (idx + 1) * size_of::<T>();
        let needed_blocks = needed_bytes.div_ceil(CACHE_LINE_BYTES);
        if needed_blocks > self.blocks.len() {
            self.blocks.resize(needed_blocks, CacheLineBlock::default());
        }
        self.len = idx + 1;
        self.as_mut_slice()[idx] = value;
    }

    /// Appends a slice of elements.
    pub fn extend_from_slice(&mut self, slice: &[T]) {
        if slice.is_empty() {
            return;
        }
        let start = self.len;
        let new_len = start + slice.len();
        let needed_bytes = new_len * size_of::<T>();
        let needed_blocks = needed_bytes.div_ceil(CACHE_LINE_BYTES);
        if needed_blocks > self.blocks.len() {
            self.blocks.resize(needed_blocks, CacheLineBlock::default());
        }
        self.len = new_len;
        self.as_mut_slice()[start..new_len].copy_from_slice(slice);
    }

    /// Returns an immutable slice of stored elements.
    #[inline]
    pub fn as_slice(&self) -> &[T] {
        let all: &[T] = cast_slice(&self.blocks);
        &all[..self.len]
    }

    /// Returns a mutable slice of stored elements.
    #[inline]
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        let all: &mut [T] = cast_slice_mut(&mut self.blocks);
        &mut all[..self.len]
    }
}

impl<T: Pod> Deref for AlignedVec<T> {
    type Target = [T];

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<T: Pod> DerefMut for AlignedVec<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.as_mut_slice()
    }
}

/// Timestamp storage column supporting compact 32-bit relative offsets or full 64-bit timestamps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TimestampColumn {
    /// Base `u64` timestamp plus 32-bit nanosecond deltas (valid up to ~4.29s per chunk).
    RelU32 {
        base_ts_ns: TimestampNs,
        rel_ns: AlignedVec<u32>,
    },
    /// Full 64-bit monotonic timestamps for low-rate or wide-span chunks.
    AbsU64(AlignedVec<u64>),
}

impl TimestampColumn {
    /// Constructs a dual-encoded column from a slice of timestamps relative to `base`.
    /// Selects `RelU32` if all timestamps are >= base and within ~4.29s (`MAX_REL_U32_NS`);
    /// otherwise falls back to `AbsU64`.
    pub fn from_timestamps(base: TimestampNs, ts: &[TimestampNs]) -> Self {
        if ts.is_empty() {
            return Self::RelU32 {
                base_ts_ns: base,
                rel_ns: AlignedVec::new(),
            };
        }

        let can_rel = ts.iter().all(|&t| t >= base && (t - base) <= MAX_REL_U32_NS);
        if can_rel {
            let mut rel_ns = AlignedVec::with_capacity(ts.len());
            for &t in ts {
                rel_ns.push((t - base) as u32);
            }
            Self::RelU32 {
                base_ts_ns: base,
                rel_ns,
            }
        } else {
            let mut abs = AlignedVec::with_capacity(ts.len());
            abs.extend_from_slice(ts);
            Self::AbsU64(abs)
        }
    }

    /// Constructs a dual-encoded column automatically using the first timestamp as base.
    pub fn from_slice(ts: &[TimestampNs]) -> Self {
        let base = ts.first().copied().unwrap_or(0);
        Self::from_timestamps(base, ts)
    }

    /// Appends a timestamp, automatically upgrading from `RelU32` to `AbsU64` if span exceeds ~4.29s.
    pub fn push(&mut self, ts: TimestampNs) {
        match self {
            Self::RelU32 { base_ts_ns, rel_ns } => {
                if ts >= *base_ts_ns && (ts - *base_ts_ns) <= MAX_REL_U32_NS {
                    rel_ns.push((ts - *base_ts_ns) as u32);
                } else {
                    let mut abs = AlignedVec::with_capacity(rel_ns.len() + 1);
                    for &delta in rel_ns.iter() {
                        abs.push(*base_ts_ns + u64::from(delta));
                    }
                    abs.push(ts);
                    *self = Self::AbsU64(abs);
                }
            }
            Self::AbsU64(abs) => {
                abs.push(ts);
            }
        }
    }

    /// Clears all timestamps without releasing backing capacity.
    pub fn clear(&mut self) {
        match self {
            Self::RelU32 { rel_ns, .. } => rel_ns.clear(),
            Self::AbsU64(abs) => abs.clear(),
        }
    }

    /// Returns the number of timestamps in this column.
    pub fn len(&self) -> usize {
        match self {
            Self::RelU32 { rel_ns, .. } => rel_ns.len(),
            Self::AbsU64(ts) => ts.len(),
        }
    }

    /// Returns `true` if there are no timestamps.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Reads the absolute nanosecond timestamp at `idx`.
    #[inline]
    pub fn get_ns(&self, idx: usize) -> Option<TimestampNs> {
        match self {
            Self::RelU32 { base_ts_ns, rel_ns } => {
                rel_ns.get(idx).map(|&delta| *base_ts_ns + u64::from(delta))
            }
            Self::AbsU64(ts) => ts.get(idx).copied(),
        }
    }

    /// Returns the first timestamp in the column.
    pub fn first_ns(&self) -> Option<TimestampNs> {
        self.get_ns(0)
    }

    /// Returns the last timestamp in the column.
    pub fn last_ns(&self) -> Option<TimestampNs> {
        let len = self.len();
        if len == 0 {
            None
        } else {
            self.get_ns(len - 1)
        }
    }

    /// Returns approximate heap memory usage in bytes.
    pub fn byte_footprint(&self) -> usize {
        match self {
            Self::RelU32 { rel_ns, .. } => rel_ns.byte_len(),
            Self::AbsU64(ts) => ts.byte_len(),
        }
    }

    /// Validates that all timestamps in this column are monotonically non-decreasing.
    ///
    /// # Errors
    /// Returns [`CoreError::Storage`] if any timestamp is strictly less than its predecessor.
    pub fn validate_monotonic(&self) -> Result<(), CoreError> {
        match self {
            Self::RelU32 { base_ts_ns, rel_ns } => {
                let slice = rel_ns.as_slice();
                for i in 1..slice.len() {
                    if slice[i] < slice[i - 1] {
                        let prev_ts = *base_ts_ns + u64::from(slice[i - 1]);
                        let curr_ts = *base_ts_ns + u64::from(slice[i]);
                        return Err(CoreError::Storage(format!(
                            "ScalarDataChunk non-monotonic timestamps: at index {i}: {curr_ts} < {prev_ts}"
                        )));
                    }
                }
                Ok(())
            }
            Self::AbsU64(abs) => {
                let slice = abs.as_slice();
                for i in 1..slice.len() {
                    if slice[i] < slice[i - 1] {
                        let prev_ts = slice[i - 1];
                        let curr_ts = slice[i];
                        return Err(CoreError::Storage(format!(
                            "ScalarDataChunk non-monotonic timestamps: at index {i}: {curr_ts} < {prev_ts}"
                        )));
                    }
                }
                Ok(())
            }
        }
    }
}

/// 1-bit packed boolean logic chunk with pre-computed intra-chunk transition edge indices.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BitVecChunk {
    pub words: AlignedVec<u64>,
    pub len: u32,
    pub edge_indices: Vec<u16>,
}

impl BitVecChunk {
    /// Creates an empty bit-packed digital logic chunk.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a bit-packed chunk pre-allocated for `bit_cap` samples.
    pub fn with_capacity(bit_cap: usize) -> Self {
        let word_cap = bit_cap.div_ceil(64);
        Self {
            words: AlignedVec::with_capacity(word_cap),
            len: 0,
            edge_indices: Vec::new(),
        }
    }

    /// Returns the number of stored boolean samples.
    #[inline]
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// Returns `true` when zero samples are stored.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Reads boolean sample `idx`.
    #[inline]
    pub fn get(&self, idx: usize) -> Option<bool> {
        if idx >= self.len as usize {
            return None;
        }
        let word = self.words.get(idx / 64)?;
        Some(((word >> (idx % 64)) & 1) != 0)
    }

    /// Returns approximate heap memory usage in bytes.
    pub fn byte_footprint(&self) -> usize {
        self.words.byte_len() + self.edge_indices.capacity() * size_of::<u16>()
    }

    /// Initial logic level (`0` or `1`) at the left edge of the chunk.
    #[inline]
    pub fn digital_init_level(&self) -> u8 {
        self.get(0).map_or(0, |b| if b { 1 } else { 0 })
    }

    /// Appends a single boolean sample.
    pub fn push(&mut self, val: bool) {
        let idx = self.len as usize;
        let word_idx = idx / 64;
        let bit_offset = idx % 64;
        if bit_offset == 0 {
            let new_word = if val { 1u64 } else { 0u64 };
            self.words.push(new_word);
        } else if val {
            self.words[word_idx] |= 1u64 << bit_offset;
        } else {
            self.words[word_idx] &= !(1u64 << bit_offset);
        }
        self.len += 1;
    }

    /// Appends a slice of boolean samples.
    pub fn extend_from_slice(&mut self, bits: &[bool]) {
        if bits.is_empty() {
            return;
        }
        let mut idx = 0;
        // 1. Fill remainder of partially filled word
        while idx < bits.len() && (self.len % 64) != 0 {
            self.push(bits[idx]);
            idx += 1;
        }
        // 2. Chunk-pack 64-bool blocks into u64 words
        while idx + 64 <= bits.len() {
            let chunk = &bits[idx..idx + 64];
            let mut word = 0u64;
            for (i, &b) in chunk.iter().enumerate() {
                if b {
                    word |= 1u64 << i;
                }
            }
            self.words.push(word);
            self.len += 64;
            idx += 64;
        }
        // 3. Push remaining tail
        while idx < bits.len() {
            self.push(bits[idx]);
            idx += 1;
        }
    }

    /// Pre-computes intra-chunk transition edge indices using bitwise XOR and trailing zeros.
    pub fn compute_edge_indices(&mut self) {
        self.compute_edge_indices_with_init(None);
    }

    /// Pre-computes transition edge indices with an optional initial level for inter-chunk continuity.
    pub fn compute_edge_indices_with_init(&mut self, prev_level: Option<bool>) {
        self.edge_indices.clear();
        if self.len == 0 {
            return;
        }

        let num_words = (self.len as usize).div_ceil(64);
        let mut prev_bit = match prev_level {
            Some(lvl) => u64::from(lvl),
            None => self.words[0] & 1,
        };

        for word_idx in 0..num_words {
            let w = self.words[word_idx];
            let shifted = (w << 1) | prev_bit;
            let mut diff = w ^ shifted;

            if word_idx == num_words - 1 {
                let valid_bits = (self.len - 1) % 64 + 1;
                let valid_mask = if valid_bits == 64 {
                    u64::MAX
                } else {
                    (1u64 << valid_bits) - 1
                };
                diff &= valid_mask;
            }

            let base_idx = word_idx * 64;
            while diff != 0 {
                let bit_idx = diff.trailing_zeros() as usize;
                if let Ok(edge) = u16::try_from(base_idx + bit_idx) {
                    self.edge_indices.push(edge);
                }
                diff &= diff - 1; // BLSR
            }

            prev_bit = (w >> 63) & 1;
        }
    }

    /// Clears samples and edge indices without releasing backing word capacity.
    pub fn clear(&mut self) {
        self.words.clear();
        self.len = 0;
        self.edge_indices.clear();
    }
}

/// Native-width value column for scalar and frame series chunks.
#[derive(Clone, Debug, PartialEq)]
pub enum NativeScalarColumn {
    Bool(BitVecChunk),
    U8(AlignedVec<u8>),
    U16(AlignedVec<u16>),
    U32(AlignedVec<u32>),
    U64(AlignedVec<u64>),
    I8(AlignedVec<i8>),
    I16(AlignedVec<i16>),
    I32(AlignedVec<i32>),
    I64(AlignedVec<i64>),
    F32(AlignedVec<f32>),
    F64(AlignedVec<f64>),
}

impl NativeScalarColumn {
    /// Allocates a native column matching `scalar_type` with capacity for `cap` elements.
    pub fn with_capacity(scalar_type: ScalarType, cap: usize) -> Self {
        match scalar_type {
            ScalarType::Bool => Self::Bool(BitVecChunk::with_capacity(cap)),
            ScalarType::U8 => Self::U8(AlignedVec::with_capacity(cap)),
            ScalarType::U16 => Self::U16(AlignedVec::with_capacity(cap)),
            ScalarType::U32 => Self::U32(AlignedVec::with_capacity(cap)),
            ScalarType::U64 => Self::U64(AlignedVec::with_capacity(cap)),
            ScalarType::I8 => Self::I8(AlignedVec::with_capacity(cap)),
            ScalarType::I16 => Self::I16(AlignedVec::with_capacity(cap)),
            ScalarType::I32 => Self::I32(AlignedVec::with_capacity(cap)),
            ScalarType::I64 => Self::I64(AlignedVec::with_capacity(cap)),
            ScalarType::F32 => Self::F32(AlignedVec::with_capacity(cap)),
            ScalarType::F64 => Self::F64(AlignedVec::with_capacity(cap)),
        }
    }

    /// Returns the [`ScalarType`] stored in this column.
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

    /// Returns the number of stored elements.
    pub fn len(&self) -> usize {
        match self {
            Self::Bool(c) => c.len(),
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
        }
    }

    /// Returns `true` if the column contains zero elements.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns approximate heap memory usage in bytes.
    pub fn byte_footprint(&self) -> usize {
        match self {
            Self::Bool(c) => c.byte_footprint(),
            Self::U8(v) => v.byte_len(),
            Self::U16(v) => v.byte_len(),
            Self::U32(v) => v.byte_len(),
            Self::U64(v) => v.byte_len(),
            Self::I8(v) => v.byte_len(),
            Self::I16(v) => v.byte_len(),
            Self::I32(v) => v.byte_len(),
            Self::I64(v) => v.byte_len(),
            Self::F32(v) => v.byte_len(),
            Self::F64(v) => v.byte_len(),
        }
    }

    /// Appends samples from a borrowed [`ScalarSlice`] into this column.
    ///
    /// # Errors
    /// Returns [`CoreError::Storage`] if the slice type does not match this column's [`ScalarType`].
    pub fn append_slice(&mut self, slice: ScalarSlice) -> Result<usize, CoreError> {
        let count = slice.len();
        if count == 0 {
            return Ok(0);
        }

        match (self, slice) {
            (Self::Bool(col), ScalarSlice::Bool(s)) => {
                col.extend_from_slice(s);
            }
            (Self::U8(col), ScalarSlice::U8(s)) => {
                col.extend_from_slice(s);
            }
            (Self::U16(col), ScalarSlice::U16(s)) => {
                col.extend_from_slice(s);
            }
            (Self::U32(col), ScalarSlice::U32(s)) => {
                col.extend_from_slice(s);
            }
            (Self::U64(col), ScalarSlice::U64(s)) => {
                col.extend_from_slice(s);
            }
            (Self::I8(col), ScalarSlice::I8(s)) => {
                col.extend_from_slice(s);
            }
            (Self::I16(col), ScalarSlice::I16(s)) => {
                col.extend_from_slice(s);
            }
            (Self::I32(col), ScalarSlice::I32(s)) => {
                col.extend_from_slice(s);
            }
            (Self::I64(col), ScalarSlice::I64(s)) => {
                col.extend_from_slice(s);
            }
            (Self::F32(col), ScalarSlice::F32(s)) => {
                col.extend_from_slice(s);
            }
            (Self::F64(col), ScalarSlice::F64(s)) => {
                col.extend_from_slice(s);
            }
            (col, slice) => {
                return Err(CoreError::Storage(format!(
                    "Scalar column type mismatch: column is {:?}, but slice is {:?}",
                    col.scalar_type(),
                    slice.scalar_type()
                )));
            }
        }

        Ok(count)
    }

    /// Reads sample `idx` converted to `f64`, or `None` if out of bounds.
    #[inline]
    pub fn get_as_f64(&self, idx: usize) -> Option<f64> {
        match self {
            Self::Bool(col) => col.get(idx).map(|v| if v { 1.0 } else { 0.0 }),
            Self::U8(col) => col.get(idx).copied().map(f64::from),
            Self::U16(col) => col.get(idx).copied().map(f64::from),
            Self::U32(col) => col.get(idx).copied().map(f64::from),
            Self::U64(col) => col.get(idx).copied().map(|v| v as f64),
            Self::I8(col) => col.get(idx).copied().map(f64::from),
            Self::I16(col) => col.get(idx).copied().map(f64::from),
            Self::I32(col) => col.get(idx).copied().map(f64::from),
            Self::I64(col) => col.get(idx).copied().map(|v| v as f64),
            Self::F32(col) => col.get(idx).copied().map(f64::from),
            Self::F64(col) => col.get(idx).copied(),
        }
    }

    /// Reads sample `idx` preserving its native representation as a [`ScalarValue`], or `None` if out of bounds.
    #[inline]
    pub fn get_scalar_value(&self, idx: usize) -> Option<ScalarValue> {
        match self {
            Self::Bool(col) => col.get(idx).map(ScalarValue::Bool),
            Self::U8(col) => col.get(idx).copied().map(ScalarValue::U8),
            Self::U16(col) => col.get(idx).copied().map(ScalarValue::U16),
            Self::U32(col) => col.get(idx).copied().map(ScalarValue::U32),
            Self::U64(col) => col.get(idx).copied().map(ScalarValue::U64),
            Self::I8(col) => col.get(idx).copied().map(ScalarValue::I8),
            Self::I16(col) => col.get(idx).copied().map(ScalarValue::I16),
            Self::I32(col) => col.get(idx).copied().map(ScalarValue::I32),
            Self::I64(col) => col.get(idx).copied().map(ScalarValue::I64),
            Self::F32(col) => col.get(idx).copied().map(ScalarValue::F32),
            Self::F64(col) => col.get(idx).copied().map(ScalarValue::F64),
        }
    }

    /// Clears all elements in the column without releasing backing capacity.
    pub fn clear(&mut self) {
        match self {
            Self::Bool(col) => col.clear(),
            Self::U8(col) => col.clear(),
            Self::U16(col) => col.clear(),
            Self::U32(col) => col.clear(),
            Self::U64(col) => col.clear(),
            Self::I8(col) => col.clear(),
            Self::I16(col) => col.clear(),
            Self::I32(col) => col.clear(),
            Self::I64(col) => col.clear(),
            Self::F32(col) => col.clear(),
            Self::F64(col) => col.clear(),
        }
    }
}

/// Pre-computed intra-chunk M4 envelope levels (L1 and L2).
#[derive(Clone, Debug, PartialEq)]
pub struct ChunkM4Levels {
    /// Level 1 buckets: step 64 raw samples (1,024 buckets for CHUNK_LEN = 65,536).
    pub l1: AlignedVec<M4Bucket>,
    /// Level 2 buckets: step 4,096 raw samples (16 buckets for CHUNK_LEN = 65,536).
    pub l2: AlignedVec<M4Bucket>,
}

impl Default for ChunkM4Levels {
    fn default() -> Self {
        Self::empty()
    }
}

impl ChunkM4Levels {
    /// Creates an empty intra-chunk M4 container with pre-allocated capacities.
    pub fn empty() -> Self {
        Self {
            l1: AlignedVec::with_capacity(CHUNK_LEN / M4_BRANCH_FACTOR),
            l2: AlignedVec::with_capacity(CHUNK_LEN / (M4_BRANCH_FACTOR * M4_BRANCH_FACTOR)),
        }
    }

    /// Creates an intra-chunk M4 container with custom capacities.
    pub fn with_capacities(l1_cap: usize, l2_cap: usize) -> Self {
        Self {
            l1: AlignedVec::with_capacity(l1_cap),
            l2: AlignedVec::with_capacity(l2_cap),
        }
    }

    /// Builds intra-chunk L1 and L2 levels from raw sample columns.
    pub fn build_from_scalar(
        timestamps: &TimestampColumn,
        values: &NativeScalarColumn,
        scale: &LinearScale,
    ) -> Result<Self, CoreError> {
        let len = timestamps.len();
        if len != values.len() {
            return Err(CoreError::Storage(format!(
                "ChunkM4Levels length mismatch: timestamps length ({len}) != values length ({})",
                values.len()
            )));
        }

        let l1_cap = len.div_ceil(M4_BRANCH_FACTOR);
        let l2_cap = len.div_ceil(M4_BRANCH_FACTOR * M4_BRANCH_FACTOR);
        let mut levels = Self::with_capacities(l1_cap, l2_cap);

        if len == 0 {
            return Ok(levels);
        }

        // 1. Build L1 buckets (64 raw samples each)
        let num_full_l1 = len / M4_BRANCH_FACTOR;
        for k in 0..num_full_l1 {
            let start = k * M4_BRANCH_FACTOR;
            let end = start + M4_BRANCH_FACTOR;
            let t_first = timestamps.get_ns(start).unwrap_or(0);
            let t_last = timestamps.get_ns(end - 1).unwrap_or(0);
            let b = reduce_column_slice(values, start, end, t_first, t_last, scale);
            levels.l1.push(b);
        }

        // Tail L1 bucket if length is not a multiple of 64
        let tail_start = num_full_l1 * M4_BRANCH_FACTOR;
        if tail_start < len {
            let t_first = timestamps.get_ns(tail_start).unwrap_or(0);
            let t_last = timestamps.get_ns(len - 1).unwrap_or(0);
            let b = reduce_column_slice(values, tail_start, len, t_first, t_last, scale);
            levels.l1.push(b);
        }

        // 2. Build L2 buckets (64 L1 buckets each = 4,096 raw samples)
        let l1_slice = levels.l1.as_slice();
        let num_full_l2 = l1_slice.len() / M4_BRANCH_FACTOR;
        for j in 0..num_full_l2 {
            let start = j * M4_BRANCH_FACTOR;
            let end = start + M4_BRANCH_FACTOR;
            let child_array: &[M4Bucket; 64] = l1_slice[start..end]
                .try_into()
                .expect("exact 64-element slice");
            levels.l2.push(reduce_64_buckets(child_array));
        }

        // Tail L2 bucket if L1 count is not a multiple of 64
        let tail_l2_start = num_full_l2 * M4_BRANCH_FACTOR;
        if tail_l2_start < l1_slice.len() {
            if let Some(b) = reduce_bucket_slice(&l1_slice[tail_l2_start..]) {
                levels.l2.push(b);
            }
        }

        Ok(levels)
    }

    /// Returns the single summary bucket aggregating the entire chunk.
    pub fn chunk_summary(&self) -> Option<M4Bucket> {
        if !self.l2.is_empty() {
            reduce_bucket_slice(self.l2.as_slice())
        } else if !self.l1.is_empty() {
            reduce_bucket_slice(self.l1.as_slice())
        } else {
            None
        }
    }

    /// Returns `true` if no M4 buckets have been computed.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.l1.is_empty()
    }

    /// Returns the number of Level-1 buckets.
    #[inline]
    pub fn len_l1(&self) -> usize {
        self.l1.len()
    }

    /// Returns the number of Level-2 buckets.
    #[inline]
    pub fn len_l2(&self) -> usize {
        self.l2.len()
    }

    /// Returns total resident memory bytes occupied by intra-chunk M4 levels.
    #[inline]
    pub fn byte_footprint(&self) -> usize {
        self.l1.len() * std::mem::size_of::<M4Bucket>()
            + self.l2.len() * std::mem::size_of::<M4Bucket>()
    }

    /// Clears all intra-chunk buckets while retaining buffer capacity.
    pub fn clear(&mut self) {
        self.l1.clear();
        self.l2.clear();
    }
}

/// Universal 64-to-1 reduction across child [`M4Bucket`] structures.
#[inline]
pub fn reduce_64_buckets(buckets: &[M4Bucket; 64]) -> M4Bucket {
    let mut v_min = buckets[0].v_min;
    let mut v_max = buckets[0].v_max;

    for b in &buckets[1..] {
        v_min = v_min.min(b.v_min);
        v_max = v_max.max(b.v_max);
    }

    M4Bucket {
        t_first_ns: buckets[0].t_first_ns,
        t_last_ns: buckets[63].t_last_ns,
        v_first: buckets[0].v_first,
        v_min,
        v_max,
        v_last: buckets[63].v_last,
    }
}

/// Arbitrary-slice reduction across child [`M4Bucket`] structures.
pub fn reduce_bucket_slice(buckets: &[M4Bucket]) -> Option<M4Bucket> {
    if buckets.is_empty() {
        return None;
    }
    let mut v_min = buckets[0].v_min;
    let mut v_max = buckets[0].v_max;

    for b in &buckets[1..] {
        v_min = v_min.min(b.v_min);
        v_max = v_max.max(b.v_max);
    }

    Some(M4Bucket {
        t_first_ns: buckets[0].t_first_ns,
        t_last_ns: buckets[buckets.len() - 1].t_last_ns,
        v_first: buckets[0].v_first,
        v_min,
        v_max,
        v_last: buckets[buckets.len() - 1].v_last,
    })
}

macro_rules! impl_int_m4_reduce_types {
    ($fn_name:ident, $t:ty) => {
        #[inline]
        pub fn $fn_name(
            slice: &[$t],
            t_first_ns: u64,
            t_last_ns: u64,
            scale: &LinearScale,
        ) -> M4Bucket {
            debug_assert!(!slice.is_empty());
            let first = slice[0];
            let last = slice[slice.len() - 1];
            let mut min_val = first;
            let mut max_val = first;

            for &x in &slice[1..] {
                min_val = min_val.min(x);
                max_val = max_val.max(x);
            }

            let v_first = scale.apply_f32(first as f64);
            let v_last = scale.apply_f32(last as f64);
            let (v_min, v_max) = if scale.scale >= 0.0 {
                (
                    scale.apply_f32(min_val as f64),
                    scale.apply_f32(max_val as f64),
                )
            } else {
                (
                    scale.apply_f32(max_val as f64),
                    scale.apply_f32(min_val as f64),
                )
            };

            M4Bucket {
                t_first_ns,
                t_last_ns,
                v_first,
                v_min,
                v_max,
                v_last,
            }
        }
    };
}

impl_int_m4_reduce_types!(reduce_raw_u8_slice, u8);
impl_int_m4_reduce_types!(reduce_raw_u16_slice, u16);
impl_int_m4_reduce_types!(reduce_raw_u32_slice, u32);
impl_int_m4_reduce_types!(reduce_raw_u64_slice, u64);
impl_int_m4_reduce_types!(reduce_raw_i8_slice, i8);
impl_int_m4_reduce_types!(reduce_raw_i16_slice, i16);
impl_int_m4_reduce_types!(reduce_raw_i32_slice, i32);
impl_int_m4_reduce_types!(reduce_raw_i64_slice, i64);

/// Single-precision IEEE-754 reduction with safe branchless NaN handling.
#[inline]
pub fn reduce_raw_f32_slice(
    slice: &[f32],
    t_first_ns: u64,
    t_last_ns: u64,
    scale: &LinearScale,
) -> M4Bucket {
    debug_assert!(!slice.is_empty());
    let first = slice[0];
    let last = slice[slice.len() - 1];
    let mut min_val = first;
    let mut max_val = first;

    for &x in &slice[1..] {
        min_val = min_val.min(x);
        max_val = max_val.max(x);
    }

    let v_first = scale.apply_f32(first as f64);
    let v_last = scale.apply_f32(last as f64);
    let (v_min, v_max) = if scale.scale >= 0.0 {
        (
            scale.apply_f32(min_val as f64),
            scale.apply_f32(max_val as f64),
        )
    } else {
        (
            scale.apply_f32(max_val as f64),
            scale.apply_f32(min_val as f64),
        )
    };

    M4Bucket {
        t_first_ns,
        t_last_ns,
        v_first,
        v_min,
        v_max,
        v_last,
    }
}

/// Double-precision IEEE-754 reduction with safe branchless NaN handling.
#[inline]
pub fn reduce_raw_f64_slice(
    slice: &[f64],
    t_first_ns: u64,
    t_last_ns: u64,
    scale: &LinearScale,
) -> M4Bucket {
    debug_assert!(!slice.is_empty());
    let first = slice[0];
    let last = slice[slice.len() - 1];
    let mut min_val = first;
    let mut max_val = first;

    for &x in &slice[1..] {
        min_val = min_val.min(x);
        max_val = max_val.max(x);
    }

    let v_first = scale.apply_f32(first);
    let v_last = scale.apply_f32(last);
    let (v_min, v_max) = if scale.scale >= 0.0 {
        (scale.apply_f32(min_val), scale.apply_f32(max_val))
    } else {
        (scale.apply_f32(max_val), scale.apply_f32(min_val))
    };

    M4Bucket {
        t_first_ns,
        t_last_ns,
        v_first,
        v_min,
        v_max,
        v_last,
    }
}

/// Reduces a slice of [`NativeScalarColumn::Bool`] (1-bit packed `BitVecChunk`).
#[inline]
pub fn reduce_raw_bool_slice(
    bitvec: &BitVecChunk,
    start: usize,
    end: usize,
    t_first_ns: u64,
    t_last_ns: u64,
    scale: &LinearScale,
) -> M4Bucket {
    let first_bool = bitvec.get(start).unwrap_or(false);
    let last_bool = bitvec.get(end - 1).unwrap_or(false);

    let mut has_zero = false;
    let mut has_one = false;

    for i in start..end {
        if bitvec.get(i).unwrap_or(false) {
            has_one = true;
        } else {
            has_zero = true;
        }
        if has_zero && has_one {
            break;
        }
    }

    let raw_min = if has_zero { 0.0 } else { 1.0 };
    let raw_max = if has_one { 1.0 } else { 0.0 };

    let v_first = scale.apply_f32(if first_bool { 1.0 } else { 0.0 });
    let v_last = scale.apply_f32(if last_bool { 1.0 } else { 0.0 });
    let (v_min, v_max) = if scale.scale >= 0.0 {
        (scale.apply_f32(raw_min), scale.apply_f32(raw_max))
    } else {
        (scale.apply_f32(raw_max), scale.apply_f32(raw_min))
    };

    M4Bucket {
        t_first_ns,
        t_last_ns,
        v_first,
        v_min,
        v_max,
        v_last,
    }
}

/// Generic dispatcher reducing a subslice `[start..end]` of [`NativeScalarColumn`].
pub fn reduce_column_slice(
    values: &NativeScalarColumn,
    start: usize,
    end: usize,
    t_first_ns: u64,
    t_last_ns: u64,
    scale: &LinearScale,
) -> M4Bucket {
    match values {
        NativeScalarColumn::U8(c) => {
            reduce_raw_u8_slice(&c.as_slice()[start..end], t_first_ns, t_last_ns, scale)
        }
        NativeScalarColumn::U16(c) => {
            reduce_raw_u16_slice(&c.as_slice()[start..end], t_first_ns, t_last_ns, scale)
        }
        NativeScalarColumn::U32(c) => {
            reduce_raw_u32_slice(&c.as_slice()[start..end], t_first_ns, t_last_ns, scale)
        }
        NativeScalarColumn::U64(c) => {
            reduce_raw_u64_slice(&c.as_slice()[start..end], t_first_ns, t_last_ns, scale)
        }
        NativeScalarColumn::I8(c) => {
            reduce_raw_i8_slice(&c.as_slice()[start..end], t_first_ns, t_last_ns, scale)
        }
        NativeScalarColumn::I16(c) => {
            reduce_raw_i16_slice(&c.as_slice()[start..end], t_first_ns, t_last_ns, scale)
        }
        NativeScalarColumn::I32(c) => {
            reduce_raw_i32_slice(&c.as_slice()[start..end], t_first_ns, t_last_ns, scale)
        }
        NativeScalarColumn::I64(c) => {
            reduce_raw_i64_slice(&c.as_slice()[start..end], t_first_ns, t_last_ns, scale)
        }
        NativeScalarColumn::F32(c) => {
            reduce_raw_f32_slice(&c.as_slice()[start..end], t_first_ns, t_last_ns, scale)
        }
        NativeScalarColumn::F64(c) => {
            reduce_raw_f64_slice(&c.as_slice()[start..end], t_first_ns, t_last_ns, scale)
        }
        NativeScalarColumn::Bool(bv) => {
            reduce_raw_bool_slice(bv, start, end, t_first_ns, t_last_ns, scale)
        }
    }
}

/// Sealed chunk payload for a `ScalarSeries` channel.
#[derive(Clone, Debug, PartialEq)]
pub struct ScalarDataChunk {
    pub timestamps: TimestampColumn,
    pub values: NativeScalarColumn,
    pub calibration: LinearScale,
    pub m4: ChunkM4Levels,
}

impl ScalarDataChunk {
    /// Creates and validates a sealed scalar data chunk, automatically computing its M4 envelope levels.
    ///
    /// # Errors
    /// - Returns [`CoreError::Storage`] if `timestamps.len() != values.len()`.
    /// - Returns [`CoreError::Storage`] if `timestamps.len() > CHUNK_LEN`.
    /// - Returns [`CoreError::Storage`] if timestamps are non-monotonic.
    pub fn new(
        timestamps: TimestampColumn,
        values: NativeScalarColumn,
        calibration: LinearScale,
    ) -> Result<Self, CoreError> {
        let m4 = ChunkM4Levels::build_from_scalar(&timestamps, &values, &calibration)?;
        Self::new_with_m4(timestamps, values, calibration, m4)
    }

    /// Creates and validates a sealed scalar data chunk with pre-computed M4 envelope levels.
    pub fn new_with_m4(
        timestamps: TimestampColumn,
        values: NativeScalarColumn,
        calibration: LinearScale,
        m4: ChunkM4Levels,
    ) -> Result<Self, CoreError> {
        if timestamps.len() != values.len() {
            return Err(CoreError::Storage(format!(
                "ScalarDataChunk length mismatch: timestamps length ({}) != values length ({})",
                timestamps.len(),
                values.len()
            )));
        }
        if timestamps.len() > CHUNK_LEN {
            return Err(CoreError::Storage(format!(
                "ScalarDataChunk length ({}) exceeds CHUNK_LEN ({})",
                timestamps.len(),
                CHUNK_LEN
            )));
        }
        timestamps.validate_monotonic()?;
        Ok(Self {
            timestamps,
            values,
            calibration,
            m4,
        })
    }

    /// Returns the number of samples in this chunk.
    #[inline]
    pub fn len(&self) -> usize {
        self.timestamps.len()
    }

    /// Returns `true` if this chunk contains zero samples.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the time range `(t_start_ns, t_end_ns)` covered by this chunk, or `None` if empty.
    #[inline]
    pub fn time_range_ns(&self) -> Option<(TimestampNs, TimestampNs)> {
        Some((self.timestamps.first_ns()?, self.timestamps.last_ns()?))
    }

    /// Reads sample at `idx`, returning `(timestamp_ns, calibrated_f64_value)`.
    #[inline]
    pub fn get_sample_as_f64(&self, idx: usize) -> Option<(TimestampNs, f64)> {
        let ts = self.timestamps.get_ns(idx)?;
        let raw = self.values.get_as_f64(idx)?;
        Some((ts, self.calibration.apply(raw)))
    }

    /// Returns approximate heap memory usage of this chunk in bytes.
    #[inline]
    pub fn byte_footprint(&self) -> usize {
        self.timestamps.byte_footprint() + self.values.byte_footprint() + self.m4.byte_footprint()
    }
}

/// Sealed chunk payload for a `FrameSeries` channel (e.g., FFT spectrum frames).
#[derive(Clone, Debug, PartialEq)]
pub struct FrameDataChunk {
    pub timestamps: TimestampColumn,
    pub frame_len: u32,
    pub values: NativeScalarColumn,
}

/// Sealed chunk payload for an `EventSeries` channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventDataChunk {
    /// Numeric state-machine transition timeline rendered as MSO state ribbons.
    StateTransitions {
        timestamps_ns: AlignedVec<u64>,
        state_codes: AlignedVec<i32>,
    },
    /// Discrete text log or structured event snapshots.
    TextRecords {
        timestamps_ns: AlignedVec<u64>,
        messages: Vec<Arc<str>>,
    },
}

impl EventDataChunk {
    /// Creates an empty `StateTransitions` chunk pre-sized for `cap` events.
    pub fn new_state_transitions(cap: usize) -> Self {
        Self::StateTransitions {
            timestamps_ns: AlignedVec::with_capacity(cap),
            state_codes: AlignedVec::with_capacity(cap),
        }
    }

    /// Appends a state transition, enforcing non-decreasing timestamp ordering.
    ///
    /// # Errors
    /// Returns [`CoreError::Storage`] if `ts_ns < last_ts` or if called on a `TextRecords` chunk.
    pub fn append_transition(&mut self, ts_ns: TimestampNs, code: i32) -> Result<(), CoreError> {
        match self {
            Self::StateTransitions {
                timestamps_ns,
                state_codes,
            } => {
                if let Some(&last_ts) = timestamps_ns.last() {
                    if ts_ns < last_ts {
                        return Err(CoreError::Storage(format!(
                            "non-monotonic event timestamp: {ts_ns} < {last_ts}"
                        )));
                    }
                }
                timestamps_ns.push(ts_ns);
                state_codes.push(code);
                Ok(())
            }
            Self::TextRecords { .. } => Err(CoreError::Storage(
                "cannot append state transition to TextRecords chunk".into(),
            )),
        }
    }

    /// Looks up the active state code at timestamp `ts_ns` via binary search ($O(\log N)$).
    /// Returns `None` if `ts_ns` is before the first recorded transition in this chunk.
    pub fn get_state_at(&self, ts_ns: TimestampNs) -> Option<i32> {
        match self {
            Self::StateTransitions {
                timestamps_ns,
                state_codes,
            } => {
                let p = timestamps_ns.partition_point(|&t| t <= ts_ns);
                if p == 0 {
                    None
                } else {
                    Some(state_codes[p - 1])
                }
            }
            Self::TextRecords { .. } => None,
        }
    }

    /// Returns the transition timestamp and code at `idx` in $O(1)$.
    pub fn get_transition(&self, idx: usize) -> Option<(TimestampNs, i32)> {
        match self {
            Self::StateTransitions {
                timestamps_ns,
                state_codes,
            } => {
                let ts = timestamps_ns.get(idx).copied()?;
                let code = state_codes.get(idx).copied()?;
                Some((ts, code))
            }
            Self::TextRecords { .. } => None,
        }
    }

    /// Returns the number of events/transitions in this chunk.
    pub fn len(&self) -> usize {
        match self {
            Self::StateTransitions { timestamps_ns, .. } => timestamps_ns.len(),
            Self::TextRecords { timestamps_ns, .. } => timestamps_ns.len(),
        }
    }

    /// Returns `true` if this chunk contains zero events.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Unified sealed data chunk across all three channel modalities.
#[derive(Clone, Debug, PartialEq)]
pub enum DataChunk {
    Scalar(ScalarDataChunk),
    Frame(FrameDataChunk),
    Event(EventDataChunk),
}

impl DataChunk {
    /// Returns the number of time-series samples/events in this chunk.
    pub fn sample_count(&self) -> usize {
        match self {
            Self::Scalar(c) => c.timestamps.len(),
            Self::Frame(c) => c.timestamps.len(),
            Self::Event(EventDataChunk::StateTransitions { timestamps_ns, .. }) => {
                timestamps_ns.len()
            }
            Self::Event(EventDataChunk::TextRecords { timestamps_ns, .. }) => timestamps_ns.len(),
        }
    }

    /// Returns `(t_start_ns, t_end_ns)` covered by this chunk, or `None` if empty.
    pub fn time_range_ns(&self) -> Option<(TimestampNs, TimestampNs)> {
        match self {
            Self::Scalar(c) => Some((c.timestamps.first_ns()?, c.timestamps.last_ns()?)),
            Self::Frame(c) => Some((c.timestamps.first_ns()?, c.timestamps.last_ns()?)),
            Self::Event(EventDataChunk::StateTransitions { timestamps_ns, .. }) => {
                let first = *timestamps_ns.first()?;
                let last = *timestamps_ns.last()?;
                Some((first, last))
            }
            Self::Event(EventDataChunk::TextRecords { timestamps_ns, .. }) => {
                let first = *timestamps_ns.first()?;
                let last = *timestamps_ns.last()?;
                Some((first, last))
            }
        }
    }

    /// Returns approximate heap memory usage of this chunk in bytes.
    pub fn byte_footprint(&self) -> usize {
        match self {
            Self::Scalar(c) => c.byte_footprint(),
            Self::Frame(c) => c.timestamps.byte_footprint() + c.values.byte_footprint(),
            Self::Event(EventDataChunk::StateTransitions {
                timestamps_ns,
                state_codes,
            }) => timestamps_ns.byte_len() + state_codes.byte_len(),
            Self::Event(EventDataChunk::TextRecords {
                timestamps_ns,
                messages,
            }) => {
                let str_bytes: usize = messages.iter().map(|s| s.len() + size_of::<Arc<str>>()).sum();
                timestamps_ns.byte_len() + str_bytes
            }
        }
    }
}

/// File offset locator for an evicted chunk backed by a `.ddcap` memory-mapped file (Phase 3 hook).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MmapChunkLocator {
    pub file_id: u64,
    pub byte_offset: u64,
    pub byte_len: u32,
    pub flags: u32,
}

/// Storage handle abstraction isolating in-memory `Arc<DataChunk>` from future `.ddcap` mmap slices (Hook `H4`).
#[derive(Clone, Debug)]
pub enum ChunkStorageRef {
    InMemory(Arc<DataChunk>),
    MmapSlice(MmapChunkLocator),
}

impl ChunkStorageRef {
    /// Returns the in-memory `Arc<DataChunk>` if resident in RAM.
    pub fn as_in_memory(&self) -> Option<&Arc<DataChunk>> {
        match self {
            Self::InMemory(chunk) => Some(chunk),
            Self::MmapSlice(_) => None,
        }
    }

    /// Returns `true` when the chunk resides in RAM.
    pub fn is_in_memory(&self) -> bool {
        matches!(self, Self::InMemory(_))
    }

    /// Returns the RAM footprint accounted against the memory quota (`0` for mmap slices).
    pub fn resident_bytes(&self) -> usize {
        match self {
            Self::InMemory(chunk) => chunk.byte_footprint(),
            Self::MmapSlice(_) => 0,
        }
    }
}

/// Metadata + storage handle for a sealed chunk tracked by a series index.
#[derive(Clone, Debug)]
pub struct ChunkHandle {
    pub chunk_seq: u64,
    pub t_start_ns: TimestampNs,
    pub t_end_ns: TimestampNs,
    pub sample_count: u32,
    pub storage: ChunkStorageRef,
}

/// Single M4 envelope bucket summarizing min/max/first/last across a sample span.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable, Serialize, Deserialize)]
pub struct M4Bucket {
    pub t_first_ns: u64,
    pub t_last_ns: u64,
    pub v_first: f32,
    pub v_min: f32,
    pub v_max: f32,
    pub v_last: f32,
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_native_scalar_column_append_and_retrieve_all_types() {
        // 1. U8
        let mut col_u8 = NativeScalarColumn::with_capacity(ScalarType::U8, 8);
        assert_eq!(col_u8.scalar_type(), ScalarType::U8);
        assert_eq!(col_u8.append_slice(ScalarSlice::U8(&[])).unwrap(), 0);
        assert_eq!(
            col_u8.append_slice(ScalarSlice::U8(&[0, 127, 255])).unwrap(),
            3
        );
        assert_eq!(col_u8.len(), 3);
        assert!(!col_u8.is_empty());
        assert_eq!(col_u8.get_as_f64(0), Some(0.0));
        assert_eq!(col_u8.get_as_f64(1), Some(127.0));
        assert_eq!(col_u8.get_as_f64(2), Some(255.0));
        assert_eq!(col_u8.get_as_f64(3), None);
        assert_eq!(col_u8.get_scalar_value(1), Some(ScalarValue::U8(127)));
        assert_eq!(col_u8.get_scalar_value(3), None);
        col_u8.clear();
        assert_eq!(col_u8.len(), 0);
        assert!(col_u8.is_empty());

        // 2. U16
        let mut col_u16 = NativeScalarColumn::with_capacity(ScalarType::U16, 4);
        assert_eq!(col_u16.scalar_type(), ScalarType::U16);
        assert_eq!(
            col_u16
                .append_slice(ScalarSlice::U16(&[0, 32767, 65535]))
                .unwrap(),
            3
        );
        assert_eq!(col_u16.get_as_f64(0), Some(0.0));
        assert_eq!(col_u16.get_as_f64(2), Some(65535.0));
        assert_eq!(col_u16.get_scalar_value(2), Some(ScalarValue::U16(65535)));

        // 3. U32
        let mut col_u32 = NativeScalarColumn::with_capacity(ScalarType::U32, 4);
        assert_eq!(col_u32.scalar_type(), ScalarType::U32);
        assert_eq!(
            col_u32
                .append_slice(ScalarSlice::U32(&[0, 2_147_483_647, 4_294_967_295]))
                .unwrap(),
            3
        );
        assert_eq!(col_u32.get_as_f64(2), Some(4_294_967_295.0));
        assert_eq!(
            col_u32.get_scalar_value(1),
            Some(ScalarValue::U32(2_147_483_647))
        );

        // 4. U64
        let mut col_u64 = NativeScalarColumn::with_capacity(ScalarType::U64, 4);
        assert_eq!(col_u64.scalar_type(), ScalarType::U64);
        assert_eq!(
            col_u64
                .append_slice(ScalarSlice::U64(&[0, 1_000_000_000, u64::MAX]))
                .unwrap(),
            3
        );
        assert_eq!(col_u64.get_as_f64(1), Some(1_000_000_000.0));
        assert_eq!(col_u64.get_scalar_value(2), Some(ScalarValue::U64(u64::MAX)));

        // 5. I8
        let mut col_i8 = NativeScalarColumn::with_capacity(ScalarType::I8, 5);
        assert_eq!(col_i8.scalar_type(), ScalarType::I8);
        assert_eq!(
            col_i8
                .append_slice(ScalarSlice::I8(&[-128, -1, 0, 1, 127]))
                .unwrap(),
            5
        );
        assert_eq!(col_i8.get_as_f64(0), Some(-128.0));
        assert_eq!(col_i8.get_as_f64(1), Some(-1.0));
        assert_eq!(col_i8.get_as_f64(4), Some(127.0));
        assert_eq!(col_i8.get_scalar_value(0), Some(ScalarValue::I8(-128)));

        // 6. I16
        let mut col_i16 = NativeScalarColumn::with_capacity(ScalarType::I16, 5);
        assert_eq!(col_i16.scalar_type(), ScalarType::I16);
        assert_eq!(
            col_i16
                .append_slice(ScalarSlice::I16(&[-32768, -1, 0, 1, 32767]))
                .unwrap(),
            5
        );
        assert_eq!(col_i16.get_as_f64(0), Some(-32768.0));
        assert_eq!(col_i16.get_as_f64(4), Some(32767.0));
        assert_eq!(col_i16.get_scalar_value(4), Some(ScalarValue::I16(32767)));

        // 7. I32
        let mut col_i32 = NativeScalarColumn::with_capacity(ScalarType::I32, 3);
        assert_eq!(col_i32.scalar_type(), ScalarType::I32);
        assert_eq!(
            col_i32
                .append_slice(ScalarSlice::I32(&[i32::MIN, 0, i32::MAX]))
                .unwrap(),
            3
        );
        assert_eq!(col_i32.get_as_f64(0), Some(i32::MIN as f64));
        assert_eq!(col_i32.get_as_f64(2), Some(i32::MAX as f64));
        assert_eq!(col_i32.get_scalar_value(0), Some(ScalarValue::I32(i32::MIN)));

        // 8. I64
        let mut col_i64 = NativeScalarColumn::with_capacity(ScalarType::I64, 3);
        assert_eq!(col_i64.scalar_type(), ScalarType::I64);
        assert_eq!(
            col_i64
                .append_slice(ScalarSlice::I64(&[i64::MIN, 0, i64::MAX]))
                .unwrap(),
            3
        );
        assert_eq!(col_i64.get_as_f64(0), Some(i64::MIN as f64));
        assert_eq!(col_i64.get_as_f64(2), Some(i64::MAX as f64));
        assert_eq!(col_i64.get_scalar_value(2), Some(ScalarValue::I64(i64::MAX)));

        // 9. F32
        let mut col_f32 = NativeScalarColumn::with_capacity(ScalarType::F32, 4);
        assert_eq!(col_f32.scalar_type(), ScalarType::F32);
        assert_eq!(
            col_f32
                .append_slice(ScalarSlice::F32(&[-100.5, 0.0, 100.25, -0.0]))
                .unwrap(),
            4
        );
        assert_eq!(col_f32.get_as_f64(0), Some(-100.5));
        assert_eq!(col_f32.get_as_f64(2), Some(100.25));
        assert_eq!(col_f32.get_scalar_value(2), Some(ScalarValue::F32(100.25)));

        // 10. F64
        let mut col_f64 = NativeScalarColumn::with_capacity(ScalarType::F64, 3);
        assert_eq!(col_f64.scalar_type(), ScalarType::F64);
        assert_eq!(
            col_f64
                .append_slice(ScalarSlice::F64(&[-1e10, 0.0, std::f64::consts::PI]))
                .unwrap(),
            3
        );
        assert_eq!(col_f64.get_as_f64(0), Some(-1e10));
        assert_eq!(col_f64.get_as_f64(2), Some(std::f64::consts::PI));
        assert_eq!(
            col_f64.get_scalar_value(2),
            Some(ScalarValue::F64(std::f64::consts::PI))
        );

        // 11. Bool
        let mut col_bool = NativeScalarColumn::with_capacity(ScalarType::Bool, 5);
        assert_eq!(col_bool.scalar_type(), ScalarType::Bool);
        assert_eq!(
            col_bool
                .append_slice(ScalarSlice::Bool(&[true, false, true, true, false]))
                .unwrap(),
            5
        );
        assert_eq!(col_bool.len(), 5);
        assert_eq!(col_bool.get_as_f64(0), Some(1.0));
        assert_eq!(col_bool.get_as_f64(1), Some(0.0));
        assert_eq!(col_bool.get_as_f64(3), Some(1.0));
        assert_eq!(col_bool.get_as_f64(5), None);
        assert_eq!(col_bool.get_scalar_value(0), Some(ScalarValue::Bool(true)));
        assert_eq!(col_bool.get_scalar_value(1), Some(ScalarValue::Bool(false)));
        assert_eq!(col_bool.get_scalar_value(5), None);
    }

    #[test]
    fn test_native_scalar_column_type_mismatch_rejection() {
        let mut col_u8 = NativeScalarColumn::with_capacity(ScalarType::U8, 4);
        let err = col_u8
            .append_slice(ScalarSlice::I16(&[1, 2]))
            .expect_err("should reject type mismatch");
        assert!(err.to_string().contains("mismatch"));

        let mut col_f32 = NativeScalarColumn::with_capacity(ScalarType::F32, 4);
        let err2 = col_f32
            .append_slice(ScalarSlice::F64(&[1.0, 2.0]))
            .expect_err("should reject type mismatch");
        assert!(err2.to_string().contains("mismatch"));

        let mut col_bool = NativeScalarColumn::with_capacity(ScalarType::Bool, 4);
        let err3 = col_bool
            .append_slice(ScalarSlice::U8(&[1, 0]))
            .expect_err("should reject type mismatch");
        assert!(err3.to_string().contains("mismatch"));
    }

    #[test]
    fn test_bit_vec_chunk_push_and_extend() {
        let mut bv = BitVecChunk::new();
        assert_eq!(bv.len(), 0);
        assert!(bv.is_empty());
        assert_eq!(bv.digital_init_level(), 0);

        bv.push(true);
        assert_eq!(bv.len(), 1);
        assert!(!bv.is_empty());
        assert_eq!(bv.get(0), Some(true));
        assert_eq!(bv.get(1), None);
        assert_eq!(bv.digital_init_level(), 1);

        bv.push(false);
        assert_eq!(bv.len(), 2);
        assert_eq!(bv.get(1), Some(false));

        // Extend with 64 bits to cross word boundary
        let pattern_64: Vec<bool> = (0..64).map(|i| i % 2 == 0).collect();
        bv.extend_from_slice(&pattern_64);
        assert_eq!(bv.len(), 66);
        assert_eq!(bv.get(0), Some(true));
        assert_eq!(bv.get(1), Some(false));
        for i in 0..64 {
            assert_eq!(bv.get(2 + i), Some(i % 2 == 0));
        }

        // Test clear
        bv.clear();
        assert_eq!(bv.len(), 0);
        assert!(bv.is_empty());
        assert_eq!(bv.digital_init_level(), 0);

        // Push after clear works cleanly
        bv.push(false);
        bv.push(true);
        assert_eq!(bv.len(), 2);
        assert_eq!(bv.get(0), Some(false));
        assert_eq!(bv.get(1), Some(true));
    }

    #[test]
    fn test_bit_vec_chunk_edge_extraction() {
        // Empty chunk
        let mut bv_empty = BitVecChunk::new();
        bv_empty.compute_edge_indices();
        assert!(bv_empty.edge_indices.is_empty());

        // Single bit false
        let mut bv_single = BitVecChunk::new();
        bv_single.push(false);
        bv_single.compute_edge_indices();
        assert!(bv_single.edge_indices.is_empty());
        bv_single.compute_edge_indices_with_init(Some(true));
        assert_eq!(bv_single.edge_indices, vec![0]);

        // Known sequence: [false, true, true, false, false, true]
        let mut bv = BitVecChunk::new();
        bv.extend_from_slice(&[false, true, true, false, false, true]);
        bv.compute_edge_indices();
        // bit 0: false
        // bit 1: true != false -> edge at 1
        // bit 2: true == true -> no edge
        // bit 3: false != true -> edge at 3
        // bit 4: false == false -> no edge
        // bit 5: true != false -> edge at 5
        assert_eq!(bv.edge_indices, vec![1, 3, 5]);

        // Inter-chunk initial level edge: if prev_level was true, bit 0 (false) is an edge at 0
        bv.compute_edge_indices_with_init(Some(true));
        assert_eq!(bv.edge_indices, vec![0, 1, 3, 5]);

        // Word boundary transitions (around bit 63/64/65)
        let mut bv_words = BitVecChunk::new();
        let mut bits = vec![false; 130];
        bits[63] = true; // bit 62=false, bit 63=true -> rising edge at 63
        bits[64] = false; // bit 63=true, bit 64=false -> falling edge at 64
        bits[65] = true; // bit 64=false, bit 65=true -> rising edge at 65
        bits[66] = true; // bit 65=true, bit 66=true -> no edge
        // bits[67..130] are false -> falling edge at 67
        bv_words.extend_from_slice(&bits);
        bv_words.compute_edge_indices();
        assert_eq!(bv_words.edge_indices, vec![63, 64, 65, 67]);

        // Full 65,536-bit pseudo-random chunk compared against naive ground truth
        let mut bv_full = BitVecChunk::with_capacity(CHUNK_LEN);
        let mut random_bits = Vec::with_capacity(CHUNK_LEN);
        let mut lcg: u64 = 0x1234_5678_9ABC_DEF0;
        for _ in 0..CHUNK_LEN {
            lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1);
            let b = (lcg >> 63) != 0;
            random_bits.push(b);
        }
        bv_full.extend_from_slice(&random_bits);
        assert_eq!(bv_full.len(), CHUNK_LEN);

        bv_full.compute_edge_indices();

        // Calculate naive ground truth
        let mut ground_truth = Vec::new();
        for k in 1..CHUNK_LEN {
            if random_bits[k] != random_bits[k - 1] {
                ground_truth.push(k as u16);
            }
        }
        assert_eq!(bv_full.edge_indices, ground_truth);
        assert!(!bv_full.edge_indices.is_empty());
    }

    #[test]
    fn test_timestamp_column_dual_encoding() {
        let base: TimestampNs = 1_000_000_000;

        // 1. RelU32 within MAX_REL_U32_NS
        let ts_rel = vec![
            base,
            base + 1_000,
            base + 1_000_000,
            base + MAX_REL_U32_NS,
        ];
        let col_rel = TimestampColumn::from_timestamps(base, &ts_rel);
        match &col_rel {
            TimestampColumn::RelU32 { base_ts_ns, rel_ns } => {
                assert_eq!(*base_ts_ns, base);
                assert_eq!(rel_ns.len(), 4);
                assert_eq!(rel_ns[0], 0);
                assert_eq!(rel_ns[1], 1_000);
                assert_eq!(rel_ns[2], 1_000_000);
                assert_eq!(rel_ns[3], u32::MAX);
            }
            TimestampColumn::AbsU64(_) => panic!("expected RelU32 encoding"),
        }
        assert_eq!(col_rel.len(), 4);
        assert_eq!(col_rel.first_ns(), Some(base));
        assert_eq!(col_rel.last_ns(), Some(base + MAX_REL_U32_NS));
        assert_eq!(col_rel.get_ns(1), Some(base + 1_000));

        // 2. Fallback to AbsU64 due to span > 4.29s
        let ts_abs_span = vec![base, base + MAX_REL_U32_NS + 1];
        let col_abs_span = TimestampColumn::from_timestamps(base, &ts_abs_span);
        match &col_abs_span {
            TimestampColumn::AbsU64(abs) => {
                assert_eq!(abs.len(), 2);
                assert_eq!(abs[0], base);
                assert_eq!(abs[1], base + MAX_REL_U32_NS + 1);
            }
            TimestampColumn::RelU32 { .. } => panic!("expected AbsU64 encoding due to span overflow"),
        }

        // 3. Fallback to AbsU64 due to timestamp < base
        let ts_before_base = vec![base - 1, base + 100];
        let col_abs_before = TimestampColumn::from_timestamps(base, &ts_before_base);
        assert!(matches!(col_abs_before, TimestampColumn::AbsU64(_)));

        // 4. from_slice automatic base detection
        let col_from_slice = TimestampColumn::from_slice(&[5_000, 5_500, 6_000]);
        assert_eq!(col_from_slice.first_ns(), Some(5_000));
        assert_eq!(col_from_slice.last_ns(), Some(6_000));

        // 5. Dynamic push upgrading RelU32 to AbsU64
        let mut col_dynamic = TimestampColumn::from_timestamps(base, &[base, base + 100]);
        assert!(matches!(col_dynamic, TimestampColumn::RelU32 { .. }));
        col_dynamic.push(base + 200);
        assert!(matches!(col_dynamic, TimestampColumn::RelU32 { .. }));
        assert_eq!(col_dynamic.len(), 3);

        // Push timestamp exceeding 4.29s span -> upgrades to AbsU64
        col_dynamic.push(base + MAX_REL_U32_NS + 500);
        assert!(matches!(col_dynamic, TimestampColumn::AbsU64(_)));
        assert_eq!(col_dynamic.len(), 4);
        assert_eq!(col_dynamic.get_ns(0), Some(base));
        assert_eq!(col_dynamic.get_ns(1), Some(base + 100));
        assert_eq!(col_dynamic.get_ns(2), Some(base + 200));
        assert_eq!(col_dynamic.get_ns(3), Some(base + MAX_REL_U32_NS + 500));

        // 6. Clear
        col_dynamic.clear();
        assert_eq!(col_dynamic.len(), 0);
        assert!(col_dynamic.is_empty());
    }

    #[test]
    fn test_event_data_chunk_state_transitions() {
        let mut event_chunk = EventDataChunk::new_state_transitions(8);
        assert_eq!(event_chunk.len(), 0);
        assert!(event_chunk.is_empty());
        assert_eq!(event_chunk.get_state_at(100), None);

        // Append valid transitions
        event_chunk.append_transition(100, 1).unwrap();
        event_chunk.append_transition(200, 2).unwrap();
        // Equal timestamp allowed for simultaneous transition
        event_chunk.append_transition(200, 3).unwrap();
        event_chunk.append_transition(300, 0).unwrap();

        assert_eq!(event_chunk.len(), 4);
        assert_eq!(event_chunk.get_transition(0), Some((100, 1)));
        assert_eq!(event_chunk.get_transition(2), Some((200, 3)));
        assert_eq!(event_chunk.get_transition(4), None);

        // Binary search lookup via get_state_at
        assert_eq!(event_chunk.get_state_at(50), None); // before first transition
        assert_eq!(event_chunk.get_state_at(100), Some(1));
        assert_eq!(event_chunk.get_state_at(150), Some(1));
        assert_eq!(event_chunk.get_state_at(200), Some(3)); // latest state at instant 200
        assert_eq!(event_chunk.get_state_at(250), Some(3));
        assert_eq!(event_chunk.get_state_at(300), Some(0));
        assert_eq!(event_chunk.get_state_at(1_000), Some(0)); // after last transition

        // Non-monotonic transition rejection
        let err = event_chunk
            .append_transition(250, 99)
            .expect_err("should reject backward timestamp");
        assert!(err.to_string().contains("non-monotonic"));
    }

    #[test]
    fn test_scalar_data_chunk_validation_and_access() {
        let base_ts: TimestampNs = 10_000_000;
        let ts_col = TimestampColumn::from_timestamps(
            base_ts,
            &[base_ts, base_ts + 1_000, base_ts + 2_000],
        );

        let mut val_col = NativeScalarColumn::with_capacity(ScalarType::F64, 3);
        val_col.append_slice(ScalarSlice::F64(&[1.0, 2.0, 3.0])).unwrap();

        let scale = LinearScale::new(10.0, -5.0); // y = x * 10 - 5
        let chunk = ScalarDataChunk::new(ts_col.clone(), val_col, scale)
            .expect("should construct valid ScalarDataChunk");

        assert_eq!(chunk.len(), 3);
        assert!(!chunk.is_empty());
        assert_eq!(chunk.time_range_ns(), Some((base_ts, base_ts + 2_000)));

        // Calibrated retrieval
        assert_eq!(chunk.get_sample_as_f64(0), Some((base_ts, 5.0))); // 1.0 * 10 - 5 = 5.0
        assert_eq!(chunk.get_sample_as_f64(1), Some((base_ts + 1_000, 15.0))); // 2.0 * 10 - 5 = 15.0
        assert_eq!(chunk.get_sample_as_f64(2), Some((base_ts + 2_000, 25.0))); // 3.0 * 10 - 5 = 25.0
        assert_eq!(chunk.get_sample_as_f64(3), None);

        // Length mismatch rejection
        let mut mismatch_vals = NativeScalarColumn::with_capacity(ScalarType::F64, 2);
        mismatch_vals.append_slice(ScalarSlice::F64(&[1.0, 2.0])).unwrap();
        let err = ScalarDataChunk::new(ts_col, mismatch_vals, scale)
            .expect_err("should reject length mismatch");
        assert!(err.to_string().contains("length mismatch"));

        // Non-monotonic timestamps rejection
        let mut bad_ts_vec = AlignedVec::with_capacity(2);
        bad_ts_vec.extend_from_slice(&[200_u64, 100_u64]);
        let bad_ts_col = TimestampColumn::AbsU64(bad_ts_vec);
        let mut ok_vals = NativeScalarColumn::with_capacity(ScalarType::F64, 2);
        ok_vals.append_slice(ScalarSlice::F64(&[1.0, 2.0])).unwrap();
        let err_mono = ScalarDataChunk::new(bad_ts_col, ok_vals, scale)
            .expect_err("should reject non-monotonic timestamps");
        assert!(err_mono.to_string().contains("non-monotonic"));
    }
}
