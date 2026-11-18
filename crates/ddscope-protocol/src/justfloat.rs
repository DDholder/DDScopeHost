//! VOFA+ `JustFloat` and per-channel typed `JustTyped` streaming binary frame decoders (`FR-PR-01`).

use std::collections::HashSet;

use bytes::BytesMut;
use memchr::memmem;
use serde::{Deserialize, Serialize};

use crate::downlink::encode_downlink_default;
use crate::{DownlinkPayload, ProtocolCodec};
use ddscope_types::{
    ChannelDescriptor, CoreError, CoreResult, DecodedBatch, DecodedColumnBuf, ScalarType,
    TimestampNs,
};

/// Canonical 4-byte VOFA+ `JustFloat` frame tail magic (`+inf` in IEEE-754 LE `f32`).
pub const JUSTFLOAT_TAIL_MAGIC: [u8; 4] = [0x00, 0x00, 0x80, 0x7F];

fn default_justfloat_source_id() -> String {
    "just_float".to_string()
}

fn default_justtyped_source_id() -> String {
    "just_typed".to_string()
}

fn default_max_carry_bytes() -> usize {
    64 * 1024
}

/// Policy applied when a `JustFloat` frame contains `NaN` or `Inf` channel values.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NonFinitePolicy {
    /// Discard the entire frame containing any `NaN`/`Inf` channel sample (default).
    #[default]
    RejectFrame,
    /// Replace non-finite channel samples with `0.0` and keep the frame.
    ReplaceWithZero,
    /// Keep non-finite IEEE-754 values as-is.
    Keep,
}

/// Configuration for [`JustFloatCodec`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JustFloatConfig {
    #[serde(default = "default_justfloat_source_id")]
    pub source_id: String,
    pub channel_names: Vec<String>,
    #[serde(default)]
    pub non_finite_policy: NonFinitePolicy,
    #[serde(default = "default_max_carry_bytes")]
    pub max_carry_bytes: usize,
}

impl JustFloatConfig {
    /// Creates a configuration with default `RejectFrame` non-finite policy.
    pub fn new(source_id: impl Into<String>, channel_names: Vec<String>) -> Self {
        Self {
            source_id: source_id.into(),
            channel_names,
            non_finite_policy: NonFinitePolicy::default(),
            max_carry_bytes: default_max_carry_bytes(),
        }
    }
}

/// Statistics tracked by [`JustFloatCodec`] for resynchronization observability.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct JustFloatStats {
    pub decoded_frames: u64,
    pub length_mismatch_frames: u64,
    pub non_finite_frames: u64,
}

/// Shared carry-buffer and SIMD tail-magic framing state machine for `JustFloat` and `JustTyped`.
struct TailMagicFramer {
    payload_bytes: usize,
    frame_bytes: usize,
    max_carry_bytes: usize,
    finder: memmem::Finder<'static>,
    carry: Vec<u8>,
    carry_overflowed: bool,
    stats: JustFloatStats,
    scan_payload_for_inner_magic: bool,
}

impl TailMagicFramer {
    fn new(
        channel_count: usize,
        max_carry_bytes: usize,
        scan_payload_for_inner_magic: bool,
    ) -> Self {
        let payload_bytes = channel_count * 4;
        let frame_bytes = payload_bytes + 4;
        let max_carry_bytes = max_carry_bytes.max(frame_bytes);
        Self {
            payload_bytes,
            frame_bytes,
            max_carry_bytes,
            finder: memmem::Finder::new(&JUSTFLOAT_TAIL_MAGIC),
            carry: Vec::with_capacity(frame_bytes * 2),
            carry_overflowed: false,
            stats: JustFloatStats::default(),
            scan_payload_for_inner_magic,
        }
    }

    fn reset(&mut self) {
        self.carry.clear();
        self.carry_overflowed = false;
    }

