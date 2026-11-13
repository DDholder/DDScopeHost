//! Frozen `DDSF v1` (DDScope Frame v1) zero-copy little-endian binary wire specification.
//!
//! All headers and records are naturally aligned (8/16/32/64 bytes) so the frontend can
//! construct `DataView` and `Float32Array` slices directly over shared or IPC buffers.

use bytemuck::{Pod, Zeroable};

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

        let raw = bytemuck::bytes_of(&header);
        assert_eq!(&raw[0..4], b"DDSF");
        let decoded: &DdsfFrameHeader = bytemuck::from_bytes(raw);
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
}
