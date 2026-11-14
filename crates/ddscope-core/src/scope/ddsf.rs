//! Frozen `DDSF v1` (DDScope Frame v1) zero-copy little-endian binary wire specification.
//!
//! All headers and records are naturally aligned (8/16/32/64 bytes) so the frontend can
//! construct `DataView` and `Float32Array` slices directly over shared or IPC buffers.

use bytemuck::{bytes_of, cast_slice, from_bytes, try_cast_slice, Pod, Zeroable};

use crate::types::chunk::AlignedBuffer;
use crate::types::error::CoreError;

/// Little-endian magic integer for ASCII `"DDSF"` (`[0x44, 0x44, 0x53, 0x46]`).
pub const DDSF_MAGIC: u32 = 0x4653_4444;

/// Protocol version frozen by M1 (`T03`).
pub const DDSF_VERSION_V1: u16 = 1;

/// Fixed byte length of [`DdsfFrameHeader`].
pub const DDSF_HEADER_BYTES: usize = 64;

/// Fixed byte length of [`DdsfScopePacketHeader`].
pub const DDSF_SCOPE_HEADER_BYTES: usize = 32;

/// Fixed byte length of [`DdsfTrackBlockHeader`].
pub const DDSF_TRACK_HEADER_BYTES: usize = 32;

/// Fixed byte length of [`DdsfTrackStats`].
pub const DDSF_TRACK_STATS_BYTES: usize = 32;

/// Fixed byte length of [`DdsfSnapshotEntry`].
pub const DDSF_SNAPSHOT_ENTRY_BYTES: usize = 16;

/// Frame header flag indicating global memory quota warning threshold reached.
pub const DDSF_FLAG_QUOTA_WARNING: u16 = 1 << 0;

/// Frame header flag indicating one or more scopes are holding pinned chunks.
pub const DDSF_FLAG_PINNED_ACTIVE: u16 = 1 << 1;

/// Track block flag indicating a [`DdsfTrackStats`] block immediately follows [`DdsfTrackBlockHeader`].
pub const DDSF_TRACK_FLAG_HAS_STATS: u16 = 1 << 0;

/// Track block flag indicating displayed samples were clipped to window bounds.
pub const DDSF_TRACK_FLAG_CLIPPED: u16 = 1 << 1;

/// Rounds `len` up to the nearest 8-byte boundary.
#[inline]
pub const fn align_up_8(len: usize) -> usize {
    (len + 7) & !7
}

/// Rounds `len` up to the nearest 64-byte cache-line boundary.
#[inline]
pub const fn align_up_64(len: usize) -> usize {
    (len + 63) & !63
}

/// Payload representation stored inside a `TrackBlock`.
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum DdsfBlockKind {
    /// Interleaved `f32[2 * N]` pairs `[x_norm, y_val]` ([`DdsfAnalogPoint`]).
    AnalogRawXY = 1,
    /// Interleaved `f32[5 * P]` columns `[x_norm, v_first, v_min, v_max, v_last]` ([`DdsfM4Column`]).
    AnalogM4Column = 2,
    /// `f32[E]` normalized edge positions (`x_norm in [0.0, 1.0]`); initial level is in header.
    DigitalEdges = 3,
    /// Array of [`DdsfStateRibbon`] intervals `[x0_norm, x1_norm, state_id, color_idx]`.
    StateRibbons = 4,
    /// Reserved for Phase 2 FFT spectrum frames (`f32[M]`, `aux_f32_0 = f_start`, `aux_f32_1 = df`).
    SpectrumFrame = 5,
}

impl DdsfBlockKind {
    /// Returns the `u8` wire code.
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Parses a `u8` wire code into [`DdsfBlockKind`].
    pub const fn from_u8(raw: u8) -> Option<Self> {
        match raw {
            1 => Some(Self::AnalogRawXY),
            2 => Some(Self::AnalogM4Column),
            3 => Some(Self::DigitalEdges),
            4 => Some(Self::StateRibbons),
            5 => Some(Self::SpectrumFrame),
            _ => None,
        }
    }
}

/// Latest-value snapshot scalar classification in [`DdsfSnapshotEntry::kind`].
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum DdsfSnapshotKind {
    Scalar = 0,
    Bool = 1,
    StateEnum = 2,
}

impl DdsfSnapshotKind {
    /// Returns the `u8` wire code.
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Parses a `u8` wire code into [`DdsfSnapshotKind`].
    pub const fn from_u8(raw: u8) -> Option<Self> {
        match raw {
            0 => Some(Self::Scalar),
            1 => Some(Self::Bool),
            2 => Some(Self::StateEnum),
            _ => None,
        }
    }
}

/// 64-byte root frame header at offset `0x0000` of every `DDSF v1` frame.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Pod, Zeroable)]
pub struct DdsfFrameHeader {
    pub magic: u32,
    pub version: u16,
    pub flags: u16,
    pub frame_seq: u32,
    pub scope_count: u16,
    pub snapshot_count: u16,
    pub host_now_ns: u64,
    pub total_byte_len: u64,
    pub reserved_padding: [u8; 32],
}

impl DdsfFrameHeader {
    /// Constructs a valid `DDSF v1` frame header.
    pub const fn new_v1(
        frame_seq: u32,
        scope_count: u16,
        snapshot_count: u16,
        host_now_ns: u64,
        total_byte_len: u64,
        flags: u16,
    ) -> Self {
        Self {
            magic: DDSF_MAGIC,
            version: DDSF_VERSION_V1,
            flags,
            frame_seq,
            scope_count,
            snapshot_count,
            host_now_ns,
            total_byte_len,
            reserved_padding: [0u8; 32],
        }
    }

    /// Validates magic, version, and declared byte length against `buf_len`.
    pub const fn validate(&self, buf_len: usize) -> Result<(), &'static str> {
        if self.magic != DDSF_MAGIC {
            return Err("invalid DDSF magic");
        }
        if self.version != DDSF_VERSION_V1 {
            return Err("unsupported DDSF version");
        }
        if (self.total_byte_len as usize) < DDSF_HEADER_BYTES {
            return Err("total_byte_len smaller than header size");
        }
        if (self.total_byte_len as usize) > buf_len {
            return Err("total_byte_len exceeds buffer length");
        }
        Ok(())
    }
}

/// 32-byte per-scope viewport packet header immediately preceding its track blocks.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Pod, Zeroable)]
pub struct DdsfScopePacketHeader {
    pub scope_id: u32,
    pub trigger_state: u8,
    pub mode: u8,
    pub track_block_count: u16,
    pub win_t_start_ns: u64,
    pub win_t_span_ns: u64,
    pub subpixel_offset_ns: f32,
    pub packet_byte_len: u32,
}

/// 32-byte per-track block header ensuring 8-byte aligned payload offsets.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Pod, Zeroable)]
pub struct DdsfTrackBlockHeader {
    pub channel_id: u32,
    pub block_kind: u8,
    /// Left-edge initial logic level (`0` or `1`) for `DigitalEdges` tracks.
    pub digital_init_level: u8,
    pub track_flags: u16,
    pub element_count: u32,
    pub data_byte_len: u32,
    pub block_byte_len: u32,
    pub aux_f32_0: f32,
    pub aux_f32_1: f32,
    pub reserved: u32,
}

impl DdsfTrackBlockHeader {
    /// Returns the byte offset of [`DdsfTrackStats`] relative to the start of this block, if present.
    #[inline]
    pub const fn stats_offset(&self) -> Option<usize> {
        if (self.track_flags & DDSF_TRACK_FLAG_HAS_STATS) != 0 {
            Some(DDSF_TRACK_HEADER_BYTES)
        } else {
            None
        }
    }

    /// Returns the 8-byte aligned byte offset of the element payload relative to block start.
    #[inline]
    pub const fn payload_offset(&self) -> usize {
        if (self.track_flags & DDSF_TRACK_FLAG_HAS_STATS) != 0 {
            DDSF_TRACK_HEADER_BYTES + DDSF_TRACK_STATS_BYTES
        } else {
            DDSF_TRACK_HEADER_BYTES
        }
    }
}

/// 32-byte window measurement summary attached when `DDSF_TRACK_FLAG_HAS_STATS` is set.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct DdsfTrackStats {
    pub min: f32,
    pub max: f32,
    pub vpp: f32,
    pub mean: f32,
    pub rms: f32,
    pub freq_hz: f32,
    pub duty: f32,
    pub reserved: f32,
}

/// Element layout for [`DdsfBlockKind::AnalogRawXY`] (8 bytes).
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct DdsfAnalogPoint {
    pub x_norm: f32,
    pub y_val: f32,
}