    fn push_bounded_carry(&mut self, tail: &[u8]) {
        let keep_max = (self.frame_bytes - 1).min(self.max_carry_bytes);
        if self.carry.len() + tail.len() > keep_max {
            self.carry_overflowed = true;
        }
        if tail.len() >= keep_max {
            self.carry.clear();
            self.carry
                .extend_from_slice(&tail[tail.len() - keep_max..]);
            return;
        }
        let total = self.carry.len() + tail.len();
        if total > keep_max {
            let drop_n = total - keep_max;
            self.carry.drain(..drop_n);
        }
        self.carry.extend_from_slice(tail);
    }

    fn drain_carry<'a, F>(
        &mut self,
        chunk: &'a [u8],
        batch_out: &mut DecodedBatch,
        decode_payload: &mut F,
    ) -> (&'a [u8], usize)
    where
        F: FnMut(&[u8], &mut DecodedBatch, &mut JustFloatStats) -> bool,
    {
        // First, check if a 4-byte tail magic spans the carry/chunk boundary.
        let carry_tail_len = self.carry.len().min(3);
        let chunk_head_len = chunk.len().min(3);
        if carry_tail_len > 0 && carry_tail_len + chunk_head_len >= 4 {
            let mut bridge = [0u8; 6];
            let c_start = self.carry.len() - carry_tail_len;
            bridge[..carry_tail_len].copy_from_slice(&self.carry[c_start..]);
            bridge[carry_tail_len..carry_tail_len + chunk_head_len]
                .copy_from_slice(&chunk[..chunk_head_len]);
            let bridge_slice = &bridge[..carry_tail_len + chunk_head_len];

            if let Some(bpos) = self.finder.find(bridge_slice) {
                let magic_start_in_carry = c_start + bpos;
                let chunk_consumed = bpos + 4 - carry_tail_len;
                let mut decoded = 0;
                if !self.carry_overflowed && magic_start_in_carry == self.payload_bytes {
                    let mut tmp = std::mem::take(&mut self.carry);
                    if decode_payload(&tmp[..self.payload_bytes], batch_out, &mut self.stats) {
                        decoded = 1;
                    }
                    tmp.clear();
                    self.carry = tmp;
                } else {
                    self.stats.length_mismatch_frames += 1;
                    self.carry.clear();
                }
                self.carry_overflowed = false;
                return (&chunk[chunk_consumed..], decoded);
            }
        }

        // No magic spans the boundary; locate the first magic inside `chunk`.
        match self.finder.find(chunk) {
            None => {
                self.push_bounded_carry(chunk);
                (&[], 0)
            }
            Some(pos) => {
                let total_payload_len = self.carry.len() + pos;
                let mut decoded = 0;
                if !self.carry_overflowed && total_payload_len == self.payload_bytes {
                    self.carry.extend_from_slice(&chunk[..pos]);
                    let mut tmp = std::mem::take(&mut self.carry);
                    if decode_payload(&tmp[..self.payload_bytes], batch_out, &mut self.stats) {
                        decoded = 1;
                    }
                    tmp.clear();
                    self.carry = tmp;
                } else {
                    self.stats.length_mismatch_frames += 1;
                    self.carry.clear();
                }
                self.carry_overflowed = false;
                (&chunk[pos + 4..], decoded)
            }
        }
    }

    fn feed_chunk<F>(
        &mut self,
        chunk: &[u8],
        recv_ns: TimestampNs,
        batch_out: &mut DecodedBatch,
        mut decode_payload: F,
    ) -> usize
    where
        F: FnMut(&[u8], &mut DecodedBatch, &mut JustFloatStats) -> bool,
    {
        batch_out.packet_recv_ns = recv_ns;
        if chunk.is_empty() {
            return 0;
        }

        let mut rows_decoded = 0usize;
        let mut remaining = chunk;

        if !self.carry.is_empty() {
            let (rest, boundary_rows) = self.drain_carry(remaining, batch_out, &mut decode_payload);
            rows_decoded += boundary_rows;
            remaining = rest;
        }

        let magic_u32 = u32::from_le_bytes(JUSTFLOAT_TAIL_MAGIC);
        let payload_len = self.payload_bytes;
        let frame_len = self.frame_bytes;

        while remaining.len() >= frame_len {
            let tail_word = u32::from_le_bytes([
                remaining[payload_len],
                remaining[payload_len + 1],
                remaining[payload_len + 2],
                remaining[payload_len + 3],
            ]);
            if tail_word == magic_u32 {
                let payload = &remaining[..payload_len];
                if self.scan_payload_for_inner_magic {
                    if let Some(pos) = self.finder.find(payload) {
                        self.stats.length_mismatch_frames += 1;
                        remaining = &remaining[pos + 4..];
                        continue;
                    }
                }

                if decode_payload(payload, batch_out, &mut self.stats) {
                    rows_decoded += 1;
                }
                remaining = &remaining[frame_len..];
            } else {
                match self.finder.find(remaining) {
                    Some(pos) => {
                        if pos == payload_len {
                            if decode_payload(&remaining[..pos], batch_out, &mut self.stats) {
                                rows_decoded += 1;
                            }
                        } else {
                            self.stats.length_mismatch_frames += 1;
                        }
                        remaining = &remaining[pos + 4..];
                    }
                    None => {
                        self.push_bounded_carry(remaining);
                        remaining = &[];
                        break;
                    }
                }
            }
        }

        if !remaining.is_empty() {
            let mut tail = remaining;
            while let Some(pos) = self.finder.find(tail) {
                if pos == payload_len {
                    if decode_payload(&tail[..pos], batch_out, &mut self.stats) {
                        rows_decoded += 1;
                    }
                } else {
                    self.stats.length_mismatch_frames += 1;
                }
                tail = &tail[pos + 4..];
            }
            if !tail.is_empty() {
                self.push_bounded_carry(tail);
            }
        }

        batch_out.sample_rows += rows_decoded;
        rows_decoded
    }
}

