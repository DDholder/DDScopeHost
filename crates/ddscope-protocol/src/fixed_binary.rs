//! Fixed-schema binary frame decoder and `SimPreset` wire-contract codec (`FR-PR-01`).

use bytes::BytesMut;
use memchr::memmem;
use serde::{Deserialize, Serialize};

use crate::downlink::encode_downlink_default;
use crate::{DownlinkPayload, ProtocolCodec};
use ddscope_types::{
    BitfieldSpec, ChannelDescriptor, ChannelKind, CoreError, CoreResult, DecodedBatch,
    DecodedColumnBuf, Endianness, LinearScale, ScalarType, StateEnumMap, StateLabel, TimestampNs,
};

/// Canonical 2-byte header magic for the `SimPreset` wire contract (`[0xDD, 0x53]`).
pub const SIM_PRESET_MAGIC: [u8; 2] = [0xDD, 0x53];
/// Canonical 2-byte tail magic for the `SimPreset` wire contract (`[0x55, 0xAA]`).
pub const SIM_PRESET_TAIL: [u8; 2] = [0x55, 0xAA];

fn default_fixed_source_id() -> String {
    "fixed_binary".to_string()
}

fn default_sim_source_id() -> String {
    "sim0".to_string()
}

const fn default_sim_analog_channels() -> u8 {
    4
}

fn default_max_carry_bytes() -> usize {
    64 * 1024
}

/// Optional frame integrity checksum algorithm for [`FixedSchemaBinaryCodec`].
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChecksumKind {
    #[default]
    None,
    Sum8,
    Xor8,
    Crc16Ccitt,
    Crc32,
}

impl ChecksumKind {
    /// Returns the wire byte width of this checksum.
    pub const fn byte_len(self) -> usize {
        match self {
            Self::None => 0,
            Self::Sum8 | Self::Xor8 => 1,
            Self::Crc16Ccitt => 2,
            Self::Crc32 => 4,
        }
    }
}

/// Byte range covered by a [`FixedSchemaBinaryCodec`] checksum.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChecksumScope {
    #[default]
    PayloadOnly,
    HeaderAndPayload,
}

/// Computes the checksum over `data` using `kind`.
///
/// For `Crc16Ccitt`, uses CRC-16/CCITT-FALSE (`poly = 0x1021`, `init = 0xFFFF`).
/// For `Crc32`, uses IEEE 802.3 CRC-32 via `crc32fast`.
#[inline]
pub fn compute_checksum(kind: ChecksumKind, data: &[u8]) -> u32 {
    match kind {
        ChecksumKind::None => 0,
        ChecksumKind::Sum8 => {
            let mut sum = 0u8;
            for &b in data {
                sum = sum.wrapping_add(b);
            }
            u32::from(sum)
        }
        ChecksumKind::Xor8 => {
            let mut acc = 0u8;
            for &b in data {
                acc ^= b;
            }
            u32::from(acc)
        }
        ChecksumKind::Crc16Ccitt => {
            let mut crc: u16 = 0xFFFF;
            for &b in data {
                crc ^= u16::from(b) << 8;
                for _ in 0..8 {
                    if (crc & 0x8000) != 0 {
                        crc = (crc << 1) ^ 0x1021;
                    } else {
                        crc <<= 1;
                    }
                }
            }
            u32::from(crc)
        }
        ChecksumKind::Crc32 => crc32fast::hash(data),
    }
}

/// Field specification within a [`FixedSchemaBinaryConfig`] payload.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FixedFieldSpec {
    pub name: String,
    pub scalar_type: ScalarType,
    #[serde(default)]
    pub endian: Endianness,
    #[serde(default)]
    pub scale: LinearScale,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub bitfield: Option<BitfieldSpec>,
    /// Optional explicit wire carrier scalar type when `bitfield` is set (defaults to `U8`/`U16`/`U32`/`U64`).
    #[serde(default)]
    pub carrier_type: Option<ScalarType>,
    /// Optional explicit byte offset inside the frame payload (allows multiple bitfields to share a word).
    #[serde(default)]
    pub offset_bytes: Option<usize>,
    #[serde(default)]
    pub is_state_enum: bool,
    #[serde(default)]
    pub state_labels: StateEnumMap,
}

impl FixedFieldSpec {
    /// Creates a sequential scalar field with identity calibration and little-endian byte order.
    pub fn scalar(name: impl Into<String>, scalar_type: ScalarType) -> Self {
        Self {
            name: name.into(),
            scalar_type,
            endian: Endianness::Little,
            scale: LinearScale::identity(),
            unit: None,
            bitfield: None,
            carrier_type: None,
            offset_bytes: None,
            is_state_enum: false,
            state_labels: StateEnumMap::new(),
        }
    }

    /// Creates a bitfield-extracted field at an optional payload byte offset.
    pub fn bitfield(
        name: impl Into<String>,
        scalar_type: ScalarType,
        bitfield: BitfieldSpec,
        offset_bytes: Option<usize>,
    ) -> Self {
        Self {
            name: name.into(),
            scalar_type,
            endian: Endianness::Little,
            scale: LinearScale::identity(),
            unit: None,
            bitfield: Some(bitfield),
            carrier_type: None,
            offset_bytes,
            is_state_enum: false,
            state_labels: StateEnumMap::new(),
        }
    }

    /// Creates a state-machine enum field backed by an integer wire scalar.
    pub fn state_enum(
        name: impl Into<String>,
        scalar_type: ScalarType,
        labels: StateEnumMap,
    ) -> Self {
        Self {
            name: name.into(),
            scalar_type,
            endian: Endianness::Little,
            scale: LinearScale::identity(),
            unit: None,
            bitfield: None,
            carrier_type: None,
            offset_bytes: None,
            is_state_enum: true,
            state_labels: labels,
        }
    }