/// Element layout for [`DdsfBlockKind::AnalogM4Column`] (20 bytes).
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct DdsfM4Column {
    pub x_norm: f32,
    pub v_first: f32,
    pub v_min: f32,
    pub v_max: f32,
    pub v_last: f32,
}

/// Element layout for [`DdsfBlockKind::StateRibbons`] (16 bytes).
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct DdsfStateRibbon {
    pub x0_norm: f32,
    pub x1_norm: f32,
    pub state_id: i32,
    pub color_idx: u32,
}

/// 16-byte latest-value snapshot entry appended at the tail of each `DDSF v1` frame.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct DdsfSnapshotEntry {
    pub channel_num_id: u32,
    pub kind: u8,
    pub pad: [u8; 3],
    pub val_f64: f64,
}

// ============================================================================
// Owned Payload Types (Compatible API)
// ============================================================================

/// Scope payload descriptor for DDSF frame serialization.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScopeViewportPayload {
    pub scope_id: u32,
    pub trigger_state: u8,
    pub mode: u8,
    pub win_t_start_ns: u64,
    pub win_t_span_ns: u64,
    pub subpixel_offset_ns: f32,
    pub tracks: Vec<TrackPayload>,
}

/// Track payload descriptor containing typed elements and optional window statistics.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackPayload {
    pub channel_id: u32,
    pub block_kind: DdsfBlockKind,
    pub digital_init_level: u8,
    pub flags: u16,
    pub stats: Option<DdsfTrackStats>,
    pub raw_points: Vec<DdsfAnalogPoint>,
    pub m4_columns: Vec<DdsfM4Column>,
    pub digital_edges: Vec<f32>,
    pub state_ribbons: Vec<DdsfStateRibbon>,
    pub spectrum_bins: Vec<f32>,
    pub aux_f32_0: f32,
    pub aux_f32_1: f32,
}

impl TrackPayload {
    /// Creates an `AnalogRawXY` track payload from a point vector.
    pub fn new_raw_xy(channel_id: u32, points: Vec<DdsfAnalogPoint>) -> Self {
        Self::new_raw_xy_with_stats(channel_id, points, None)
    }

    /// Creates an `AnalogRawXY` track payload with optional window statistics.
    pub fn new_raw_xy_with_stats(
        channel_id: u32,
        points: Vec<DdsfAnalogPoint>,
        stats: Option<DdsfTrackStats>,
    ) -> Self {
        let flags = if stats.is_some() {
            DDSF_TRACK_FLAG_HAS_STATS
        } else {
            0
        };
        Self {
            channel_id,
            block_kind: DdsfBlockKind::AnalogRawXY,
            digital_init_level: 0,
            flags,
            stats,
            raw_points: points,
            m4_columns: Vec::new(),
            digital_edges: Vec::new(),
            state_ribbons: Vec::new(),
            spectrum_bins: Vec::new(),
            aux_f32_0: 0.0,
            aux_f32_1: 0.0,
        }
    }

    /// Creates an `AnalogM4Column` track payload with optional window statistics.
    pub fn new_m4(
        channel_id: u32,
        cols: Vec<DdsfM4Column>,
        stats: Option<DdsfTrackStats>,
    ) -> Self {
        let flags = if stats.is_some() {
            DDSF_TRACK_FLAG_HAS_STATS
        } else {
            0
        };
        Self {
            channel_id,
            block_kind: DdsfBlockKind::AnalogM4Column,
            digital_init_level: 0,
            flags,
            stats,
            raw_points: Vec::new(),
            m4_columns: cols,
            digital_edges: Vec::new(),
            state_ribbons: Vec::new(),
            spectrum_bins: Vec::new(),
            aux_f32_0: 0.0,
            aux_f32_1: 0.0,
        }
    }

    /// Creates a `DigitalEdges` track payload with normalized edge transitions.
    pub fn new_digital_edges(channel_id: u32, init_lvl: u8, edges: Vec<f32>) -> Self {
        Self {
            channel_id,
            block_kind: DdsfBlockKind::DigitalEdges,
            digital_init_level: init_lvl,
            flags: 0,
            stats: None,
            raw_points: Vec::new(),
            m4_columns: Vec::new(),
            digital_edges: edges,
            state_ribbons: Vec::new(),
            spectrum_bins: Vec::new(),
            aux_f32_0: 0.0,
            aux_f32_1: 0.0,
        }
    }

    /// Creates a `StateRibbons` track payload with discrete state intervals.
    pub fn new_state_ribbons(channel_id: u32, ribbons: Vec<DdsfStateRibbon>) -> Self {
        Self {
            channel_id,
            block_kind: DdsfBlockKind::StateRibbons,
            digital_init_level: 0,
            flags: 0,
            stats: None,
            raw_points: Vec::new(),
            m4_columns: Vec::new(),
            digital_edges: Vec::new(),
            state_ribbons: ribbons,
            spectrum_bins: Vec::new(),
            aux_f32_0: 0.0,
            aux_f32_1: 0.0,
        }
    }

    /// Creates a `SpectrumFrame` track payload (Phase 2 reserve).
    pub fn new_spectrum(channel_id: u32, f_start: f32, df: f32, bins: Vec<f32>) -> Self {
        Self {
            channel_id,
            block_kind: DdsfBlockKind::SpectrumFrame,
            digital_init_level: 0,
            flags: 0,
            stats: None,
            raw_points: Vec::new(),
            m4_columns: Vec::new(),
            digital_edges: Vec::new(),
            state_ribbons: Vec::new(),
            spectrum_bins: bins,
            aux_f32_0: f_start,
            aux_f32_1: df,
        }
    }

    /// Creates a zero-allocation borrowed view over this payload.
    pub fn as_view(&self) -> TrackBlockView<'_> {
        let payload = match self.block_kind {
            DdsfBlockKind::AnalogRawXY => TrackPayloadSlice::AnalogRawXY(&self.raw_points),
            DdsfBlockKind::AnalogM4Column => TrackPayloadSlice::AnalogM4Column(&self.m4_columns),
            DdsfBlockKind::DigitalEdges => TrackPayloadSlice::DigitalEdges(&self.digital_edges),
            DdsfBlockKind::StateRibbons => TrackPayloadSlice::StateRibbons(&self.state_ribbons),
            DdsfBlockKind::SpectrumFrame => TrackPayloadSlice::SpectrumFrame(&self.spectrum_bins),
        };
        TrackBlockView {
            channel_id: self.channel_id,
            block_kind: self.block_kind,
            digital_init_level: self.digital_init_level,
            flags: self.flags,
            stats: self.stats.as_ref(),
            payload,
            aux_f32_0: self.aux_f32_0,
            aux_f32_1: self.aux_f32_1,
        }
    }
}

// ============================================================================
// Zero-Allocation Borrowed Views
// ============================================================================

