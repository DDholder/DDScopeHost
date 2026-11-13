//! Cache-line aligned columnar buffers, native-width chunks, M4 pyramids, and chunk references.

use std::marker::PhantomData;
use std::mem::{align_of, size_of};
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use bytemuck::{cast_slice, cast_slice_mut, Pod, Zeroable};
use serde::{Deserialize, Serialize};

use super::ids::TimestampNs;
use super::scalar::{LinearScale, ScalarType};

/// Fixed sample capacity per sealed scalar chunk (`2^16`).
pub const CHUNK_LEN: usize = 65_536;

/// Cache-line and DDSF frame alignment boundary in bytes.
pub const CACHE_LINE_BYTES: usize = 64;

/// Branching factor for the multi-level M4 envelope pyramid.
pub const M4_BRANCH_FACTOR: usize = 64;

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
}

/// Sealed chunk payload for a `ScalarSeries` channel.
#[derive(Clone, Debug, PartialEq)]
pub struct ScalarDataChunk {
    pub timestamps: TimestampColumn,
    pub values: NativeScalarColumn,
    pub calibration: LinearScale,
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
            Self::Scalar(c) => c.timestamps.byte_footprint() + c.values.byte_footprint(),
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