    /// Sets byte order.
    pub fn with_endian(mut self, endian: Endianness) -> Self {
        self.endian = endian;
        self
    }

    /// Sets linear physical calibration attached to the channel descriptor.
    pub fn with_scale(mut self, scale: LinearScale) -> Self {
        self.scale = scale;
        self
    }

    /// Sets channel unit metadata.
    pub fn with_unit(mut self, unit: impl Into<String>) -> Self {
        self.unit = Some(unit.into());
        self
    }

    /// Sets an explicit byte offset inside the payload.
    pub fn with_offset(mut self, offset_bytes: usize) -> Self {
        self.offset_bytes = Some(offset_bytes);
        self
    }

    /// Sets an explicit carrier type for bitfield extraction.
    pub fn with_carrier_type(mut self, carrier_type: ScalarType) -> Self {
        self.carrier_type = Some(carrier_type);
        self
    }
}

/// Configuration for [`FixedSchemaBinaryCodec`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FixedSchemaBinaryConfig {
    #[serde(default = "default_fixed_source_id")]
    pub source_id: String,
    #[serde(default)]
    pub header_magic: Vec<u8>,
    #[serde(default)]
    pub tail_magic: Option<Vec<u8>>,
    #[serde(default)]
    pub checksum: ChecksumKind,
    #[serde(default)]
    pub checksum_scope: ChecksumScope,
    #[serde(default)]
    pub checksum_endian: Endianness,
    #[serde(default)]
    pub has_hw_tick_first_col: bool,
    pub fields: Vec<FixedFieldSpec>,
    #[serde(default = "default_max_carry_bytes")]
    pub max_carry_bytes: usize,
}

/// Statistics tracked by binary frame codecs.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct BinaryCodecStats {
    pub decoded_frames: u64,
    pub checksum_failures: u64,
    pub resync_discarded_bytes: u64,
}

#[derive(Copy, Clone, Debug)]
struct CompiledField {
    offset: usize,
    wire_type: ScalarType,
    target_type: ScalarType,
    endian: Endianness,
    bitfield: Option<BitfieldSpec>,
    is_state_enum: bool,
    is_hw_tick: bool,
    col_idx: usize,
}

/// Configurable fixed-layout binary protocol codec (`header_magic + payload + checksum + tail_magic`).
pub struct FixedSchemaBinaryCodec {
    header_magic: Vec<u8>,
    tail_magic: Vec<u8>,
    checksum: ChecksumKind,
    checksum_scope: ChecksumScope,
    checksum_endian: Endianness,
    payload_bytes: usize,
    frame_bytes: usize,
    max_carry_bytes: usize,
    channels: Vec<ChannelDescriptor>,
    compiled_fields: Vec<CompiledField>,
    header_finder: Option<memmem::Finder<'static>>,
    tail_finder: Option<memmem::Finder<'static>>,
    carry: Vec<u8>,
    stats: BinaryCodecStats,
}

impl std::fmt::Debug for FixedSchemaBinaryCodec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FixedSchemaBinaryCodec")
            .field("frame_bytes", &self.frame_bytes)
            .field("channels", &self.channels.len())
            .field("checksum", &self.checksum)
            .field("stats", &self.stats)
            .finish()
    }
}

fn scalar_wire_bytes(st: ScalarType) -> usize {
    match st {
        ScalarType::Bool => 1,
        _ => st.bit_width() / 8,
    }
}

fn is_integer_scalar(st: ScalarType) -> bool {
    matches!(
        st,
        ScalarType::U8
            | ScalarType::U16
            | ScalarType::U32
            | ScalarType::U64
            | ScalarType::I8
            | ScalarType::I16
            | ScalarType::I32
            | ScalarType::I64
    )
}

fn resolve_bitfield_carrier(
    field: &FixedFieldSpec,
    bf: BitfieldSpec,
) -> CoreResult<ScalarType> {
    if bf.bit_width == 0 || bf.bit_width > 64 {
        return Err(CoreError::InvalidConfig(format!(
            "field `{}` has invalid bitfield width {}",
            field.name, bf.bit_width
        )));
    }
    if field.scalar_type.is_floating_point() {
        return Err(CoreError::InvalidConfig(format!(
            "field `{}` cannot combine floating-point type with bitfield",
            field.name
        )));
    }
    let carrier = if let Some(explicit) = field.carrier_type {
        if !is_integer_scalar(explicit) {
            return Err(CoreError::InvalidConfig(format!(
                "field `{}` bitfield carrier_type must be an integer type",
                field.name
            )));
        }
        explicit
    } else if is_integer_scalar(field.scalar_type) {
        field.scalar_type
    } else {
        let total_bits = usize::from(bf.bit_offset) + usize::from(bf.bit_width);
        if total_bits <= 8 {
            ScalarType::U8
        } else if total_bits <= 16 {
            ScalarType::U16
        } else if total_bits <= 32 {
            ScalarType::U32
        } else {
            ScalarType::U64
        }
    };

    let carrier_bits = carrier.bit_width();
    if usize::from(bf.bit_offset) + usize::from(bf.bit_width) > carrier_bits {
        return Err(CoreError::InvalidConfig(format!(
            "field `{}` bitfield [{}..{}] exceeds carrier width {} bits",
            field.name,
            bf.bit_offset,
            usize::from(bf.bit_offset) + usize::from(bf.bit_width),
            carrier_bits
        )));
    }
    Ok(carrier)
}