#[inline]
fn decode_justfloat_payload(
    non_finite_policy: NonFinitePolicy,
    payload: &[u8],
    batch_out: &mut DecodedBatch,
    stats: &mut JustFloatStats,
) -> bool {
    if non_finite_policy == NonFinitePolicy::RejectFrame {
        for raw in payload.chunks_exact(4) {
            let v = f32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
            if !v.is_finite() {
                stats.non_finite_frames += 1;
                return false;
            }
        }
    }

    match non_finite_policy {
        NonFinitePolicy::RejectFrame | NonFinitePolicy::Keep => {
            for (c, raw) in payload.chunks_exact(4).enumerate() {
                let v = f32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
                if let DecodedColumnBuf::F32(col) = &mut batch_out.columns[c] {
                    col.push(v);
                }
            }
        }
        NonFinitePolicy::ReplaceWithZero => {
            let mut had_non_finite = false;
            for (c, raw) in payload.chunks_exact(4).enumerate() {
                let mut v = f32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
                if !v.is_finite() {
                    had_non_finite = true;
                    v = 0.0;
                }
                if let DecodedColumnBuf::F32(col) = &mut batch_out.columns[c] {
                    col.push(v);
                }
            }
            if had_non_finite {
                stats.non_finite_frames += 1;
            }
        }
    }

    stats.decoded_frames += 1;
    true
}

/// High-throughput VOFA+ `JustFloat` codec scanning `[0x00, 0x00, 0x80, 0x7F]` via `memchr::memmem`.
pub struct JustFloatCodec {
    channels: Vec<ChannelDescriptor>,
    channel_count: usize,
    non_finite_policy: NonFinitePolicy,
    framer: TailMagicFramer,
}

impl std::fmt::Debug for JustFloatCodec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JustFloatCodec")
            .field("channel_count", &self.channel_count)
            .field("non_finite_policy", &self.non_finite_policy)
            .field("carry_len", &self.framer.carry.len())
            .field("stats", &self.framer.stats)
            .finish()
    }
}