/// Borrowed slice representation of track block element data.
#[derive(Copy, Clone, Debug)]
pub enum TrackPayloadSlice<'a> {
    AnalogRawXY(&'a [DdsfAnalogPoint]),
    AnalogM4Column(&'a [DdsfM4Column]),
    DigitalEdges(&'a [f32]),
    StateRibbons(&'a [DdsfStateRibbon]),
    SpectrumFrame(&'a [f32]),
    RawBytes(&'a [u8]),
    Empty,
}

impl<'a> TrackPayloadSlice<'a> {
    /// Returns the number of logical elements in the slice.
    #[inline]
    pub fn element_count(&self) -> usize {
        match self {
            Self::AnalogRawXY(s) => s.len(),
            Self::AnalogM4Column(s) => s.len(),
            Self::DigitalEdges(s) => s.len(),
            Self::StateRibbons(s) => s.len(),
            Self::SpectrumFrame(s) => s.len(),
            Self::RawBytes(s) => s.len(),
            Self::Empty => 0,
        }
    }

    /// Returns the unpadded raw byte slice.
    #[inline]
    pub fn as_bytes(&self) -> &'a [u8] {
        match self {
            Self::AnalogRawXY(s) => cast_slice(s),
            Self::AnalogM4Column(s) => cast_slice(s),
            Self::DigitalEdges(s) => cast_slice(s),
            Self::StateRibbons(s) => cast_slice(s),
            Self::SpectrumFrame(s) => cast_slice(s),
            Self::RawBytes(s) => s,
            Self::Empty => &[],
        }
    }
}

/// Zero-allocation borrowed track view descriptor.
#[derive(Clone, Debug)]
pub struct TrackBlockView<'a> {
    pub channel_id: u32,
    pub block_kind: DdsfBlockKind,
    pub digital_init_level: u8,
    pub flags: u16,
    pub stats: Option<&'a DdsfTrackStats>,
    pub payload: TrackPayloadSlice<'a>,
    pub aux_f32_0: f32,
    pub aux_f32_1: f32,
}

/// Zero-allocation borrowed scope packet view descriptor.
#[derive(Clone, Debug)]
pub struct ScopePacketView<'a> {
    pub scope_id: u32,
    pub trigger_state: u8,
    pub mode: u8,
    pub win_t_start_ns: u64,
    pub win_t_span_ns: u64,
    pub subpixel_offset_ns: f32,
    pub tracks: &'a [TrackBlockView<'a>],
}

// ============================================================================
// Production DdsfFramePacker Serializer
// ============================================================================

/// High-throughput zero secondary heap allocation serializer for DDSF v1 binary frames.
#[derive(Debug)]
pub struct DdsfFramePacker {
    buffer: AlignedBuffer,
}

impl Default for DdsfFramePacker {
    fn default() -> Self {
        Self::new(256 * 1024)
    }
}

impl DdsfFramePacker {
    /// Minimum initial capacity (4 KB) ensuring sufficient room for headers and basic payloads.
    pub const DEFAULT_INITIAL_CAPACITY: usize = 4096;

    /// Creates a new packer with pre-allocated buffer capacity.
    ///
    /// The buffer is pre-initialized and cleared so that subsequent writes up to
    /// `initial_capacity` bytes are guaranteed to perform zero heap allocations.
    pub fn new(initial_capacity: usize) -> Self {
        let cap = initial_capacity.max(Self::DEFAULT_INITIAL_CAPACITY);
        let mut buffer = AlignedBuffer::with_capacity(cap);
        buffer.resize(cap, 0);
        buffer.clear();
        Self { buffer }
    }

    /// Creates a new packer with the specified initial capacity.
    #[inline]
    pub fn with_capacity(initial_capacity: usize) -> Self {
        Self::new(initial_capacity)
    }

    /// Returns an immutable reference to the packed byte slice of the most recent frame.
    #[inline]
    pub fn buffer(&self) -> &[u8] {
        self.buffer.as_slice()
    }

    /// Returns the current total capacity of the internal aligned buffer in bytes.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.buffer.capacity()
    }

    /// Serializes a complete DDSF v1 frame into the reusable internal buffer.
    pub fn pack_frame(
        &mut self,
        frame_seq: u32,
        host_now_ns: u64,
        flags: u16,
        scopes: &[ScopeViewportPayload],
        snapshots: &[DdsfSnapshotEntry],
    ) -> Result<&[u8], CoreError> {
        self.buffer.clear();

        // Pre-reserve capacity if required size exceeds current capacity
        let mut required_bytes = DDSF_HEADER_BYTES;
        for scope in scopes {
            required_bytes += DDSF_SCOPE_HEADER_BYTES;
            for track in &scope.tracks {
                let has_stats = track.stats.is_some() || (track.flags & DDSF_TRACK_FLAG_HAS_STATS) != 0;
                let stats_len = if has_stats { DDSF_TRACK_STATS_BYTES } else { 0 };
                let data_len = match track.block_kind {
                    DdsfBlockKind::AnalogRawXY => track.raw_points.len() * std::mem::size_of::<DdsfAnalogPoint>(),
                    DdsfBlockKind::AnalogM4Column => track.m4_columns.len() * std::mem::size_of::<DdsfM4Column>(),
                    DdsfBlockKind::DigitalEdges => track.digital_edges.len() * std::mem::size_of::<f32>(),
                    DdsfBlockKind::StateRibbons => track.state_ribbons.len() * std::mem::size_of::<DdsfStateRibbon>(),
                    DdsfBlockKind::SpectrumFrame => track.spectrum_bins.len() * std::mem::size_of::<f32>(),
                };
                required_bytes += DDSF_TRACK_HEADER_BYTES + stats_len + align_up_8(data_len);
            }
        }
        required_bytes += snapshots.len() * DDSF_SNAPSHOT_ENTRY_BYTES;
        if required_bytes > self.buffer.capacity() {
            self.buffer.resize(required_bytes, 0);
            self.buffer.clear();
        }

        // 1. Placeholder 64-byte frame header
        let zero_header = DdsfFrameHeader::new_v1(
            frame_seq,
            scopes.len() as u16,
            snapshots.len() as u16,
            host_now_ns,
            0,
            flags,
        );
        self.buffer.extend_from_slice(bytes_of(&zero_header));

        // 2. Pack scope viewport packets
        for scope in scopes {
            let scope_header_pos = self.buffer.len();
            let zero_scope_hdr = DdsfScopePacketHeader {
                scope_id: scope.scope_id,
                trigger_state: scope.trigger_state,
                mode: scope.mode,
                track_block_count: scope.tracks.len() as u16,
                win_t_start_ns: scope.win_t_start_ns,
                win_t_span_ns: scope.win_t_span_ns,
                subpixel_offset_ns: scope.subpixel_offset_ns,
                packet_byte_len: 0,
            };
            self.buffer.extend_from_slice(bytes_of(&zero_scope_hdr));

            // Pack tracks
            for track in &scope.tracks {
                let has_stats = track.stats.is_some() || (track.flags & DDSF_TRACK_FLAG_HAS_STATS) != 0;
                let mut track_flags = track.flags;
                if has_stats {
                    track_flags |= DDSF_TRACK_FLAG_HAS_STATS;
                }

                let (element_count, data_byte_len, payload_bytes): (usize, usize, &[u8]) =
                    match track.block_kind {
                        DdsfBlockKind::AnalogRawXY => (
                            track.raw_points.len(),
                            track.raw_points.len() * std::mem::size_of::<DdsfAnalogPoint>(),
                            cast_slice(&track.raw_points),
                        ),
                        DdsfBlockKind::AnalogM4Column => (
                            track.m4_columns.len(),
                            track.m4_columns.len() * std::mem::size_of::<DdsfM4Column>(),
                            cast_slice(&track.m4_columns),
                        ),
                        DdsfBlockKind::DigitalEdges => (
                            track.digital_edges.len(),
                            track.digital_edges.len() * std::mem::size_of::<f32>(),
                            cast_slice(&track.digital_edges),
                        ),
                        DdsfBlockKind::StateRibbons => (
                            track.state_ribbons.len(),
                            track.state_ribbons.len() * std::mem::size_of::<DdsfStateRibbon>(),
                            cast_slice(&track.state_ribbons),
                        ),
                        DdsfBlockKind::SpectrumFrame => (
                            track.spectrum_bins.len(),
                            track.spectrum_bins.len() * std::mem::size_of::<f32>(),
                            cast_slice(&track.spectrum_bins),
                        ),
                    };

                let aligned_data_len = align_up_8(data_byte_len);
                let stats_len = if has_stats { DDSF_TRACK_STATS_BYTES } else { 0 };
                let block_byte_len = DDSF_TRACK_HEADER_BYTES + stats_len + aligned_data_len;

                let track_hdr = DdsfTrackBlockHeader {
                    channel_id: track.channel_id,
                    block_kind: track.block_kind.as_u8(),
                    digital_init_level: track.digital_init_level,
                    track_flags,
                    element_count: element_count as u32,
                    data_byte_len: data_byte_len as u32,
                    block_byte_len: block_byte_len as u32,
                    aux_f32_0: track.aux_f32_0,
                    aux_f32_1: track.aux_f32_1,
                    reserved: 0,
                };
                self.buffer.extend_from_slice(bytes_of(&track_hdr));

                if let Some(stats) = &track.stats {
                    self.buffer.extend_from_slice(bytes_of(stats));
                } else if has_stats {
                    let default_stats = DdsfTrackStats::default();
                    self.buffer.extend_from_slice(bytes_of(&default_stats));
                }

                self.buffer.extend_from_slice(payload_bytes);

                // Zero-pad payload to 8-byte boundary using fixed stack buffer
                let pad = aligned_data_len - data_byte_len;
                if pad > 0 {
                    const ZERO_PAD: [u8; 8] = [0u8; 8];
                    self.buffer.extend_from_slice(&ZERO_PAD[..pad]);
                }
            }

            let scope_packet_len = (self.buffer.len() - scope_header_pos) as u32;
            let packet_len_bytes = scope_packet_len.to_le_bytes();
            let slice = self.buffer.as_mut_slice();
            slice[scope_header_pos + 28..scope_header_pos + 32].copy_from_slice(&packet_len_bytes);
        }

        // 3. Pack snapshot entries
        if !snapshots.is_empty() {
            self.buffer.extend_from_slice(cast_slice(snapshots));
        }

        // 4. Update total_byte_len in root header
        let total_byte_len = self.buffer.len() as u64;
        let len_bytes = total_byte_len.to_le_bytes();
        let slice = self.buffer.as_mut_slice();
        slice[24..32].copy_from_slice(&len_bytes);

        // 5. Header validation check
        let root_hdr: &DdsfFrameHeader = from_bytes(&self.buffer.as_slice()[..DDSF_HEADER_BYTES]);
        if let Err(err) = root_hdr.validate(self.buffer.len()) {
            return Err(CoreError::Protocol {
                message: err.to_string(),
                recoverable: false,
            });
        }

        Ok(self.buffer.as_slice())
    }

    /// Serializes a complete DDSF v1 frame from zero-allocation borrowed views.
    pub fn pack_frame_borrowed(
        &mut self,
        frame_seq: u32,
        host_now_ns: u64,
        flags: u16,
        scopes: &[ScopePacketView<'_>],
        snapshots: &[DdsfSnapshotEntry],
    ) -> Result<&[u8], CoreError> {
        self.buffer.clear();

        // Pre-reserve capacity if required size exceeds current capacity
        let mut required_bytes = DDSF_HEADER_BYTES;
        for scope in scopes {
            required_bytes += DDSF_SCOPE_HEADER_BYTES;
            for track in scope.tracks {
                let has_stats = track.stats.is_some() || (track.flags & DDSF_TRACK_FLAG_HAS_STATS) != 0;
                let stats_len = if has_stats { DDSF_TRACK_STATS_BYTES } else { 0 };
                let data_len = track.payload.as_bytes().len();
                required_bytes += DDSF_TRACK_HEADER_BYTES + stats_len + align_up_8(data_len);
            }
        }
        required_bytes += snapshots.len() * DDSF_SNAPSHOT_ENTRY_BYTES;
        if required_bytes > self.buffer.capacity() {
            self.buffer.resize(required_bytes, 0);
            self.buffer.clear();
        }

        // 1. Placeholder 64-byte frame header
        let zero_header = DdsfFrameHeader::new_v1(
            frame_seq,
            scopes.len() as u16,
            snapshots.len() as u16,
            host_now_ns,
            0,
            flags,
        );
        self.buffer.extend_from_slice(bytes_of(&zero_header));

        // 2. Pack scope viewport packets
        for scope in scopes {
            let scope_header_pos = self.buffer.len();
            let zero_scope_hdr = DdsfScopePacketHeader {
                scope_id: scope.scope_id,
                trigger_state: scope.trigger_state,
                mode: scope.mode,
                track_block_count: scope.tracks.len() as u16,
                win_t_start_ns: scope.win_t_start_ns,
                win_t_span_ns: scope.win_t_span_ns,
                subpixel_offset_ns: scope.subpixel_offset_ns,
                packet_byte_len: 0,
            };
            self.buffer.extend_from_slice(bytes_of(&zero_scope_hdr));

            // Pack tracks
            for track in scope.tracks {
                let has_stats = track.stats.is_some() || (track.flags & DDSF_TRACK_FLAG_HAS_STATS) != 0;
                let mut track_flags = track.flags;
                if has_stats {
                    track_flags |= DDSF_TRACK_FLAG_HAS_STATS;
                }

                let payload_bytes = track.payload.as_bytes();
                let element_count = track.payload.element_count();
                let data_byte_len = payload_bytes.len();

                let aligned_data_len = align_up_8(data_byte_len);
                let stats_len = if has_stats { DDSF_TRACK_STATS_BYTES } else { 0 };
                let block_byte_len = DDSF_TRACK_HEADER_BYTES + stats_len + aligned_data_len;

                let track_hdr = DdsfTrackBlockHeader {
                    channel_id: track.channel_id,
                    block_kind: track.block_kind.as_u8(),
                    digital_init_level: track.digital_init_level,
                    track_flags,
                    element_count: element_count as u32,
                    data_byte_len: data_byte_len as u32,
                    block_byte_len: block_byte_len as u32,
                    aux_f32_0: track.aux_f32_0,
                    aux_f32_1: track.aux_f32_1,
                    reserved: 0,
                };
                self.buffer.extend_from_slice(bytes_of(&track_hdr));

                if let Some(stats) = track.stats {
                    self.buffer.extend_from_slice(bytes_of(stats));
                } else if has_stats {
                    let default_stats = DdsfTrackStats::default();
                    self.buffer.extend_from_slice(bytes_of(&default_stats));
                }

                self.buffer.extend_from_slice(payload_bytes);

                // Zero-pad payload to 8-byte boundary using fixed stack buffer
                let pad = aligned_data_len - data_byte_len;
                if pad > 0 {
                    const ZERO_PAD: [u8; 8] = [0u8; 8];
                    self.buffer.extend_from_slice(&ZERO_PAD[..pad]);
                }
            }

            let scope_packet_len = (self.buffer.len() - scope_header_pos) as u32;
            let packet_len_bytes = scope_packet_len.to_le_bytes();
            let slice = self.buffer.as_mut_slice();
            slice[scope_header_pos + 28..scope_header_pos + 32].copy_from_slice(&packet_len_bytes);
        }

        // 3. Pack snapshot entries
        if !snapshots.is_empty() {
            self.buffer.extend_from_slice(cast_slice(snapshots));
        }

        // 4. Update total_byte_len in root header
        let total_byte_len = self.buffer.len() as u64;
        let len_bytes = total_byte_len.to_le_bytes();
        let slice = self.buffer.as_mut_slice();
        slice[24..32].copy_from_slice(&len_bytes);

        // 5. Header validation check
        let root_hdr: &DdsfFrameHeader = from_bytes(&self.buffer.as_slice()[..DDSF_HEADER_BYTES]);
        if let Err(err) = root_hdr.validate(self.buffer.len()) {
            return Err(CoreError::Protocol {
                message: err.to_string(),
                recoverable: false,
            });
        }

        Ok(self.buffer.as_slice())
    }

    /// Packs a DDSF v1 frame using an existing header reference.
    #[inline]
    pub fn pack_frame_with_header(
        &mut self,
        header: &DdsfFrameHeader,
        scopes: &[ScopeViewportPayload],
        snapshots: &[DdsfSnapshotEntry],
    ) -> Result<&[u8], CoreError> {
        self.pack_frame(header.frame_seq, header.host_now_ns, header.flags, scopes, snapshots)
    }

    /// Packs a DDSF v1 frame using an existing header reference and borrowed views.
    #[inline]
    pub fn pack_frame_borrowed_with_header(
        &mut self,
        header: &DdsfFrameHeader,
        scopes: &[ScopePacketView<'_>],
        snapshots: &[DdsfSnapshotEntry],
    ) -> Result<&[u8], CoreError> {
        self.pack_frame_borrowed(header.frame_seq, header.host_now_ns, header.flags, scopes, snapshots)
    }
}

// ============================================================================
// Zero-Copy Deserialization Reader View
// ============================================================================

/// Zero-copy accessor and deserializer for raw DDSF v1 frame buffers.
#[derive(Copy, Clone, Debug)]
pub struct DdsfZeroCopyReader<'a> {
    buffer: &'a [u8],
}

impl<'a> DdsfZeroCopyReader<'a> {
    /// Validates the buffer length, magic, version, and frame size, returning a zero-copy reader.
    pub fn new(buffer: &'a [u8]) -> Result<Self, CoreError> {
        if buffer.len() < DDSF_HEADER_BYTES {
            return Err(CoreError::Protocol {
                message: format!(
                    "buffer length ({}) smaller than DDSF frame header ({})",
                    buffer.len(),
                    DDSF_HEADER_BYTES
                ),
                recoverable: false,
            });
        }
        let header: &DdsfFrameHeader = from_bytes(&buffer[..DDSF_HEADER_BYTES]);
        if let Err(reason) = header.validate(buffer.len()) {
            return Err(CoreError::Protocol {
                message: format!("DDSF frame validation error: {reason}"),
                recoverable: false,
            });
        }
        Ok(Self { buffer })
    }

    /// Accesses the root 64-byte frame header.
    #[inline]
    pub fn header(&self) -> &'a DdsfFrameHeader {
        from_bytes(&self.buffer[..DDSF_HEADER_BYTES])
    }

    /// Returns an iterator over all scope viewport packets in the frame.
    #[inline]
    pub fn scopes(&self) -> DdsfScopeIter<'a> {
        DdsfScopeIter {
            buffer: self.buffer,
            offset: DDSF_HEADER_BYTES,
            remaining: self.header().scope_count as usize,
        }
    }

    /// Iterates through all scope viewports and invokes closure with header and track iterator.
    pub fn for_each_scope<F>(&self, mut f: F) -> Result<(), CoreError>
    where
        F: FnMut(&'a DdsfScopePacketHeader, DdsfTrackIter<'a>),
    {
        for scope_view in self.scopes() {
            f(scope_view.header(), scope_view.tracks());
        }
        Ok(())
    }

    /// Returns a slice view of channel latest snapshots appended at the tail of the frame.
    pub fn snapshots(&self) -> Result<&'a [DdsfSnapshotEntry], CoreError> {
        let count = self.header().snapshot_count as usize;
        if count == 0 {
            return Ok(&[]);
        }
        let snap_bytes = count * DDSF_SNAPSHOT_ENTRY_BYTES;
        let total_len = self.header().total_byte_len as usize;
        let snap_start = total_len.saturating_sub(snap_bytes);
        if snap_start < DDSF_HEADER_BYTES || snap_start + snap_bytes > self.buffer.len() {
            return Err(CoreError::Protocol {
                message: "invalid snapshot entries offset or out of bounds".into(),
                recoverable: false,
            });
        }
        Ok(cast_slice(&self.buffer[snap_start..snap_start + snap_bytes]))
    }

    /// Returns the underlying raw byte buffer.
    #[inline]
    pub fn as_bytes(&self) -> &'a [u8] {
        self.buffer
    }
}