impl FixedSchemaBinaryCodec {
    /// Creates and validates a [`FixedSchemaBinaryCodec`].
    pub fn new(config: FixedSchemaBinaryConfig) -> CoreResult<Self> {
        if config.fields.is_empty() {
            return Err(CoreError::InvalidConfig(
                "FixedSchemaBinaryCodec requires at least 1 field".into(),
            ));
        }

        let mut compiled_fields = Vec::with_capacity(config.fields.len());
        let mut channels = Vec::with_capacity(config.fields.len());
        let mut cursor = 0usize;
        let mut payload_bytes = 0usize;

        for (idx, field) in config.fields.iter().enumerate() {
            if field.name.is_empty() {
                return Err(CoreError::InvalidConfig(
                    "FixedSchemaBinaryCodec field name must not be empty".into(),
                ));
            }
            if !field.scale.scale.is_finite() || !field.scale.offset.is_finite() {
                return Err(CoreError::InvalidConfig(format!(
                    "field `{}` has non-finite LinearScale",
                    field.name
                )));
            }

            let is_hw_tick = config.has_hw_tick_first_col && idx == 0;
            if is_hw_tick {
                if !is_integer_scalar(field.scalar_type) || field.is_state_enum {
                    return Err(CoreError::InvalidConfig(format!(
                        "hw_tick first field `{}` must be an integer scalar",
                        field.name
                    )));
                }
            } else if field.is_state_enum && !is_integer_scalar(field.scalar_type) {
                return Err(CoreError::InvalidConfig(format!(
                    "state_enum field `{}` must have an integer scalar_type",
                    field.name
                )));
            }

            let wire_type = if let Some(bf) = field.bitfield {
                resolve_bitfield_carrier(field, bf)?
            } else {
                field.scalar_type
            };
            let wire_len = scalar_wire_bytes(wire_type);

            let offset = match field.offset_bytes {
                Some(explicit) => {
                    cursor = cursor.max(explicit + wire_len);
                    explicit
                }
                None => {
                    let off = cursor;
                    cursor += wire_len;
                    off
                }
            };
            payload_bytes = payload_bytes.max(offset + wire_len);

            let col_idx = if is_hw_tick {
                usize::MAX
            } else {
                let c_idx = channels.len();
                let desc = if field.is_state_enum {
                    let mut d = ChannelDescriptor::new_state_machine(
                        config.source_id.as_str(),
                        field.name.clone(),
                        field.state_labels.clone(),
                    );
                    d.unit = field.unit.clone();
                    d
                } else {
                    ChannelDescriptor {
                        id: None,
                        source_id: config.source_id.clone().into(),
                        local_path: field.name.clone(),
                        display_name: None,
                        unit: field.unit.clone(),
                        kind: ChannelKind::Scalar {
                            scalar_type: field.scalar_type,
                            calibration: field.scale,
                        },
                    }
                };
                channels.push(desc);
                c_idx
            };

            compiled_fields.push(CompiledField {
                offset,
                wire_type,
                target_type: field.scalar_type,
                endian: field.endian,
                bitfield: field.bitfield,
                is_state_enum: field.is_state_enum,
                is_hw_tick,
                col_idx,
            });
        }

        let tail_magic = config.tail_magic.unwrap_or_default();
        let checksum_len = config.checksum.byte_len();
        let frame_bytes =
            config.header_magic.len() + payload_bytes + checksum_len + tail_magic.len();
        let max_carry_bytes = config.max_carry_bytes.max(frame_bytes);

        let header_finder = if config.header_magic.is_empty() {
            None
        } else {
            Some(memmem::Finder::new(&config.header_magic).into_owned())
        };
        let tail_finder = if tail_magic.is_empty() {
            None
        } else {
            Some(memmem::Finder::new(&tail_magic).into_owned())
        };

        Ok(Self {
            header_magic: config.header_magic,
            tail_magic,
            checksum: config.checksum,
            checksum_scope: config.checksum_scope,
            checksum_endian: config.checksum_endian,
            payload_bytes,
            frame_bytes,
            max_carry_bytes,
            channels,
            compiled_fields,
            header_finder,
            tail_finder,
            carry: Vec::with_capacity(frame_bytes * 2),
            stats: BinaryCodecStats::default(),
        })
    }

    /// Returns cumulative frame decode and error counters.
    pub fn stats(&self) -> BinaryCodecStats {
        self.stats
    }

    /// Returns the total byte length of a single wire frame.
    pub fn frame_bytes(&self) -> usize {
        self.frame_bytes
    }

    fn ensure_batch_columns(&self, batch_out: &mut DecodedBatch) -> CoreResult<()> {
        if batch_out.columns.is_empty() && !self.channels.is_empty() {
            batch_out.columns = self
                .channels
                .iter()
                .map(|d| DecodedColumnBuf::for_channel_kind(&d.kind, 64))
                .collect();
            return Ok(());
        }
        if batch_out.columns.len() < self.channels.len() {
            return Err(CoreError::Protocol {
                message: format!(
                    "DecodedBatch column count {} is smaller than FixedSchemaBinary channel count {}",
                    batch_out.columns.len(),
                    self.channels.len()
                ),
                recoverable: false,
            });
        }
        Ok(())
    }

    #[inline]
    fn verify_frame(&mut self, frame: &[u8]) -> bool {
        let h_len = self.header_magic.len();
        let t_len = self.tail_magic.len();
        if h_len > 0 && &frame[..h_len] != self.header_magic.as_slice() {
            return false;
        }
        if t_len > 0 && &frame[self.frame_bytes - t_len..] != self.tail_magic.as_slice() {
            return false;
        }
        if self.checksum != ChecksumKind::None {
            let cs_off = h_len + self.payload_bytes;
            let cs_end = cs_off + self.checksum.byte_len();
            let covered = match self.checksum_scope {
                ChecksumScope::PayloadOnly => &frame[h_len..h_len + self.payload_bytes],
                ChecksumScope::HeaderAndPayload => &frame[..h_len + self.payload_bytes],
            };
            let expected = compute_checksum(self.checksum, covered);
            let actual = read_checksum(&frame[cs_off..cs_end], self.checksum, self.checksum_endian);
            if expected != actual {
                self.stats.checksum_failures += 1;
                return false;
            }
        }
        true
    }