impl JustFloatCodec {
    /// Constructs a new [`JustFloatCodec`] from `config`.
    pub fn new(config: JustFloatConfig) -> CoreResult<Self> {
        if config.channel_names.is_empty() {
            return Err(CoreError::InvalidConfig(
                "JustFloatCodec requires at least 1 channel".into(),
            ));
        }
        let channels: Vec<ChannelDescriptor> = config
            .channel_names
            .into_iter()
            .map(|name| {
                if name.is_empty() {
                    return Err(CoreError::InvalidConfig(
                        "JustFloatCodec channel name must not be empty".into(),
                    ));
                }
                Ok(ChannelDescriptor::new_scalar(
                    config.source_id.as_str(),
                    name,
                    ScalarType::F32,
                ))
            })
            .collect::<CoreResult<_>>()?;

        let channel_count = channels.len();
        let framer = TailMagicFramer::new(
            channel_count,
            config.max_carry_bytes,
            config.non_finite_policy != NonFinitePolicy::RejectFrame,
        );

        Ok(Self {
            channels,
            channel_count,
            non_finite_policy: config.non_finite_policy,
            framer,
        })
    }

    /// Returns cumulative frame decode and rejection counters.
    pub fn stats(&self) -> JustFloatStats {
        self.framer.stats
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
        if batch_out.columns.len() < self.channel_count {
            return Err(CoreError::Protocol {
                message: format!(
                    "DecodedBatch column count {} is smaller than JustFloat channel count {}",
                    batch_out.columns.len(),
                    self.channel_count
                ),
                recoverable: false,
            });
        }
        Ok(())
    }
}

impl ProtocolCodec for JustFloatCodec {
    fn protocol_name(&self) -> &str {
        "JustFloat"
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
        if chunk.is_empty() {
            batch_out.packet_recv_ns = recv_ns;
            return Ok(0);
        }

        let estimated_frames = chunk.len() / self.framer.frame_bytes;
        if estimated_frames > 0 {
            for col in batch_out.columns.iter_mut().take(self.channel_count) {
                if let DecodedColumnBuf::F32(v) = col {
                    v.reserve(estimated_frames);
                }
            }
        }

        let policy = self.non_finite_policy;
        let rows = self.framer.feed_chunk(chunk, recv_ns, batch_out, |payload, out, stats| {
            decode_justfloat_payload(policy, payload, out, stats)
        });
        Ok(rows)
    }

    fn reset_state(&mut self) {
        self.framer.reset();
    }

    fn encode_downlink(&mut self, req: &DownlinkPayload, out: &mut BytesMut) -> CoreResult<()> {
        encode_downlink_default(req, out)
    }
}

/// Per-channel 32-bit little-endian parse type for [`JustTypedCodec`].
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JustTypedChannelKind {
    #[default]
    #[serde(alias = "f32")]
    Float,
    #[serde(alias = "i32", alias = "int32")]
    Int,
    #[serde(alias = "u32", alias = "uint32")]
    Uint,
}

impl JustTypedChannelKind {
    /// Returns the native [`ScalarType`] stored for this channel kind.
    pub const fn scalar_type(self) -> ScalarType {
        match self {
            Self::Float => ScalarType::F32,
            Self::Int => ScalarType::I32,
            Self::Uint => ScalarType::U32,
        }
    }
}

/// Channel specification used by [`JustTypedConfig`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JustTypedChannelSpec {
    pub name: String,
    #[serde(default)]
    pub data_type: JustTypedChannelKind,
}

impl JustTypedChannelSpec {
    /// Creates a new [`JustTypedChannelSpec`] with the given name and 32-bit parse type.
    pub fn new(name: impl Into<String>, data_type: JustTypedChannelKind) -> Self {
        Self {
            name: name.into(),
            data_type,
        }
    }
}

/// Configuration for [`JustTypedCodec`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JustTypedConfig {
    #[serde(default = "default_justtyped_source_id")]
    pub source_id: String,
    pub channels: Vec<JustTypedChannelSpec>,
    #[serde(default)]
    pub non_finite_policy: NonFinitePolicy,
    #[serde(default = "default_max_carry_bytes")]
    pub max_carry_bytes: usize,
}