/// Iterator over scope viewport packets inside a DDSF v1 frame.
#[derive(Clone, Debug)]
pub struct DdsfScopeIter<'a> {
    buffer: &'a [u8],
    offset: usize,
    remaining: usize,
}

impl<'a> Iterator for DdsfScopeIter<'a> {
    type Item = DdsfScopePacketView<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 || self.offset + DDSF_SCOPE_HEADER_BYTES > self.buffer.len() {
            return None;
        }
        let hdr: &'a DdsfScopePacketHeader =
            from_bytes(&self.buffer[self.offset..self.offset + DDSF_SCOPE_HEADER_BYTES]);
        let packet_len = hdr.packet_byte_len as usize;
        if packet_len < DDSF_SCOPE_HEADER_BYTES || self.offset + packet_len > self.buffer.len() {
            return None;
        }
        let packet_slice = &self.buffer[self.offset..self.offset + packet_len];
        self.offset += packet_len;
        self.remaining -= 1;
        Some(DdsfScopePacketView {
            header: hdr,
            packet_slice,
        })
    }
}

/// Zero-copy view of a single scope viewport packet.
#[derive(Copy, Clone, Debug)]
pub struct DdsfScopePacketView<'a> {
    header: &'a DdsfScopePacketHeader,
    packet_slice: &'a [u8],
}