    #[inline]
    fn decode_frame_fields(&mut self, frame: &[u8], batch_out: &mut DecodedBatch) {
        let payload = &frame[self.header_magic.len()..self.header_magic.len() + self.payload_bytes];
        for field in &self.compiled_fields {
            decode_compiled_field(field, payload, batch_out);
        }
        self.stats.decoded_frames += 1;
    }

    /// Processes a contiguous buffer `buf` in-place, returning `(consumed_bytes, decoded_rows)`.
    fn process_slice(&mut self, buf: &[u8], batch_out: &mut DecodedBatch) -> (usize, usize) {
        let f_len = self.frame_bytes;
        let h_len = self.header_magic.len();
        let mut pos = 0usize;
        let mut rows = 0usize;

        while buf.len() - pos >= f_len {
            let rem = &buf[pos..];
            if h_len > 0 {
                if rem.starts_with(&self.header_magic) {
                    if self.verify_frame(&rem[..f_len]) {
                        self.decode_frame_fields(&rem[..f_len], batch_out);
                        rows += 1;
                        pos += f_len;
                    } else {
                        self.stats.resync_discarded_bytes += 1;
                        pos += 1;
                    }
                } else if let Some(finder) = &self.header_finder {
                    if let Some(found) = finder.find(rem) {
                        self.stats.resync_discarded_bytes += found as u64;
                        pos += found;
                    } else {
                        let keep = h_len.saturating_sub(1).min(rem.len());
                        let discard = rem.len() - keep;
                        self.stats.resync_discarded_bytes += discard as u64;
                        pos += discard;
                        break;
                    }
                }
            } else if !self.tail_magic.is_empty() {
                let tail_off = self.payload_bytes + self.checksum.byte_len();
                if &rem[tail_off..f_len] == self.tail_magic.as_slice()
                    && self.verify_frame(&rem[..f_len])
                {
                    self.decode_frame_fields(&rem[..f_len], batch_out);
                    rows += 1;
                    pos += f_len;
                } else if let Some(finder) = &self.tail_finder {
                    if let Some(tpos) = finder.find(rem) {
                        if tpos >= tail_off {
                            let start = tpos - tail_off;
                            if start > 0 {
                                self.stats.resync_discarded_bytes += start as u64;
                                pos += start;
                            } else {
                                // Checksum failed at start == 0; advance 1 byte past candidate.
                                self.stats.resync_discarded_bytes += 1;
                                pos += 1;
                            }
                        } else {
                            let skip = tpos + self.tail_magic.len();
                            self.stats.resync_discarded_bytes += skip as u64;
                            pos += skip;
                        }
                    } else {
                        let keep = (f_len - 1).min(rem.len());
                        let discard = rem.len() - keep;
                        self.stats.resync_discarded_bytes += discard as u64;
                        pos += discard;
                        break;
                    }
                }
            } else {
                // No header or tail magic: fixed frame size only.
                if self.verify_frame(&rem[..f_len]) {
                    self.decode_frame_fields(&rem[..f_len], batch_out);
                    rows += 1;
                    pos += f_len;
                } else {
                    self.stats.resync_discarded_bytes += 1;
                    pos += 1;
                }
            }
        }

        (pos, rows)
    }
}

impl ProtocolCodec for FixedSchemaBinaryCodec {
    fn protocol_name(&self) -> &str {
        "FixedSchemaBinary"
    }

    fn declared_channels(&self) -> &[ChannelDescriptor] {
        &self.channels
    }

    fn feed_bytes(
        &mut self,
        chunk: &[u8],
        recv_ns: TimestampNs,
        batch_out: &mut DecodedBatch,
    ) -> CoreResult<usize> {
        self.ensure_batch_columns(batch_out)?;
        batch_out.packet_recv_ns = recv_ns;
        if chunk.is_empty() {
            return Ok(0);
        }

        let est = chunk.len() / self.frame_bytes;
        if est > 0 {
            reserve_batch_columns(batch_out, &self.compiled_fields, est);
        }

        let mut total_rows = 0usize;
        let mut remaining = chunk;

        // Resolve any carried partial bytes without copying the entire incoming chunk.
        while !self.carry.is_empty() {
            if self.carry.len() < self.frame_bytes {
                let need = self.frame_bytes - self.carry.len();
                if remaining.len() < need {
                    self.carry.extend_from_slice(remaining);
                    return Ok(total_rows);
                }
                self.carry.extend_from_slice(&remaining[..need]);
                remaining = &remaining[need..];
            }

            let mut tmp = std::mem::take(&mut self.carry);
            let (consumed, rows) = self.process_slice(&tmp, batch_out);
            total_rows += rows;
            if consumed < tmp.len() {
                tmp.drain(..consumed);
                self.carry = tmp;
                if self.carry.len() >= self.frame_bytes {
                    // Should not stall; loop guards against malformed tail.
                    break;
                }
            } else {
                tmp.clear();
                self.carry = tmp;
            }
        }

        let (consumed, rows) = self.process_slice(remaining, batch_out);
        total_rows += rows;
        let leftover = &remaining[consumed..];
        if !leftover.is_empty() {
            let keep = leftover.len().min(self.max_carry_bytes);
            self.carry
                .extend_from_slice(&leftover[leftover.len() - keep..]);
        }

        batch_out.sample_rows += total_rows;
        Ok(total_rows)
    }

    fn reset_state(&mut self) {
        self.carry.clear();
    }

    fn encode_downlink(&mut self, req: &DownlinkPayload, out: &mut BytesMut) -> CoreResult<()> {
        encode_downlink_default(req, out)
    }
}