impl JustTypedConfig {
    /// Creates a [`JustTypedConfig`] with default `RejectFrame` non-finite policy for float slots.
    pub fn new(source_id: impl Into<String>, channels: Vec<JustTypedChannelSpec>) -> Self {
        Self {
            source_id: source_id.into(),
            channels,
            non_finite_policy: NonFinitePolicy::default(),
            max_carry_bytes: default_max_carry_bytes(),
        }
    }
}

#[inline]
fn decode_just_typed_payload(
    channel_kinds: &[JustTypedChannelKind],
    non_finite_policy: NonFinitePolicy,
    payload: &[u8],
    batch_out: &mut DecodedBatch,
    stats: &mut JustFloatStats,
) -> bool {
    debug_assert_eq!(payload.len(), channel_kinds.len() * 4);

    // Apply NonFinitePolicy ONLY to Float slots, and validate all Float slots
    // before pushing to any column so column lengths remain in strict lockstep.
    if non_finite_policy == NonFinitePolicy::RejectFrame {
        for (c, &kind) in channel_kinds.iter().enumerate() {
            if kind == JustTypedChannelKind::Float {
                let off = c * 4;
                let v = f32::from_le_bytes([
                    payload[off],
                    payload[off + 1],
                    payload[off + 2],
                    payload[off + 3],
                ]);
                if !v.is_finite() {
                    stats.non_finite_frames += 1;
                    return false;
                }
            }
        }
    }

    let mut had_non_finite = false;
    for (c, &kind) in channel_kinds.iter().enumerate() {
        let off = c * 4;
        let bytes = [
            payload[off],
            payload[off + 1],
            payload[off + 2],
            payload[off + 3],
        ];
        match kind {
            JustTypedChannelKind::Float => {
                let mut v = f32::from_le_bytes(bytes);
                if non_finite_policy == NonFinitePolicy::ReplaceWithZero && !v.is_finite() {
                    had_non_finite = true;
                    v = 0.0;
                }
                if let DecodedColumnBuf::F32(col) = &mut batch_out.columns[c] {
                    col.push(v);
                }
            }
            JustTypedChannelKind::Int => {
                let v = i32::from_le_bytes(bytes);
                if let DecodedColumnBuf::I32(col) = &mut batch_out.columns[c] {
                    col.push(v);
                }
            }
            JustTypedChannelKind::Uint => {
                let v = u32::from_le_bytes(bytes);
                if let DecodedColumnBuf::U32(col) = &mut batch_out.columns[c] {
                    col.push(v);
                }
            }
        }
    }

    if had_non_finite {
        stats.non_finite_frames += 1;
    }
    stats.decoded_frames += 1;
    true
}

/// High-throughput `JustFloat`-framed codec where each 4-byte LE slot can independently decode as
/// `float` (`f32`), `int` (`i32`), or `uint` (`u32`), terminated by `[0x00, 0x00, 0x80, 0x7F]`.
pub struct JustTypedCodec {
    channels: Vec<ChannelDescriptor>,
    channel_kinds: Vec<JustTypedChannelKind>,
    channel_count: usize,
    non_finite_policy: NonFinitePolicy,
    framer: TailMagicFramer,
}

impl std::fmt::Debug for JustTypedCodec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JustTypedCodec")
            .field("channel_count", &self.channel_count)
            .field("channel_kinds", &self.channel_kinds)
            .field("non_finite_policy", &self.non_finite_policy)
            .field("carry_len", &self.framer.carry.len())
            .field("stats", &self.framer.stats)
            .finish()
    }
}

