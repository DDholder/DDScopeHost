//! VOFA+ `JustFloat` streaming binary frame decoder (`FR-PR-01`).

use bytes::BytesMut;
use memchr::memmem;
use serde::{Deserialize, Serialize};

use crate::protocol::downlink::encode_downlink_default;
use crate::protocol::{DownlinkPayload, ProtocolCodec};
use crate::types::{
    ChannelDescriptor, CoreError, CoreResult, DecodedBatch, DecodedColumnBuf, ScalarType,
    TimestampNs,
};

/// Canonical 4-byte VOFA+ `JustFloat` frame tail magic (`+inf` in IEEE-754 LE `f32`).
pub const JUSTFLOAT_TAIL_MAGIC: [u8; 4] = [0x00, 0x00, 0x80, 0x7F];

fn default_justfloat_source_id() -> String {
    "just_float".to_string()
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

/// High-throughput VOFA+ `JustFloat` codec scanning `[0x00, 0x00, 0x80, 0x7F]` via `memchr::memmem`.
pub struct JustFloatCodec {
    channels: Vec<ChannelDescriptor>,
    channel_count: usize,
    payload_bytes: usize,
    frame_bytes: usize,
    non_finite_policy: NonFinitePolicy,
    max_carry_bytes: usize,
    finder: memmem::Finder<'static>,
    carry: Vec<u8>,
    carry_overflowed: bool,
    stats: JustFloatStats,
}

impl std::fmt::Debug for JustFloatCodec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JustFloatCodec")
            .field("channel_count", &self.channel_count)
            .field("non_finite_policy", &self.non_finite_policy)
            .field("carry_len", &self.carry.len())
            .field("stats", &self.stats)
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
        let payload_bytes = channel_count * 4;
        let frame_bytes = payload_bytes + 4;
        let max_carry_bytes = config.max_carry_bytes.max(frame_bytes);

        Ok(Self {
            channels,
            channel_count,
            payload_bytes,
            frame_bytes,
            non_finite_policy: config.non_finite_policy,
            max_carry_bytes,
            finder: memmem::Finder::new(&JUSTFLOAT_TAIL_MAGIC),
            carry: Vec::with_capacity(frame_bytes * 2),
            carry_overflowed: false,
            stats: JustFloatStats::default(),
        })
    }

    /// Returns cumulative frame decode and rejection counters.
    pub fn stats(&self) -> JustFloatStats {
        self.stats
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

    #[inline]
    fn decode_payload(&mut self, payload: &[u8], batch_out: &mut DecodedBatch) -> bool {
        debug_assert_eq!(payload.len(), self.payload_bytes);

        if self.non_finite_policy == NonFinitePolicy::RejectFrame {
            for raw in payload.chunks_exact(4) {
                let v = f32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
                if !v.is_finite() {
                    self.stats.non_finite_frames += 1;
                    return false;
                }
            }
        }

        match self.non_finite_policy {
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
                    self.stats.non_finite_frames += 1;
                }
            }
        }

        self.stats.decoded_frames += 1;
        true
    }

    /// Resolves any pending bytes in `self.carry` against the head of `chunk`, returning the
    /// remaining unconsumed slice of `chunk` and the number of rows decoded from the boundary.
    fn drain_carry<'a>(
        &mut self,
        chunk: &'a [u8],
        batch_out: &mut DecodedBatch,
    ) -> (&'a [u8], usize) {
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
                // Magic ends inside `chunk` at `chunk_consumed`.
                let magic_start_in_carry = c_start + bpos;
                let chunk_consumed = bpos + 4 - carry_tail_len;
                let mut decoded = 0;
                if !self.carry_overflowed && magic_start_in_carry == self.payload_bytes {
                    let mut tmp = std::mem::take(&mut self.carry);
                    if self.decode_payload(&tmp[..self.payload_bytes], batch_out) {
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
                    if self.decode_payload(&tmp[..self.payload_bytes], batch_out) {
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
}

impl ProtocolCodec for JustFloatCodec {
    fn protocol_name(&self) -> &'static str {
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
        batch_out.packet_recv_ns = recv_ns;
        if chunk.is_empty() {
            return Ok(0);
        }

        let estimated_frames = chunk.len() / self.frame_bytes;
        if estimated_frames > 0 {
            for col in batch_out.columns.iter_mut().take(self.channel_count) {
                if let DecodedColumnBuf::F32(v) = col {
                    v.reserve(estimated_frames);
                }
            }
        }

        let mut rows_decoded = 0usize;
        let mut remaining = chunk;

        if !self.carry.is_empty() {
            let (rest, boundary_rows) = self.drain_carry(remaining, batch_out);
            rows_decoded += boundary_rows;
            remaining = rest;
        }

        // Fast zero-copy aligned loop when frames are well-formed back-to-back,
        // falling back to SIMD memmem scan on any mismatch or partial tail.
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
                // Verify no earlier magic inside payload when non_finite_policy != RejectFrame,
                // or RejectFrame automatically rejects `+inf` (which IS `JUSTFLOAT_TAIL_MAGIC`).
                let payload = &remaining[..payload_len];
                if self.non_finite_policy != NonFinitePolicy::RejectFrame
                    && self.finder.find(payload).is_some()
                {
                    // Corrupted/misaligned payload containing an earlier tail magic.
                    let pos = self.finder.find(payload).expect("checked above");
                    self.stats.length_mismatch_frames += 1;
                    remaining = &remaining[pos + 4..];
                    continue;
                }

                if self.decode_payload(payload, batch_out) {
                    rows_decoded += 1;
                }
                remaining = &remaining[frame_len..];
            } else {
                // Misaligned or corrupted stream: scan forward via SIMD memmem.
                match self.finder.find(remaining) {
                    Some(pos) => {
                        if pos == payload_len {
                            // Reachable only if earlier check missed (never).
                            if self.decode_payload(&remaining[..pos], batch_out) {
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
            // Drain any complete (possibly short/corrupted) frames in the trailing `< frame_len` bytes.
            let mut tail = remaining;
            while let Some(pos) = self.finder.find(tail) {
                if pos == payload_len {
                    if self.decode_payload(&tail[..pos], batch_out) {
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
        Ok(rows_decoded)
    }

    fn reset_state(&mut self) {
        self.carry.clear();
        self.carry_overflowed = false;
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
}