fn reserve_batch_columns(batch: &mut DecodedBatch, fields: &[CompiledField], est: usize) {
    for f in fields {
        if f.is_hw_tick {
            batch.hw_ticks.reserve(est);
        } else if let Some(col) = batch.columns.get_mut(f.col_idx) {
            match col {
                DecodedColumnBuf::Bool(v) => v.reserve(est),
                DecodedColumnBuf::U8(v) => v.reserve(est),
                DecodedColumnBuf::U16(v) => v.reserve(est),
                DecodedColumnBuf::U32(v) => v.reserve(est),
                DecodedColumnBuf::U64(v) => v.reserve(est),
                DecodedColumnBuf::I8(v) => v.reserve(est),
                DecodedColumnBuf::I16(v) => v.reserve(est),
                DecodedColumnBuf::I32(v) => v.reserve(est),
                DecodedColumnBuf::I64(v) => v.reserve(est),
                DecodedColumnBuf::F32(v) => v.reserve(est),
                DecodedColumnBuf::F64(v) => v.reserve(est),
                DecodedColumnBuf::StateEnum(v) => v.reserve(est),
            }
        }
    }
}

#[inline]
fn read_checksum(bytes: &[u8], kind: ChecksumKind, endian: Endianness) -> u32 {
    match kind {
        ChecksumKind::None => 0,
        ChecksumKind::Sum8 | ChecksumKind::Xor8 => u32::from(bytes[0]),
        ChecksumKind::Crc16Ccitt => {
            let v = match endian {
                Endianness::Little => u16::from_le_bytes([bytes[0], bytes[1]]),
                Endianness::Big => u16::from_be_bytes([bytes[0], bytes[1]]),
            };
            u32::from(v)
        }
        ChecksumKind::Crc32 => match endian {
            Endianness::Little => u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            Endianness::Big => u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        },
    }
}

#[inline]
fn read_raw_u64(bytes: &[u8], st: ScalarType, endian: Endianness) -> u64 {
    match (st, endian) {
        (ScalarType::Bool | ScalarType::U8, _) => u64::from(bytes[0]),
        (ScalarType::I8, _) => (bytes[0] as i8) as i64 as u64,
        (ScalarType::U16, Endianness::Little) => u64::from(u16::from_le_bytes([bytes[0], bytes[1]])),
        (ScalarType::U16, Endianness::Big) => u64::from(u16::from_be_bytes([bytes[0], bytes[1]])),
        (ScalarType::I16, Endianness::Little) => {
            i64::from(i16::from_le_bytes([bytes[0], bytes[1]])) as u64
        }
        (ScalarType::I16, Endianness::Big) => {
            i64::from(i16::from_be_bytes([bytes[0], bytes[1]])) as u64
        }
        (ScalarType::U32, Endianness::Little) => {
            u64::from(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        }
        (ScalarType::U32, Endianness::Big) => {
            u64::from(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        }
        (ScalarType::I32, Endianness::Little) => {
            i64::from(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])) as u64
        }
        (ScalarType::I32, Endianness::Big) => {
            i64::from(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])) as u64
        }
        (ScalarType::U64 | ScalarType::I64, Endianness::Little) => u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]),
        (ScalarType::U64 | ScalarType::I64, Endianness::Big) => u64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]),
        (ScalarType::F32 | ScalarType::F64, _) => 0,
    }
}

#[inline]
fn extract_bitfield(raw: u64, bf: BitfieldSpec, signed: bool) -> u64 {
    let shifted = raw >> bf.bit_offset;
    let masked = if bf.bit_width >= 64 {
        shifted
    } else {
        shifted & ((1u64 << bf.bit_width) - 1)
    };
    if signed && bf.bit_width < 64 {
        let shift = 64 - u32::from(bf.bit_width);
        (((masked << shift) as i64) >> shift) as u64
    } else {
        masked
    }
}

#[inline]
fn decode_compiled_field(field: &CompiledField, payload: &[u8], batch_out: &mut DecodedBatch) {
    let slice = &payload[field.offset..];
    if field.is_hw_tick {
        let mut raw = read_raw_u64(slice, field.wire_type, field.endian);
        if let Some(bf) = field.bitfield {
            raw = extract_bitfield(raw, bf, false);
        }
        batch_out.hw_ticks.push(raw);
        return;
    }

    if field.is_state_enum {
        let mut raw = read_raw_u64(slice, field.wire_type, field.endian);
        if let Some(bf) = field.bitfield {
            raw = extract_bitfield(raw, bf, field.target_type.is_signed());
        }
        let code = if field.target_type.is_signed() {
            (raw as i64) as i32
        } else {
            raw as i32
        };
        if let DecodedColumnBuf::StateEnum(col) = &mut batch_out.columns[field.col_idx] {
            col.push(code);
        }
        return;
    }

    match field.target_type {
        ScalarType::F32 => {
            let v = match field.endian {
                Endianness::Little => f32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]),
                Endianness::Big => f32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]),
            };
            if let DecodedColumnBuf::F32(col) = &mut batch_out.columns[field.col_idx] {
                col.push(v);
            }
        }
        ScalarType::F64 => {
            let v = match field.endian {
                Endianness::Little => f64::from_le_bytes([
                    slice[0], slice[1], slice[2], slice[3], slice[4], slice[5], slice[6], slice[7],
                ]),
                Endianness::Big => f64::from_be_bytes([
                    slice[0], slice[1], slice[2], slice[3], slice[4], slice[5], slice[6], slice[7],
                ]),
            };
            if let DecodedColumnBuf::F64(col) = &mut batch_out.columns[field.col_idx] {
                col.push(v);
            }
        }
        _ => {
            let mut raw = read_raw_u64(slice, field.wire_type, field.endian);
            if let Some(bf) = field.bitfield {
                raw = extract_bitfield(raw, bf, field.target_type.is_signed());
            }
            match &mut batch_out.columns[field.col_idx] {
                DecodedColumnBuf::Bool(col) => col.push(raw != 0),
                DecodedColumnBuf::U8(col) => col.push(raw as u8),
                DecodedColumnBuf::U16(col) => col.push(raw as u16),
                DecodedColumnBuf::U32(col) => col.push(raw as u32),
                DecodedColumnBuf::U64(col) => col.push(raw),
                DecodedColumnBuf::I8(col) => col.push(raw as i8),
                DecodedColumnBuf::I16(col) => col.push(raw as i16),
                DecodedColumnBuf::I32(col) => col.push(raw as i32),
                DecodedColumnBuf::I64(col) => col.push(raw as i64),
                _ => {}
            }
        }
    }
}