impl<'a> DdsfScopePacketView<'a> {
    #[inline]
    pub fn header(&self) -> &'a DdsfScopePacketHeader {
        self.header
    }

    #[inline]
    pub fn tracks(&self) -> DdsfTrackIter<'a> {
        DdsfTrackIter {
            data: &self.packet_slice[DDSF_SCOPE_HEADER_BYTES..],
            remaining: self.header.track_block_count as usize,
        }
    }

    #[inline]
    pub fn raw_bytes(&self) -> &'a [u8] {
        self.packet_slice
    }
}

/// Iterator over track blocks within a scope packet.
#[derive(Clone, Debug)]
pub struct DdsfTrackIter<'a> {
    data: &'a [u8],
    remaining: usize,
}

impl<'a> Iterator for DdsfTrackIter<'a> {
    type Item = DdsfTrackBlockView<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 || self.data.len() < DDSF_TRACK_HEADER_BYTES {
            return None;
        }
        let hdr: &'a DdsfTrackBlockHeader =
            from_bytes(&self.data[..DDSF_TRACK_HEADER_BYTES]);
        let block_len = hdr.block_byte_len as usize;
        if block_len < DDSF_TRACK_HEADER_BYTES || block_len > self.data.len() {
            return None;
        }
        let block_slice = &self.data[..block_len];
        self.data = &self.data[block_len..];
        self.remaining -= 1;
        Some(DdsfTrackBlockView {
            header: hdr,
            block_slice,
        })
    }
}

/// Zero-copy view of a single track block.
#[derive(Copy, Clone, Debug)]
pub struct DdsfTrackBlockView<'a> {
    header: &'a DdsfTrackBlockHeader,
    block_slice: &'a [u8],
}