impl JustTypedCodec {
    /// Constructs a new [`JustTypedCodec`] from `config`.
    pub fn new(config: JustTypedConfig) -> CoreResult<Self> {
        if config.channels.is_empty() {
            return Err(CoreError::InvalidConfig(
                "JustTypedCodec requires at least 1 channel".into(),
            ));
        }

        let mut seen_names = HashSet::with_capacity(config.channels.len());
        let mut channels = Vec::with_capacity(config.channels.len());
        let mut channel_kinds = Vec::with_capacity(config.channels.len());

        for spec in config.channels {
            let trimmed = spec.name.trim();
            if trimmed.is_empty() {
                return Err(CoreError::InvalidConfig(
                    "JustTypedCodec channel name must not be empty".into(),
                ));
            }
            if !seen_names.insert(trimmed.to_owned()) {
                return Err(CoreError::InvalidConfig(format!(
                    "JustTypedCodec duplicate channel name `{trimmed}`"
                )));
            }
            channels.push(ChannelDescriptor::new_scalar(
                config.source_id.as_str(),
                trimmed,
                spec.data_type.scalar_type(),
            ));
            channel_kinds.push(spec.data_type);
        }

        let channel_count = channels.len();
        let framer = TailMagicFramer::new(channel_count, config.max_carry_bytes, true);

        Ok(Self {
            channels,
            channel_kinds,
            channel_count,
            non_finite_policy: config.non_finite_policy,
            framer,
        })
    }

    /// Returns cumulative frame decode and rejection counters.
    pub fn stats(&self) -> JustFloatStats {
        self.framer.stats
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
        if batch_out.columns.len() < self.channel_count {
            return Err(CoreError::Protocol {
                message: format!(
                    "DecodedBatch column count {} is smaller than JustTyped channel count {}",
                    batch_out.columns.len(),
                    self.channel_count
                ),
                recoverable: false,
            });
        }
        for (c, kind) in self.channel_kinds.iter().enumerate() {
            let matches = matches!(
                (&batch_out.columns[c], kind),
                (DecodedColumnBuf::F32(_), JustTypedChannelKind::Float)
                    | (DecodedColumnBuf::I32(_), JustTypedChannelKind::Int)
                    | (DecodedColumnBuf::U32(_), JustTypedChannelKind::Uint)
            );
            if !matches {
                return Err(CoreError::Protocol {
                    message: format!(
                        "DecodedBatch column {c} buffer kind does not match JustTyped channel kind {:?}",
                        kind
                    ),
                    recoverable: false,
                });
            }
        }
        Ok(())
    }
}

impl ProtocolCodec for JustTypedCodec {
    fn protocol_name(&self) -> &str {
        "JustTyped"
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
        if chunk.is_empty() {
            batch_out.packet_recv_ns = recv_ns;
            return Ok(0);
        }

        let estimated_frames = chunk.len() / self.framer.frame_bytes;
        if estimated_frames > 0 {
            for col in batch_out.columns.iter_mut().take(self.channel_count) {
                match col {
                    DecodedColumnBuf::F32(v) => v.reserve(estimated_frames),
                    DecodedColumnBuf::I32(v) => v.reserve(estimated_frames),
                    DecodedColumnBuf::U32(v) => v.reserve(estimated_frames),
                    _ => {}
                }
            }
        }

        let kinds = &self.channel_kinds;
        let policy = self.non_finite_policy;
        let rows = self.framer.feed_chunk(chunk, recv_ns, batch_out, |payload, out, stats| {
            decode_just_typed_payload(kinds, policy, payload, out, stats)
        });
        Ok(rows)
    }

    fn reset_state(&mut self) {
        self.framer.reset();
    }