/// Configuration for [`SimPresetCodec`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimPresetConfig {
    #[serde(default = "default_sim_source_id")]
    pub source_id: String,
    #[serde(default = "default_sim_analog_channels")]
    pub analog_channels: u8,
    #[serde(default = "default_max_carry_bytes")]
    pub max_carry_bytes: usize,
}

impl Default for SimPresetConfig {
    fn default() -> Self {
        Self {
            source_id: default_sim_source_id(),
            analog_channels: default_sim_analog_channels(),
            max_carry_bytes: default_max_carry_bytes(),
        }
    }
}

/// Builds the canonical 4-state motor control state map (`0:IDLE`, `1:ALIGN`, `2:CLOSED_LOOP`, `3:OCP_FAULT`).
pub fn sim_ctrl_state_labels() -> StateEnumMap {
    let mut map = StateEnumMap::new();
    map.insert(
        0,
        StateLabel {
            name: "IDLE".to_string(),
            color_rgba: Some(0x6474_8BFF),
        },
    );
    map.insert(
        1,
        StateLabel {
            name: "ALIGN".to_string(),
            color_rgba: Some(0xF59E_0BFF),
        },
    );
    map.insert(
        2,
        StateLabel {
            name: "CLOSED_LOOP".to_string(),
            color_rgba: Some(0x10B9_81FF),
        },
    );
    map.insert(
        3,
        StateLabel {
            name: "OCP_FAULT".to_string(),
            color_rgba: Some(0xEF44_44FF),
        },
    );
    map
}

/// Packs `SimPreset` digital logic and glitch flags into the wire `flags_bits` byte.
#[inline]
pub const fn sim_preset_flags(
    pwm_a: bool,
    pwm_b: bool,
    fault_pin: bool,
    glitch_active: bool,
) -> u8 {
    let mut flags = 0u8;
    if pwm_a {
        flags |= 0x01;
    }
    if pwm_b {
        flags |= 0x02;
    }
    if fault_pin {
        flags |= 0x04;
    }
    if glitch_active {
        flags |= 0x80;
    }
    flags
}

/// Encodes a single `SimPreset` wire frame (`12 + 4 * A` LE bytes) matching `@tb2-sim`'s contract.
pub fn encode_sim_preset_frame(
    hw_tick: u32,
    analogs: &[f32],
    flags_bits: u8,
    ctrl_state: u8,
    out: &mut Vec<u8>,
) {
    let a = analogs.len() as u8;
    out.extend_from_slice(&SIM_PRESET_MAGIC);
    out.push(a);
    out.push(flags_bits);
    out.extend_from_slice(&hw_tick.to_le_bytes());
    for &v in analogs {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.push(ctrl_state);
    out.push(0); // reserved
    out.extend_from_slice(&SIM_PRESET_TAIL);
}

/// Wire-compatible decoder for `@tb2-sim`'s `SimPreset` mixed-signal binary frame (`12 + 4 * A` bytes).
pub struct SimPresetCodec {
    analog_channels: u8,
    frame_bytes: usize,
    max_carry_bytes: usize,
    channels: Vec<ChannelDescriptor>,
    finder: memmem::Finder<'static>,
    carry: Vec<u8>,
    stats: BinaryCodecStats,
}

impl std::fmt::Debug for SimPresetCodec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SimPresetCodec")
            .field("analog_channels", &self.analog_channels)
            .field("frame_bytes", &self.frame_bytes)
            .field("stats", &self.stats)
            .finish()
    }
}

impl SimPresetCodec {
    /// Constructs a [`SimPresetCodec`] validating `analog_channels` in `1..=16`.
    pub fn new(config: SimPresetConfig) -> CoreResult<Self> {
        if !(1..=16).contains(&config.analog_channels) {
            return Err(CoreError::InvalidConfig(format!(
                "SimPresetCodec analog_channels must be in 1..=16, got {}",
                config.analog_channels
            )));
        }
        let a = usize::from(config.analog_channels);
        let frame_bytes = 12 + 4 * a;

        let mut channels = Vec::with_capacity(a + 4);
        for idx in 0..a {
            channels.push(ChannelDescriptor::new_scalar(
                config.source_id.as_str(),
                format!("ch{idx}"),
                ScalarType::F32,
            ));
        }
        channels.push(ChannelDescriptor::new_bool(
            config.source_id.as_str(),
            "pwm_a",
        ));
        channels.push(ChannelDescriptor::new_bool(
            config.source_id.as_str(),
            "pwm_b",
        ));
        channels.push(ChannelDescriptor::new_bool(
            config.source_id.as_str(),
            "fault_pin",
        ));
        channels.push(ChannelDescriptor::new_state_machine(
            config.source_id.as_str(),
            "ctrl_state",
            sim_ctrl_state_labels(),
        ));

        Ok(Self {
            analog_channels: config.analog_channels,
            frame_bytes,
            max_carry_bytes: config.max_carry_bytes.max(frame_bytes),
            channels,
            finder: memmem::Finder::new(&SIM_PRESET_MAGIC),
            carry: Vec::with_capacity(frame_bytes * 2),
            stats: BinaryCodecStats::default(),
        })
    }

    /// Returns cumulative frame decode and resync counters.
    pub fn stats(&self) -> BinaryCodecStats {
        self.stats
    }