impl<'a> DdsfTrackBlockView<'a> {
    #[inline]
    pub fn header(&self) -> &'a DdsfTrackBlockHeader {
        self.header
    }

    #[inline]
    pub fn stats(&self) -> Option<&'a DdsfTrackStats> {
        if (self.header.track_flags & DDSF_TRACK_FLAG_HAS_STATS) != 0 {
            if self.block_slice.len() >= DDSF_TRACK_HEADER_BYTES + DDSF_TRACK_STATS_BYTES {
                Some(from_bytes(
                    &self.block_slice[DDSF_TRACK_HEADER_BYTES..DDSF_TRACK_HEADER_BYTES + DDSF_TRACK_STATS_BYTES],
                ))
            } else {
                None
            }
        } else {
            None
        }
    }

    #[inline]
    pub fn payload_bytes(&self) -> &'a [u8] {
        let p_offset = self.header.payload_offset();
        let p_len = self.header.data_byte_len as usize;
        if p_offset + p_len <= self.block_slice.len() {
            &self.block_slice[p_offset..p_offset + p_len]
        } else {
            &[]
        }
    }

    pub fn raw_points(&self) -> Result<&'a [DdsfAnalogPoint], CoreError> {
        if self.header.block_kind != DdsfBlockKind::AnalogRawXY.as_u8() {
            return Err(CoreError::Protocol {
                message: format!(
                    "track block kind mismatch: expected AnalogRawXY (1), got {}",
                    self.header.block_kind
                ),
                recoverable: false,
            });
        }
        try_cast_slice(self.payload_bytes()).map_err(|err| CoreError::Protocol {
            message: format!("failed to cast payload to DdsfAnalogPoint: {err}"),
            recoverable: false,
        })
    }

    pub fn m4_columns(&self) -> Result<&'a [DdsfM4Column], CoreError> {
        if self.header.block_kind != DdsfBlockKind::AnalogM4Column.as_u8() {
            return Err(CoreError::Protocol {
                message: format!(
                    "track block kind mismatch: expected AnalogM4Column (2), got {}",
                    self.header.block_kind
                ),
                recoverable: false,
            });
        }
        try_cast_slice(self.payload_bytes()).map_err(|err| CoreError::Protocol {
            message: format!("failed to cast payload to DdsfM4Column: {err}"),
            recoverable: false,
        })
    }

    pub fn digital_edges(&self) -> Result<&'a [f32], CoreError> {
        if self.header.block_kind != DdsfBlockKind::DigitalEdges.as_u8() {
            return Err(CoreError::Protocol {
                message: format!(
                    "track block kind mismatch: expected DigitalEdges (3), got {}",
                    self.header.block_kind
                ),
                recoverable: false,
            });
        }
        try_cast_slice(self.payload_bytes()).map_err(|err| CoreError::Protocol {
            message: format!("failed to cast payload to f32 edges: {err}"),
            recoverable: false,
        })
    }

    pub fn state_ribbons(&self) -> Result<&'a [DdsfStateRibbon], CoreError> {
        if self.header.block_kind != DdsfBlockKind::StateRibbons.as_u8() {
            return Err(CoreError::Protocol {
                message: format!(
                    "track block kind mismatch: expected StateRibbons (4), got {}",
                    self.header.block_kind
                ),
                recoverable: false,
            });
        }
        try_cast_slice(self.payload_bytes()).map_err(|err| CoreError::Protocol {
            message: format!("failed to cast payload to DdsfStateRibbon: {err}"),
            recoverable: false,
        })
    }

    pub fn spectrum_bins(&self) -> Result<&'a [f32], CoreError> {
        if self.header.block_kind != DdsfBlockKind::SpectrumFrame.as_u8() {
            return Err(CoreError::Protocol {
                message: format!(
                    "track block kind mismatch: expected SpectrumFrame (5), got {}",
                    self.header.block_kind
                ),
                recoverable: false,
            });
        }
        try_cast_slice(self.payload_bytes()).map_err(|err| CoreError::Protocol {
            message: format!("failed to cast payload to f32 spectrum bins: {err}"),
            recoverable: false,
        })
    }
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use std::mem::{align_of, offset_of, size_of};

    use super::*;

    #[test]
    fn ddsf_structs_match_frozen_byte_sizes_and_offsets() {
        assert_eq!(size_of::<DdsfFrameHeader>(), DDSF_HEADER_BYTES);
        assert_eq!(align_of::<DdsfFrameHeader>(), 8);
        assert_eq!(offset_of!(DdsfFrameHeader, magic), 0x00);
        assert_eq!(offset_of!(DdsfFrameHeader, version), 0x04);
        assert_eq!(offset_of!(DdsfFrameHeader, flags), 0x06);
        assert_eq!(offset_of!(DdsfFrameHeader, frame_seq), 0x08);
        assert_eq!(offset_of!(DdsfFrameHeader, scope_count), 0x0C);
        assert_eq!(offset_of!(DdsfFrameHeader, snapshot_count), 0x0E);
        assert_eq!(offset_of!(DdsfFrameHeader, host_now_ns), 0x10);
        assert_eq!(offset_of!(DdsfFrameHeader, total_byte_len), 0x18);
        assert_eq!(offset_of!(DdsfFrameHeader, reserved_padding), 0x20);

        assert_eq!(size_of::<DdsfScopePacketHeader>(), DDSF_SCOPE_HEADER_BYTES);
        assert_eq!(offset_of!(DdsfScopePacketHeader, scope_id), 0x00);
        assert_eq!(offset_of!(DdsfScopePacketHeader, trigger_state), 0x04);
        assert_eq!(offset_of!(DdsfScopePacketHeader, mode), 0x05);
        assert_eq!(offset_of!(DdsfScopePacketHeader, track_block_count), 0x06);
        assert_eq!(offset_of!(DdsfScopePacketHeader, win_t_start_ns), 0x08);
        assert_eq!(offset_of!(DdsfScopePacketHeader, win_t_span_ns), 0x10);
        assert_eq!(offset_of!(DdsfScopePacketHeader, subpixel_offset_ns), 0x18);
        assert_eq!(offset_of!(DdsfScopePacketHeader, packet_byte_len), 0x1C);

        assert_eq!(size_of::<DdsfTrackBlockHeader>(), DDSF_TRACK_HEADER_BYTES);
        assert_eq!(offset_of!(DdsfTrackBlockHeader, channel_id), 0x00);
        assert_eq!(offset_of!(DdsfTrackBlockHeader, block_kind), 0x04);
        assert_eq!(offset_of!(DdsfTrackBlockHeader, digital_init_level), 0x05);
        assert_eq!(offset_of!(DdsfTrackBlockHeader, track_flags), 0x06);
        assert_eq!(offset_of!(DdsfTrackBlockHeader, element_count), 0x08);
        assert_eq!(offset_of!(DdsfTrackBlockHeader, data_byte_len), 0x0C);
        assert_eq!(offset_of!(DdsfTrackBlockHeader, block_byte_len), 0x10);
        assert_eq!(offset_of!(DdsfTrackBlockHeader, aux_f32_0), 0x14);
        assert_eq!(offset_of!(DdsfTrackBlockHeader, aux_f32_1), 0x18);
        assert_eq!(offset_of!(DdsfTrackBlockHeader, reserved), 0x1C);

        assert_eq!(size_of::<DdsfTrackStats>(), DDSF_TRACK_STATS_BYTES);
        assert_eq!(size_of::<DdsfAnalogPoint>(), 8);
        assert_eq!(size_of::<DdsfM4Column>(), 20);
        assert_eq!(size_of::<DdsfStateRibbon>(), 16);

        assert_eq!(size_of::<DdsfSnapshotEntry>(), DDSF_SNAPSHOT_ENTRY_BYTES);
        assert_eq!(offset_of!(DdsfSnapshotEntry, channel_num_id), 0x00);
        assert_eq!(offset_of!(DdsfSnapshotEntry, kind), 0x04);
        assert_eq!(offset_of!(DdsfSnapshotEntry, pad), 0x05);
        assert_eq!(offset_of!(DdsfSnapshotEntry, val_f64), 0x08);
    }

    #[test]
    fn ddsf_header_roundtrip_and_validation_work() {
        assert_eq!(DDSF_MAGIC.to_le_bytes(), *b"DDSF");

        let header = DdsfFrameHeader::new_v1(7, 2, 4, 1_234_567_890, 128, DDSF_FLAG_PINNED_ACTIVE);
        assert!(header.validate(128).is_ok());
        assert!(header.validate(64).is_err());

        let raw = bytes_of(&header);
        assert_eq!(&raw[0..4], b"DDSF");
        let decoded: &DdsfFrameHeader = from_bytes(raw);
        assert_eq!(*decoded, header);

        let mut bad = header;
        bad.magic = 0x1234_5678;
        assert!(bad.validate(128).is_err());
    }

    #[test]
    fn track_header_offsets_and_alignment_helpers_are_8_byte_aligned() {
        assert_eq!(align_up_8(0), 0);
        assert_eq!(align_up_8(1), 8);
        assert_eq!(align_up_8(20), 24);
        assert_eq!(align_up_64(65), 128);

        let mut tb = DdsfTrackBlockHeader {
            channel_id: 1,
            block_kind: DdsfBlockKind::DigitalEdges.as_u8(),
            digital_init_level: 1,
            track_flags: 0,
            element_count: 3,
            data_byte_len: 12,
            block_byte_len: 32 + 16,
            aux_f32_0: 0.0,
            aux_f32_1: 0.0,
            reserved: 0,
        };
        assert_eq!(tb.stats_offset(), None);
        assert_eq!(tb.payload_offset(), 32);

        tb.track_flags |= DDSF_TRACK_FLAG_HAS_STATS;
        assert_eq!(tb.stats_offset(), Some(32));
        assert_eq!(tb.payload_offset(), 64);
    }

    #[test]
    fn test_pack_empty_frame() {
        let mut packer = DdsfFramePacker::new(4096);
        let buf = packer.pack_frame(1, 1000, 0, &[], &[]).expect("pack failed");
        assert_eq!(buf.len(), DDSF_HEADER_BYTES);

        let reader = DdsfZeroCopyReader::new(buf).expect("reader failed");
        let hdr = reader.header();
        assert_eq!(hdr.frame_seq, 1);
        assert_eq!(hdr.scope_count, 0);
        assert_eq!(hdr.snapshot_count, 0);
        assert_eq!(hdr.total_byte_len, DDSF_HEADER_BYTES as u64);
        assert_eq!(reader.scopes().count(), 0);
        assert_eq!(reader.snapshots().unwrap().len(), 0);
    }

    #[test]
    fn test_pack_single_scope_individual_tracks() {
        let mut packer = DdsfFramePacker::new(4096);

        // 1. AnalogRawXY
        let pts = vec![
            DdsfAnalogPoint { x_norm: 0.0, y_val: 1.0 },
            DdsfAnalogPoint { x_norm: 0.5, y_val: 2.0 },
        ];
        let scope_raw = ScopeViewportPayload {
            scope_id: 1,
            trigger_state: 3,
            mode: 2,
            win_t_start_ns: 0,
            win_t_span_ns: 10_000,
            subpixel_offset_ns: 0.0,
            tracks: vec![TrackPayload::new_raw_xy(10, pts.clone())],
        };
        let buf = packer.pack_frame(1, 100, 0, &[scope_raw], &[]).unwrap();
        let reader = DdsfZeroCopyReader::new(buf).unwrap();
        let mut scopes = reader.scopes();
        let sc = scopes.next().unwrap();
        assert_eq!(sc.header().scope_id, 1);
        let mut tracks = sc.tracks();
        let tr = tracks.next().unwrap();
        assert_eq!(tr.header().channel_id, 10);
        assert_eq!(tr.raw_points().unwrap(), pts.as_slice());

        // 2. AnalogM4Column with odd count (P=3 -> 60 bytes, 4 bytes padding)
        let cols = vec![
            DdsfM4Column { x_norm: 0.1, v_first: 1.0, v_min: 0.0, v_max: 2.0, v_last: 1.5 },
            DdsfM4Column { x_norm: 0.2, v_first: 1.5, v_min: 1.0, v_max: 3.0, v_last: 2.5 },
            DdsfM4Column { x_norm: 0.3, v_first: 2.5, v_min: 0.5, v_max: 4.0, v_last: 3.0 },
        ];
        let scope_m4 = ScopeViewportPayload {
            scope_id: 2,
            trigger_state: 0,
            mode: 0,
            win_t_start_ns: 0,
            win_t_span_ns: 50_000,
            subpixel_offset_ns: 0.0,
            tracks: vec![TrackPayload::new_m4(20, cols.clone(), None)],
        };
        let buf = packer.pack_frame(2, 200, 0, &[scope_m4], &[]).unwrap();
        let reader = DdsfZeroCopyReader::new(buf).unwrap();
        let sc = reader.scopes().next().unwrap();
        let tr = sc.tracks().next().unwrap();
        assert_eq!(tr.header().data_byte_len, 60);
        assert_eq!(tr.header().block_byte_len, 32 + 64); // 32 hdr + 64 aligned
        assert_eq!(tr.m4_columns().unwrap(), cols.as_slice());

        // 3. DigitalEdges with odd count (E=3 -> 12 bytes, 4 bytes padding)
        let edges = vec![0.1f32, 0.4, 0.9];
        let scope_dig = ScopeViewportPayload {
            scope_id: 3,
            trigger_state: 1,
            mode: 1,
            win_t_start_ns: 0,
            win_t_span_ns: 1000,
            subpixel_offset_ns: 0.0,
            tracks: vec![TrackPayload::new_digital_edges(30, 1, edges.clone())],
        };
        let buf = packer.pack_frame(3, 300, 0, &[scope_dig], &[]).unwrap();
        let reader = DdsfZeroCopyReader::new(buf).unwrap();
        let sc = reader.scopes().next().unwrap();
        let tr = sc.tracks().next().unwrap();
        assert_eq!(tr.header().digital_init_level, 1);
        assert_eq!(tr.header().data_byte_len, 12);
        assert_eq!(tr.header().block_byte_len, 32 + 16);
        assert_eq!(tr.digital_edges().unwrap(), edges.as_slice());

        // 4. StateRibbons
        let ribbons = vec![
            DdsfStateRibbon { x0_norm: 0.0, x1_norm: 0.5, state_id: 1, color_idx: 10 },
            DdsfStateRibbon { x0_norm: 0.5, x1_norm: 1.0, state_id: 2, color_idx: 20 },
        ];
        let scope_rib = ScopeViewportPayload {
            scope_id: 4,
            trigger_state: 4,
            mode: 3,
            win_t_start_ns: 0,
            win_t_span_ns: 1000,
            subpixel_offset_ns: 0.0,
            tracks: vec![TrackPayload::new_state_ribbons(40, ribbons.clone())],
        };
        let buf = packer.pack_frame(4, 400, 0, &[scope_rib], &[]).unwrap();
        let reader = DdsfZeroCopyReader::new(buf).unwrap();
        let sc = reader.scopes().next().unwrap();
        let tr = sc.tracks().next().unwrap();
        assert_eq!(tr.state_ribbons().unwrap(), ribbons.as_slice());

        // 5. SpectrumFrame
        let bins = vec![1.0f32, 2.0, 3.0, 4.0, 5.0];
        let scope_spec = ScopeViewportPayload {
            scope_id: 5,
            trigger_state: 0,
            mode: 0,
            win_t_start_ns: 0,
            win_t_span_ns: 1000,
            subpixel_offset_ns: 0.0,
            tracks: vec![TrackPayload::new_spectrum(50, 100.0, 10.0, bins.clone())],
        };
        let buf = packer.pack_frame(5, 500, 0, &[scope_spec], &[]).unwrap();
        let reader = DdsfZeroCopyReader::new(buf).unwrap();
        let sc = reader.scopes().next().unwrap();
        let tr = sc.tracks().next().unwrap();
        assert_eq!(tr.header().aux_f32_0, 100.0);
        assert_eq!(tr.header().aux_f32_1, 10.0);
        assert_eq!(tr.spectrum_bins().unwrap(), bins.as_slice());
    }

    #[test]
    fn test_stats_block_alignment_and_flag() {
        let stats = DdsfTrackStats {
            min: -1.0,
            max: 5.0,
            vpp: 6.0,
            mean: 2.0,
            rms: 2.5,
            freq_hz: 50.0,
            duty: 0.5,
            reserved: 0.0,
        };
        let cols = vec![DdsfM4Column {
            x_norm: 0.0,
            v_first: 1.0,
            v_min: -1.0,
            v_max: 5.0,
            v_last: 2.0,
        }];

        let scope = ScopeViewportPayload {
            scope_id: 1,
            trigger_state: 3,
            mode: 2,
            win_t_start_ns: 0,
            win_t_span_ns: 10_000,
            subpixel_offset_ns: 0.0,
            tracks: vec![
                TrackPayload::new_m4(1, cols.clone(), Some(stats)),
                TrackPayload::new_m4(2, cols.clone(), None),
            ],
        };

        let mut packer = DdsfFramePacker::new(4096);
        let buf = packer.pack_frame(1, 100, 0, &[scope], &[]).unwrap();
        let reader = DdsfZeroCopyReader::new(buf).unwrap();
        let sc = reader.scopes().next().unwrap();
        let mut tracks = sc.tracks();

        // Track 1 has stats
        let tr1 = tracks.next().unwrap();
        assert_eq!(tr1.header().track_flags & DDSF_TRACK_FLAG_HAS_STATS, DDSF_TRACK_FLAG_HAS_STATS);
        assert_eq!(tr1.stats(), Some(&stats));
        assert_eq!(tr1.m4_columns().unwrap(), cols.as_slice());

        // Track 2 does not have stats
        let tr2 = tracks.next().unwrap();
        assert_eq!(tr2.header().track_flags & DDSF_TRACK_FLAG_HAS_STATS, 0);
        assert_eq!(tr2.stats(), None);
        assert_eq!(tr2.m4_columns().unwrap(), cols.as_slice());
    }

    #[test]
    fn test_8_byte_alignment_on_odd_counts() {
        let mut packer = DdsfFramePacker::new(8192);

        // Test odd M4 column counts (1, 3, 5) and odd digital edge counts (1, 3, 5)
        for p in [1, 2, 3, 5] {
            let cols = vec![DdsfM4Column::default(); p];
            let edges = vec![0.5f32; p];
            let scope = ScopeViewportPayload {
                scope_id: 1,
                trigger_state: 0,
                mode: 0,
                win_t_start_ns: 0,
                win_t_span_ns: 1000,
                subpixel_offset_ns: 0.0,
                tracks: vec![
                    TrackPayload::new_m4(1, cols, None),
                    TrackPayload::new_digital_edges(2, 0, edges),
                ],
            };
            let buf = packer.pack_frame(1, 100, 0, &[scope], &[]).unwrap();
            let reader = DdsfZeroCopyReader::new(buf).unwrap();
            assert_eq!(buf.as_ptr() as usize % 64, 0);

            let sc = reader.scopes().next().unwrap();
            for tr in sc.tracks() {
                let block_len = tr.header().block_byte_len as usize;
                assert_eq!(block_len % 8, 0, "block_byte_len must be multiple of 8");
                let p_offset = tr.header().payload_offset();
                assert_eq!(p_offset % 8, 0, "payload_offset must be multiple of 8");
            }
        }
    }

    #[test]
    fn test_multi_scope_mixed_signals() {
        let pts = vec![DdsfAnalogPoint { x_norm: 0.1, y_val: 1.0 }; 20];
        let cols = vec![DdsfM4Column::default(); 10];
        let edges = vec![0.2f32, 0.4, 0.6];
        let ribbons = vec![DdsfStateRibbon { x0_norm: 0.0, x1_norm: 1.0, state_id: 1, color_idx: 0 }; 5];

        let scopes = vec![
            ScopeViewportPayload {
                scope_id: 1,
                trigger_state: 3,
                mode: 2,
                win_t_start_ns: 1_000,
                win_t_span_ns: 10_000,
                subpixel_offset_ns: 5.0,
                tracks: vec![
                    TrackPayload::new_raw_xy(1, pts.clone()),
                    TrackPayload::new_m4(2, cols.clone(), None),
                ],
            },
            ScopeViewportPayload {
                scope_id: 2,
                trigger_state: 1,
                mode: 1,
                win_t_start_ns: 2_000,
                win_t_span_ns: 20_000,
                subpixel_offset_ns: 0.0,
                tracks: vec![
                    TrackPayload::new_digital_edges(3, 1, edges.clone()),
                    TrackPayload::new_state_ribbons(4, ribbons.clone()),
                ],
            },
            ScopeViewportPayload {
                scope_id: 3,
                trigger_state: 0,
                mode: 0,
                win_t_start_ns: 3_000,
                win_t_span_ns: 30_000,
                subpixel_offset_ns: 0.0,
                tracks: Vec::new(),
            },
        ];

        let snapshots: Vec<DdsfSnapshotEntry> = (0..8)
            .map(|i| DdsfSnapshotEntry {
                channel_num_id: i,
                kind: (i % 3) as u8,
                pad: [0; 3],
                val_f64: (i as f64) * 2.5,
            })
            .collect();

        let mut packer = DdsfFramePacker::new(16384);
        let buf = packer.pack_frame(42, 999_999, DDSF_FLAG_PINNED_ACTIVE, &scopes, &snapshots).unwrap();
        let reader = DdsfZeroCopyReader::new(buf).unwrap();

        assert_eq!(reader.header().frame_seq, 42);
        assert_eq!(reader.header().scope_count, 3);
        assert_eq!(reader.header().snapshot_count, 8);
        assert_eq!(reader.header().flags, DDSF_FLAG_PINNED_ACTIVE);

        let parsed_scopes: Vec<_> = reader.scopes().collect();
        assert_eq!(parsed_scopes.len(), 3);
        assert_eq!(parsed_scopes[0].header().scope_id, 1);
        assert_eq!(parsed_scopes[1].header().scope_id, 2);
        assert_eq!(parsed_scopes[2].header().scope_id, 3);

        let s1_tracks: Vec<_> = parsed_scopes[0].tracks().collect();
        assert_eq!(s1_tracks.len(), 2);
        assert_eq!(s1_tracks[0].raw_points().unwrap(), pts.as_slice());
        assert_eq!(s1_tracks[1].m4_columns().unwrap(), cols.as_slice());

        let s2_tracks: Vec<_> = parsed_scopes[1].tracks().collect();
        assert_eq!(s2_tracks.len(), 2);
        assert_eq!(s2_tracks[0].digital_edges().unwrap(), edges.as_slice());
        assert_eq!(s2_tracks[1].state_ribbons().unwrap(), ribbons.as_slice());

        let s3_tracks: Vec<_> = parsed_scopes[2].tracks().collect();
        assert_eq!(s3_tracks.len(), 0);

        let parsed_snaps = reader.snapshots().unwrap();
        assert_eq!(parsed_snaps, snapshots.as_slice());
    }

    #[test]
    fn test_buffer_growth_and_steady_state_pointer_invariance() {
        let mut packer = DdsfFramePacker::new(64);
        let initial_cap = packer.capacity();

        let pts = vec![DdsfAnalogPoint { x_norm: 0.1, y_val: 1.0 }; 500]; // 4000 bytes
        let scope = ScopeViewportPayload {
            scope_id: 1,
            trigger_state: 0,
            mode: 0,
            win_t_start_ns: 0,
            win_t_span_ns: 1000,
            subpixel_offset_ns: 0.0,
            tracks: vec![TrackPayload::new_raw_xy(1, pts)],
        };

        // Buffer expands to accommodate large frame
        let buf = packer.pack_frame(1, 100, 0, std::slice::from_ref(&scope), &[]).unwrap();
        let buf_len = buf.len();
        assert!(packer.capacity() > initial_cap);
        assert!(buf_len > 4000);

        let ptr_warm = packer.buffer().as_ptr();
        let cap_warm = packer.capacity();

        // 1,000 steady-state iterations without buffer relocation
        for seq in 2..=1000 {
            let _ = packer.pack_frame(seq, seq as u64, 0, std::slice::from_ref(&scope), &[]).unwrap();
            assert_eq!(packer.buffer().as_ptr(), ptr_warm, "buffer pointer moved at seq {seq}!");
            assert_eq!(packer.capacity(), cap_warm, "buffer capacity changed at seq {seq}!");
        }
    }

    #[test]
    fn test_borrowed_views_packing_bit_exact() {
        let pts = vec![DdsfAnalogPoint { x_norm: 0.25, y_val: 3.5 }; 50];
        let m4 = vec![DdsfM4Column { x_norm: 0.5, v_first: 1.0, v_min: -1.0, v_max: 2.0, v_last: 0.5 }; 25];
        let edges = vec![0.1f32, 0.5, 0.9];
        let ribbons = vec![DdsfStateRibbon { x0_norm: 0.0, x1_norm: 1.0, state_id: 2, color_idx: 1 }; 5];
        let stats = DdsfTrackStats { min: -1.0, max: 2.0, vpp: 3.0, mean: 0.5, rms: 0.8, freq_hz: 100.0, duty: 0.5, reserved: 0.0 };

        let owned_scope = ScopeViewportPayload {
            scope_id: 1,
            trigger_state: 3,
            mode: 2,
            win_t_start_ns: 1_000,
            win_t_span_ns: 10_000,
            subpixel_offset_ns: 2.5,
            tracks: vec![
                TrackPayload::new_raw_xy(1, pts.clone()),
                TrackPayload::new_m4(2, m4.clone(), Some(stats)),
                TrackPayload::new_digital_edges(3, 1, edges.clone()),
                TrackPayload::new_state_ribbons(4, ribbons.clone()),
            ],
        };

        let snapshots = vec![
            DdsfSnapshotEntry { channel_num_id: 1, kind: 0, pad: [0; 3], val_f64: std::f64::consts::PI },
            DdsfSnapshotEntry { channel_num_id: 2, kind: 1, pad: [0; 3], val_f64: 1.0 },
        ];

        let mut packer_owned = DdsfFramePacker::new(16384);
        let owned_bytes = packer_owned.pack_frame(1, 100, DDSF_FLAG_QUOTA_WARNING, &[owned_scope], &snapshots).unwrap().to_vec();

        // Construct borrowed view
        let track_views = vec![
            TrackBlockView {
                channel_id: 1,
                block_kind: DdsfBlockKind::AnalogRawXY,
                digital_init_level: 0,
                flags: 0,
                stats: None,
                payload: TrackPayloadSlice::AnalogRawXY(&pts),
                aux_f32_0: 0.0,
                aux_f32_1: 0.0,
            },
            TrackBlockView {
                channel_id: 2,
                block_kind: DdsfBlockKind::AnalogM4Column,
                digital_init_level: 0,
                flags: DDSF_TRACK_FLAG_HAS_STATS,
                stats: Some(&stats),
                payload: TrackPayloadSlice::AnalogM4Column(&m4),
                aux_f32_0: 0.0,
                aux_f32_1: 0.0,
            },
            TrackBlockView {
                channel_id: 3,
                block_kind: DdsfBlockKind::DigitalEdges,
                digital_init_level: 1,
                flags: 0,
                stats: None,
                payload: TrackPayloadSlice::DigitalEdges(&edges),
                aux_f32_0: 0.0,
                aux_f32_1: 0.0,
            },
            TrackBlockView {
                channel_id: 4,
                block_kind: DdsfBlockKind::StateRibbons,
                digital_init_level: 0,
                flags: 0,
                stats: None,
                payload: TrackPayloadSlice::StateRibbons(&ribbons),
                aux_f32_0: 0.0,
                aux_f32_1: 0.0,
            },
        ];

        let scope_view = ScopePacketView {
            scope_id: 1,
            trigger_state: 3,
            mode: 2,
            win_t_start_ns: 1_000,
            win_t_span_ns: 10_000,
            subpixel_offset_ns: 2.5,
            tracks: &track_views,
        };

        let mut packer_borrowed = DdsfFramePacker::new(16384);
        let borrowed_bytes = packer_borrowed.pack_frame_borrowed(1, 100, DDSF_FLAG_QUOTA_WARNING, &[scope_view], &snapshots).unwrap();

        assert_eq!(owned_bytes.as_slice(), borrowed_bytes);
    }

    #[test]
    fn test_zero_copy_reader_error_cases() {
        // Buffer smaller than header
        assert!(DdsfZeroCopyReader::new(&[0u8; 63]).is_err());

        // Corrupted magic
        let mut packer = DdsfFramePacker::new(4096);
        let buf = packer.pack_frame(1, 100, 0, &[], &[]).unwrap();
        let mut corrupted = buf.to_vec();
        corrupted[0] = 0xFF;
        assert!(DdsfZeroCopyReader::new(&corrupted).is_err());

        // Corrupted version
        let mut corrupted_ver = buf.to_vec();
        corrupted_ver[4] = 99;
        assert!(DdsfZeroCopyReader::new(&corrupted_ver).is_err());

        // Truncated buffer
        assert!(DdsfZeroCopyReader::new(&buf[..buf.len() - 1]).is_err());
    }
}