    fn encode_downlink(&mut self, req: &DownlinkPayload, out: &mut BytesMut) -> CoreResult<()> {
        encode_downlink_default(req, out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_jf_frame(vals: &[f32]) -> Vec<u8> {
        let mut out = Vec::with_capacity((vals.len() + 1) * 4);
        for &v in vals {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&JUSTFLOAT_TAIL_MAGIC);
        out
    }

    #[test]
    fn decodes_sticky_and_one_byte_split_frames() {
        let cfg = JustFloatConfig::new("sim0", vec!["a".into(), "b".into()]);
        let mut codec = JustFloatCodec::new(cfg).unwrap();
        let mut batch = DecodedBatch::from_descriptors(codec.declared_channels(), 8);

        let mut wire = encode_jf_frame(&[1.25, -2.5]);
        wire.extend_from_slice(&encode_jf_frame(&[3.5, 4.75]));

        for &b in &wire {
            codec.feed_bytes(&[b], 999, &mut batch).unwrap();
        }
        assert_eq!(batch.sample_rows, 2);
        assert_eq!(batch.packet_recv_ns, 999);
        assert_eq!(
            batch.columns[0].as_scalar_slice().unwrap().get_as_f64(0),
            Some(1.25)
        );
        assert_eq!(
            batch.columns[1].as_scalar_slice().unwrap().get_as_f64(1),
            Some(4.75)
        );
    }

    #[test]
    fn discards_corrupted_length_and_nan_frames_then_resyncs() {
        let cfg = JustFloatConfig::new("sim0", vec!["a".into(), "b".into()]);
        let mut codec = JustFloatCodec::new(cfg).unwrap();
        let mut batch = DecodedBatch::from_descriptors(codec.declared_channels(), 8);

        let mut wire = vec![0x11, 0x22, 0x33];
        wire.extend_from_slice(&encode_jf_frame(&[1.0, 2.0])); // prepended garbage makes length 11 != 8
        wire.extend_from_slice(&encode_jf_frame(&[f32::NAN, 5.0])); // NaN rejected
        wire.extend_from_slice(&encode_jf_frame(&[6.0, 7.0])); // valid!

        let rows = codec.feed_bytes(&wire, 100, &mut batch).unwrap();
        assert_eq!(rows, 1);
        assert_eq!(batch.sample_rows, 1);
        assert_eq!(codec.stats().length_mismatch_frames, 1);
        assert_eq!(codec.stats().non_finite_frames, 1);
        assert_eq!(
            batch.columns[0].as_scalar_slice().unwrap().get_as_f64(0),
            Some(6.0)
        );
    }

    fn encode_jt_frame(f_val: f32, i_val: i32, u_val: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity(16);
        out.extend_from_slice(&f_val.to_le_bytes());
        out.extend_from_slice(&i_val.to_le_bytes());
        out.extend_from_slice(&u_val.to_le_bytes());
        out.extend_from_slice(&JUSTFLOAT_TAIL_MAGIC);
        out
    }

    fn sample_just_typed_config() -> JustTypedConfig {
        JustTypedConfig::new(
            "mcu0",
            vec![
                JustTypedChannelSpec::new("voltage", JustTypedChannelKind::Float),
                JustTypedChannelSpec::new("signed_cnt", JustTypedChannelKind::Int),
                JustTypedChannelSpec::new("status_bits", JustTypedChannelKind::Uint),
            ],
        )
    }

    #[test]
    fn just_typed_decodes_mixed_float_int_uint_across_sticky_split_and_resync() {
        let mut codec = JustTypedCodec::new(sample_just_typed_config()).unwrap();
        assert_eq!(codec.protocol_name(), "JustTyped");
        assert_eq!(
            codec
                .declared_channels()
                .iter()
                .map(|c| match &c.kind {
                    ddscope_types::ChannelKind::Scalar { scalar_type, .. } => Some(*scalar_type),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            vec![
                Some(ScalarType::F32),
                Some(ScalarType::I32),
                Some(ScalarType::U32),
            ]
        );

        let mut batch = DecodedBatch::from_descriptors(codec.declared_channels(), 8);

        // Include 0xFFFFFFFF (-1 / u32::MAX) and 0x80000001 (i32::MIN+1 / MSB bit31 set),
        // which look like f32 NaN if treated as floats.
        let mut wire = encode_jt_frame(12.5, -1, u32::MAX);
        wire.extend_from_slice(&encode_jt_frame(-3.25, i32::MIN + 1, 0x8000_0001));

        for &b in &wire {
            codec.feed_bytes(&[b], 555, &mut batch).unwrap();
        }
        assert_eq!(batch.sample_rows, 2);
        assert_eq!(batch.packet_recv_ns, 555);
        assert_eq!(codec.stats().non_finite_frames, 0);

        // Fast aligned + garbage resync frame
        let mut wire2 = vec![0xAA, 0xBB];
        wire2.extend_from_slice(&encode_jt_frame(1.0, 2, 3)); // corrupted length
        wire2.extend_from_slice(&encode_jt_frame(7.5, -42, 0xDEAD_BEEF));
        let rows2 = codec.feed_bytes(&wire2, 777, &mut batch).unwrap();
        assert_eq!(rows2, 1);
        assert_eq!(batch.sample_rows, 3);
        assert_eq!(codec.stats().length_mismatch_frames, 1);

        assert_eq!(
            batch.columns[0].as_scalar_slice().unwrap().get_as_f64(0),
            Some(12.5)
        );
        assert_eq!(batch.columns[1], DecodedColumnBuf::I32(vec![-1, i32::MIN + 1, -42]));
        assert_eq!(
            batch.columns[2],
            DecodedColumnBuf::U32(vec![u32::MAX, 0x8000_0001, 0xDEAD_BEEF])
        );
    }

    #[test]
    fn just_typed_non_finite_applies_only_to_float_and_keeps_columns_aligned() {
        let mut codec_reject = JustTypedCodec::new(sample_just_typed_config()).unwrap();
        let mut batch_reject = DecodedBatch::from_descriptors(codec_reject.declared_channels(), 8);

        let mut wire = encode_jt_frame(f32::NAN, -1, u32::MAX);
        wire.extend_from_slice(&encode_jt_frame(4.5, -9, 123));

        let rows = codec_reject
            .feed_bytes(&wire, 100, &mut batch_reject)
            .unwrap();
        assert_eq!(rows, 1);
        assert_eq!(codec_reject.stats().non_finite_frames, 1);
        assert_eq!(batch_reject.columns[0].len(), 1);
        assert_eq!(batch_reject.columns[1].len(), 1);
        assert_eq!(batch_reject.columns[2].len(), 1);
        assert_eq!(batch_reject.columns[1], DecodedColumnBuf::I32(vec![-9]));
        assert_eq!(batch_reject.columns[2], DecodedColumnBuf::U32(vec![123]));

        let mut cfg_replace = sample_just_typed_config();
        cfg_replace.non_finite_policy = NonFinitePolicy::ReplaceWithZero;
        let mut codec_replace = JustTypedCodec::new(cfg_replace).unwrap();
        let mut batch_replace =
            DecodedBatch::from_descriptors(codec_replace.declared_channels(), 8);

        let wire_replace = encode_jt_frame(f32::NEG_INFINITY, -1, 0xFFFF_FFFF);
        let rows_rep = codec_replace
            .feed_bytes(&wire_replace, 200, &mut batch_replace)
            .unwrap();
        assert_eq!(rows_rep, 1);
        assert_eq!(codec_replace.stats().non_finite_frames, 1);
        assert_eq!(batch_replace.columns[0], DecodedColumnBuf::F32(vec![0.0]));
        assert_eq!(batch_replace.columns[1], DecodedColumnBuf::I32(vec![-1]));
        assert_eq!(
            batch_replace.columns[2],
            DecodedColumnBuf::U32(vec![u32::MAX])
        );
    }

    #[test]
    fn just_typed_rejects_empty_blank_or_duplicate_channels() {
        assert!(JustTypedCodec::new(JustTypedConfig::new("s", vec![])).is_err());
        assert!(JustTypedCodec::new(JustTypedConfig::new(
            "s",
            vec![JustTypedChannelSpec::new("   ", JustTypedChannelKind::Int)],
        ))
        .is_err());
        assert!(JustTypedCodec::new(JustTypedConfig::new(
            "s",
            vec![
                JustTypedChannelSpec::new("dup", JustTypedChannelKind::Float),
                JustTypedChannelSpec::new("dup", JustTypedChannelKind::Uint),
            ],
        ))
        .is_err());
    }
}