    /// Returns the expected wire frame length (`12 + 4 * analog_channels`).
    pub fn frame_bytes(&self) -> usize {
        self.frame_bytes
    }

    fn ensure_batch_columns(&self, batch_out: &mut DecodedBatch) -> CoreResult<()> {
        if batch_out.columns.is_empty() {
            batch_out.columns = self
                .channels
                .iter()
                .map(|d| DecodedColumnBuf::for_channel_kind(&d.kind, 64))
                .collect();
            return Ok(());
        }
        if batch_out.columns.len() < self.channels.len() {
            return Err(CoreError::Protocol {
                message: format!(
                    "DecodedBatch column count {} is smaller than SimPreset channel count {}",
                    batch_out.columns.len(),
                    self.channels.len()
                ),
                recoverable: false,
            });
        }
        Ok(())
    }

    #[inline]
    fn is_valid_frame(&self, frame: &[u8]) -> bool {
        let a = usize::from(self.analog_channels);
        let tail_off = 10 + 4 * a;
        frame[0] == SIM_PRESET_MAGIC[0]
            && frame[1] == SIM_PRESET_MAGIC[1]
            && frame[2] == self.analog_channels
            && frame[tail_off] == SIM_PRESET_TAIL[0]
            && frame[tail_off + 1] == SIM_PRESET_TAIL[1]
    }

    #[inline]
    fn decode_frame(&mut self, frame: &[u8], batch_out: &mut DecodedBatch) {
        let a = usize::from(self.analog_channels);
        let flags = frame[3];
        let hw_tick = u32::from_le_bytes([frame[4], frame[5], frame[6], frame[7]]);
        batch_out.hw_ticks.push(u64::from(hw_tick));

        let analog_base = 8;
        for c in 0..a {
            let off = analog_base + c * 4;
            let v = f32::from_le_bytes([
                frame[off],
                frame[off + 1],
                frame[off + 2],
                frame[off + 3],
            ]);
            if let DecodedColumnBuf::F32(col) = &mut batch_out.columns[c] {
                col.push(v);
            }
        }

        if let DecodedColumnBuf::Bool(col) = &mut batch_out.columns[a] {
            col.push((flags & 0x01) != 0);
        }
        if let DecodedColumnBuf::Bool(col) = &mut batch_out.columns[a + 1] {
            col.push((flags & 0x02) != 0);
        }
        if let DecodedColumnBuf::Bool(col) = &mut batch_out.columns[a + 2] {
            col.push((flags & 0x04) != 0);
        }
        let ctrl_state = i32::from(frame[8 + 4 * a]);
        if let DecodedColumnBuf::StateEnum(col) = &mut batch_out.columns[a + 3] {
            col.push(ctrl_state);
        }

        self.stats.decoded_frames += 1;
    }

    fn process_slice(&mut self, buf: &[u8], batch_out: &mut DecodedBatch) -> (usize, usize) {
        let f_len = self.frame_bytes;
        let mut pos = 0usize;
        let mut rows = 0usize;

        while buf.len() - pos >= f_len {
            let rem = &buf[pos..];
            if rem[0] == SIM_PRESET_MAGIC[0] && rem[1] == SIM_PRESET_MAGIC[1] {
                if self.is_valid_frame(&rem[..f_len]) {
                    self.decode_frame(&rem[..f_len], batch_out);
                    rows += 1;
                    pos += f_len;
                } else {
                    self.stats.resync_discarded_bytes += 1;
                    pos += 1;
                }
            } else if let Some(found) = self.finder.find(rem) {
                self.stats.resync_discarded_bytes += found as u64;
                pos += found;
            } else {
                let keep = if rem.last() == Some(&SIM_PRESET_MAGIC[0]) {
                    1
                } else {
                    0
                };
                let discard = rem.len() - keep;
                self.stats.resync_discarded_bytes += discard as u64;
                pos += discard;
                break;
            }
        }

        (pos, rows)
    }
}

impl ProtocolCodec for SimPresetCodec {
    fn protocol_name(&self) -> &str {
        "SimPreset"
    }

    fn declared_channels(&self) -> &[ChannelDescriptor] {
        &self.channels
    }

    fn feed_bytes(
        &mut self,
        chunk: &[u8],
        recv_ns: TimestampNs,
        batch_out: &mut DecodedBatch,
    ) -> CoreResult<usize> {
        self.ensure_batch_columns(batch_out)?;
        batch_out.packet_recv_ns = recv_ns;
        if chunk.is_empty() {
            return Ok(0);
        }

        let est = chunk.len() / self.frame_bytes;
        if est > 0 {
            batch_out.hw_ticks.reserve(est);
            for col in &mut batch_out.columns {
                match col {
                    DecodedColumnBuf::F32(v) => v.reserve(est),
                    DecodedColumnBuf::Bool(v) => v.reserve(est),
                    DecodedColumnBuf::StateEnum(v) => v.reserve(est),
                    _ => {}
                }
            }
        }

        let mut total_rows = 0usize;
        let mut remaining = chunk;

        while !self.carry.is_empty() {
            if self.carry.len() < self.frame_bytes {
                let need = self.frame_bytes - self.carry.len();
                if remaining.len() < need {
                    self.carry.extend_from_slice(remaining);
                    return Ok(total_rows);
                }
                self.carry.extend_from_slice(&remaining[..need]);
                remaining = &remaining[need..];
            }

            let mut tmp = std::mem::take(&mut self.carry);
            let (consumed, rows) = self.process_slice(&tmp, batch_out);
            total_rows += rows;
            if consumed < tmp.len() {
                tmp.drain(..consumed);
                self.carry = tmp;
                if self.carry.len() >= self.frame_bytes {
                    break;
                }
            } else {
                tmp.clear();
                self.carry = tmp;
            }
        }

        let (consumed, rows) = self.process_slice(remaining, batch_out);
        total_rows += rows;
        let leftover = &remaining[consumed..];
        if !leftover.is_empty() {
            let keep = leftover.len().min(self.max_carry_bytes);
            self.carry
                .extend_from_slice(&leftover[leftover.len() - keep..]);
        }

        batch_out.sample_rows += total_rows;
        Ok(total_rows)
    }

