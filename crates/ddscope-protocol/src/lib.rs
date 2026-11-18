//! Bidirectional protocol codec contracts and Phase 2 ACK transaction hooks.

pub mod downlink;
pub mod firewater;
pub mod fixed_binary;
pub mod justfloat;
pub mod plugin_host;
pub mod registry;

pub use downlink::{encode_downlink_default, encode_hex_string, BasicDownlinkEncoder};
pub use firewater::{
    default_sim_state_labels, parse_ascii_f32, parse_ascii_f64, FireWaterColumnKind,
    FireWaterColumnSpec, FireWaterConfig, FireWaterCsvCodec, FireWaterStats,
};
pub use fixed_binary::{
    compute_checksum, encode_sim_preset_frame, sim_ctrl_state_labels, sim_preset_flags,
    BinaryCodecStats, ChecksumKind, ChecksumScope, FixedFieldSpec, FixedSchemaBinaryCodec,
    FixedSchemaBinaryConfig, SimPresetCodec, SimPresetConfig, SIM_PRESET_MAGIC,
    SIM_PRESET_TAIL,
};
pub use justfloat::{
    JustFloatCodec, JustFloatConfig, JustFloatStats, JustTypedChannelKind, JustTypedChannelSpec,
    JustTypedCodec, JustTypedConfig, NonFinitePolicy, JUSTFLOAT_TAIL_MAGIC,
};
pub use registry::{
    create_fire_water_codec, create_fixed_schema_binary_codec, create_just_float_codec,
    create_just_typed_codec, create_sim_preset_codec, ProtocolRegistry,
};

use std::sync::Arc;

use bytes::BytesMut;
use serde::{Deserialize, Serialize};

use ddscope_types::{
    ChannelDescriptor, CoreResult, DecodedBatch, InboundAckEvent, ScalarValue, StructSchema,
    TimestampNs,
};

/// Static factory function signature for creating a [`ProtocolCodec`] instance from JSON config.
pub type ProtocolCodecFactoryFn = fn(&str) -> CoreResult<Box<dyn ProtocolCodec>>;

/// Thread-safe factory closure/function handle stored inside [`ProtocolRegistry`].
pub type ProtocolCodecFactory =
    Arc<dyn Fn(&str) -> CoreResult<Box<dyn ProtocolCodec>> + Send + Sync>;

/// Rule used by Phase 2 ACK transaction tracker to match inbound responses (Hook `H1`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AckMatcherRule {
    ByMsgId { msg_id: u32 },
    ByEchoSeq { seq: u64 },
    ByFunctionCode { code: u8 },
    AsciiPrefix { prefix: String },
}

/// Downlink transmission reliability semantics.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DownlinkMode {
    /// Non-blocking transmission with optional coalescing by key.
    FireAndForget { coalesce: bool },
    /// Request-response transaction waiting for an uplink ACK match (Phase 2).
    AckTransaction {
        timeout_ms: u32,
        max_retries: u8,
        expected_ack_matcher: AckMatcherRule,
    },
}

/// Outcome of a downlink dispatch operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransactionResult {
    SentFireAndForget,
    AckSuccess { rtt_us: u64 },
    AckRejected { error_code: i64, reason: String },
    TimedOut { attempts: u8 },
}

/// Logical payload handed to [`ProtocolCodec::encode_downlink`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum DownlinkPayload {
    RawBytes(Vec<u8>),
    AsciiLine(String),
    KeyValue {
        key: String,
        value: ScalarValue,
    },
    CommandFields {
        command_id: String,
        fields: Vec<(String, ScalarValue)>,
    },
}

/// Stateful framing, batch decoding, and downlink encoding contract implemented by all protocols.
pub trait ProtocolCodec: Send {
    /// Human-readable protocol name (e.g., `"JustFloat"`, `"FireWater"`, or plugin ID).
    fn protocol_name(&self) -> &str;

    /// Returns the static channel descriptors declared by this codec configuration.
    fn declared_channels(&self) -> &[ChannelDescriptor];

    /// Optional structured message schemas declared by `.ddproto` / C-struct codecs (Hook `H1`).
    fn declared_schemas(&self) -> &[Arc<StructSchema>] {
        &[]
    }

    /// Incrementally parses incoming bytes into `batch_out` (SoA columns) and returns consumed bytes
    /// or decoded sample row count depending on codec framing.
    fn feed_bytes(
        &mut self,
        chunk: &[u8],
        recv_ns: TimestampNs,
        batch_out: &mut DecodedBatch,
    ) -> CoreResult<usize>;

    /// Resets any partial frame or re-synchronization buffer (e.g. on replay seek or reconnect).
    fn reset_state(&mut self);

    /// Encodes a logical downlink request into raw wire bytes appended to `out`.
    fn encode_downlink(&mut self, req: &DownlinkPayload, out: &mut BytesMut) -> CoreResult<()>;

    /// Evaluates whether an inbound ACK event satisfies `rule` (Hook `H1`).
    fn matches_ack(&self, _rule: &AckMatcherRule, _event: &InboundAckEvent) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddscope_types::{DecodedColumnBuf, ScalarType};

    struct MockF32Codec {
        channels: Vec<ChannelDescriptor>,
    }

    impl ProtocolCodec for MockF32Codec {
        fn protocol_name(&self) -> &str {
            "MockF32"
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
            batch_out.packet_recv_ns = recv_ns;
            let count = chunk.len() / 4;
            if let Some(DecodedColumnBuf::F32(col)) = batch_out.columns.first_mut() {
                for bytes in chunk.chunks_exact(4) {
                    let v = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                    col.push(v);
                }
            }
            batch_out.sample_rows = count;
            Ok(count)
        }

        fn reset_state(&mut self) {}

        fn encode_downlink(&mut self, req: &DownlinkPayload, out: &mut BytesMut) -> CoreResult<()> {
            if let DownlinkPayload::AsciiLine(line) = req {
                out.extend_from_slice(line.as_bytes());
            }
            Ok(())
        }
    }

    #[test]
    fn protocol_codec_is_object_safe_and_populates_decoded_batch() {
        let ch = ChannelDescriptor::new_scalar("sim0", "vbus", ScalarType::F32);
        let mut codec: Box<dyn ProtocolCodec> = Box::new(MockF32Codec {
            channels: vec![ch],
        });
        assert_eq!(codec.protocol_name(), "MockF32");
        assert!(codec.declared_schemas().is_empty());

        let mut batch = DecodedBatch::from_descriptors(codec.declared_channels(), 8);
        let sample = 12.5_f32.to_le_bytes();
        let rows = codec
            .feed_bytes(&sample, 42_000, &mut batch)
            .expect("mock feed should succeed");
        assert_eq!(rows, 1);
        assert_eq!(batch.sample_rows, 1);
        assert_eq!(batch.packet_recv_ns, 42_000);
    }
}