    fn reset_state(&mut self) {
        self.carry.clear();
    }

    fn encode_downlink(&mut self, req: &DownlinkPayload, out: &mut BytesMut) -> CoreResult<()> {
        encode_downlink_default(req, out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sim_preset_roundtrip_and_garbage_resync() {
        let mut codec = SimPresetCodec::new(SimPresetConfig::default()).unwrap();
        assert_eq!(codec.frame_bytes(), 28);
        assert_eq!(codec.declared_channels().len(), 8);

        let mut wire = vec![0xFF, 0xDD, 0x53, 0x99, 0x12, 0x34]; // fake header + garbage
        encode_sim_preset_frame(
            42,
            &[1.0, -2.5, 3.25, 24.5],
            sim_preset_flags(true, false, true, true),
            2,
            &mut wire,
        );
        wire.extend_from_slice(&[0xAB, 0xCD]);
        encode_sim_preset_frame(
            43,
            &[0.5, 0.25, -0.75, 104.0],
            sim_preset_flags(false, true, true, false),
            3,
            &mut wire,
        );

        let mut batch = DecodedBatch::from_descriptors(codec.declared_channels(), 8);
        for &b in &wire {
            codec.feed_bytes(&[b], 5000, &mut batch).unwrap();
        }
        assert_eq!(batch.sample_rows, 2);
        assert_eq!(batch.hw_ticks, vec![42, 43]);
        assert_eq!(
            batch.columns[0].as_scalar_slice().unwrap().get_as_f64(0),
            Some(1.0)
        );
        assert_eq!(
            batch.columns[3].as_scalar_slice().unwrap().get_as_f64(1),
            Some(104.0)
        );
        assert_eq!(
            batch.columns[4].as_scalar_slice().unwrap().get_as_f64(0),
            Some(1.0)
        );
        assert_eq!(
            batch.columns[5].as_scalar_slice().unwrap().get_as_f64(1),
            Some(1.0)
        );
        assert_eq!(batch.columns[7].as_state_slice(), Some(&[2, 3][..]));
    }

    #[test]
    fn fixed_schema_binary_supports_crc_endian_bitfields_and_state() {
        let mut labels = StateEnumMap::new();
        labels.insert(
            0,
            StateLabel {
                name: "IDLE".into(),
                color_rgba: None,
            },
        );
        labels.insert(
            2,
            StateLabel {
                name: "RUN".into(),
                color_rgba: None,
            },
        );

        let cfg = FixedSchemaBinaryConfig {
            source_id: "mcu0".into(),
            header_magic: vec![0xAA, 0x55],
            tail_magic: Some(vec![0x0D, 0x0A]),
            checksum: ChecksumKind::Crc16Ccitt,
            checksum_scope: ChecksumScope::PayloadOnly,
            checksum_endian: Endianness::Little,
            has_hw_tick_first_col: true,
            fields: vec![
                FixedFieldSpec::scalar("tick", ScalarType::U32),
                FixedFieldSpec::scalar("vbus", ScalarType::I16)
                    .with_endian(Endianness::Big)
                    .with_scale(LinearScale::new(0.01, 0.0))
                    .with_unit("V"),
                FixedFieldSpec::bitfield(
                    "pwm_a",
                    ScalarType::Bool,
                    BitfieldSpec {
                        bit_offset: 0,
                        bit_width: 1,
                    },
                    Some(6),
                ),
                FixedFieldSpec::bitfield(
                    "fault",
                    ScalarType::Bool,
                    BitfieldSpec {
                        bit_offset: 2,
                        bit_width: 1,
                    },
                    Some(6),
                ),
                FixedFieldSpec::state_enum("state", ScalarType::U8, labels).with_offset(7),
            ],
            max_carry_bytes: 4096,
        };

        let mut codec = FixedSchemaBinaryCodec::new(cfg).unwrap();
        assert_eq!(codec.frame_bytes(), 2 + 8 + 2 + 2);

        let mut payload = Vec::new();
        payload.extend_from_slice(&1234u32.to_le_bytes());
        payload.extend_from_slice(&2400i16.to_be_bytes());
        payload.push(0b0000_0101); // pwm_a=1, fault=1
        payload.push(2); // RUN
        let crc = compute_checksum(ChecksumKind::Crc16Ccitt, &payload) as u16;

        let mut frame = vec![0xAA, 0x55];
        frame.extend_from_slice(&payload);
        frame.extend_from_slice(&crc.to_le_bytes());
        frame.extend_from_slice(&[0x0D, 0x0A]);

        let mut bad_frame = frame.clone();
        bad_frame[6] ^= 0xFF; // corrupt CRC

        let mut wire = bad_frame;
        wire.extend_from_slice(&frame);

        let mut batch = DecodedBatch::from_descriptors(codec.declared_channels(), 4);
        let rows = codec.feed_bytes(&wire, 77, &mut batch).unwrap();
        assert_eq!(rows, 1);
        assert_eq!(codec.stats().checksum_failures, 1);
        assert_eq!(batch.hw_ticks, vec![1234]);
        assert_eq!(
            batch.columns[0].as_scalar_slice().unwrap().get_as_f64(0),
            Some(2400.0)
        );
        assert_eq!(
            batch.columns[1].as_scalar_slice().unwrap().get_as_f64(0),
            Some(1.0)
        );
        assert_eq!(
            batch.columns[2].as_scalar_slice().unwrap().get_as_f64(0),
            Some(1.0)
        );
        assert_eq!(batch.columns[3].as_state_slice(), Some(&[2][..]));
    }
}
