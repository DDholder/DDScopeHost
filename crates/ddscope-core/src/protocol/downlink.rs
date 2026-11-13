//! Downlink payload encoding and hex-string utilities (`FR-PR-03`).

use std::fmt::Write as _;

use bytes::{BufMut, BytesMut};

use crate::protocol::DownlinkPayload;
use crate::types::{CoreError, CoreResult, ScalarValue};

/// Stateless helper that encodes [`DownlinkPayload`] variants into wire bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BasicDownlinkEncoder {
    /// Whether `DownlinkPayload::AsciiLine` appends `'\n'` if not already newline-terminated.
    pub ensure_newline: bool,
}

impl Default for BasicDownlinkEncoder {
    fn default() -> Self {
        Self {
            ensure_newline: true,
        }
    }
}

impl BasicDownlinkEncoder {
    /// Creates a new [`BasicDownlinkEncoder`] with default newline termination enabled.
    pub const fn new() -> Self {
        Self {
            ensure_newline: true,
        }
    }

    /// Creates an encoder with explicit newline termination policy for [`DownlinkPayload::AsciiLine`].
    pub const fn with_ensure_newline(ensure_newline: bool) -> Self {
        Self { ensure_newline }
    }

    /// Encodes `payload` and appends the resulting wire bytes into `out`.
    pub fn encode(&self, payload: &DownlinkPayload, out: &mut BytesMut) -> CoreResult<()> {
        match payload {
            DownlinkPayload::RawBytes(bytes) => {
                out.extend_from_slice(bytes);
                Ok(())
            }
            DownlinkPayload::AsciiLine(line) => {
                out.extend_from_slice(line.as_bytes());
                if self.ensure_newline && !line.ends_with('\n') {
                    out.put_u8(b'\n');
                }
                Ok(())
            }
            DownlinkPayload::KeyValue { key, value } => encode_key_value(key, *value, out),
            DownlinkPayload::CommandFields { command_id, fields } => {
                encode_command_fields(command_id, fields, out)
            }
        }
    }
}

/// Encodes `payload` using [`BasicDownlinkEncoder::default()`].
pub fn encode_downlink_default(payload: &DownlinkPayload, out: &mut BytesMut) -> CoreResult<()> {
    BasicDownlinkEncoder::default().encode(payload, out)
}

/// Parses an ASCII hex string (accepting spaces/whitespace and `0x`/`0X` prefixes) and appends
/// the decoded raw bytes to `out` (`FR-PR-03`).
///
/// Examples of accepted inputs:
/// - `"DE AD BE EF"`
/// - `"0xDE 0xAD 0xBE 0xEF"`
/// - `"0xdeadbeef"`
/// - `"deadbeef"`
pub fn encode_hex_string(hex_str: &str, out: &mut BytesMut) -> CoreResult<()> {
    let mut any_token = false;
    let start_len = out.len();
    for raw_tok in hex_str.split_whitespace() {
        any_token = true;
        let tok = raw_tok
            .strip_prefix("0x")
            .or_else(|| raw_tok.strip_prefix("0X"))
            .unwrap_or(raw_tok);

        if tok.is_empty() || tok.len() % 2 != 0 {
            out.truncate(start_len);
            return Err(CoreError::Protocol {
                message: format!("invalid hex token `{raw_tok}`: expected even number of hex digits"),
                recoverable: true,
            });
        }

        let byte_count = tok.len() / 2;
        let prev_len = out.len();
        out.resize(prev_len + byte_count, 0);
        if let Err(err) = hex::decode_to_slice(tok, &mut out[prev_len..]) {
            out.truncate(start_len);
            return Err(CoreError::Protocol {
                message: format!("invalid hex token `{raw_tok}`: {err}"),
                recoverable: true,
            });
        }
    }

    if !any_token {
        return Ok(());
    }
    Ok(())
}

fn encode_key_value(key: &str, value: ScalarValue, out: &mut BytesMut) -> CoreResult<()> {
    if key.is_empty() || key.contains(['\n', '\r', '=']) {
        return Err(CoreError::Protocol {
            message: format!("invalid downlink key `{key}`"),
            recoverable: true,
        });
    }
    validate_scalar_finite(value)?;
    out.extend_from_slice(key.as_bytes());
    out.put_u8(b'=');
    write_scalar_value(value, out)?;
    out.put_u8(b'\n');
    Ok(())
}

fn encode_command_fields(
    command_id: &str,
    fields: &[(String, ScalarValue)],
    out: &mut BytesMut,
) -> CoreResult<()> {
    if command_id.is_empty() || command_id.contains(['\n', '\r']) {
        return Err(CoreError::Protocol {
            message: format!("invalid downlink command_id `{command_id}`"),
            recoverable: true,
        });
    }
    for (k, v) in fields {
        if k.is_empty() || k.contains(['\n', '\r', '=', ',']) {
            return Err(CoreError::Protocol {
                message: format!("invalid command field key `{k}`"),
                recoverable: true,
            });
        }
        validate_scalar_finite(*v)?;
    }

    out.extend_from_slice(command_id.as_bytes());
    for (idx, (k, v)) in fields.iter().enumerate() {
        out.put_u8(if idx == 0 { b' ' } else { b',' });
        out.extend_from_slice(k.as_bytes());
        out.put_u8(b'=');
        write_scalar_value(*v, out)?;
    }
    out.put_u8(b'\n');
    Ok(())
}

fn validate_scalar_finite(value: ScalarValue) -> CoreResult<()> {
    match value {
        ScalarValue::F32(v) if !v.is_finite() => Err(CoreError::Protocol {
            message: "non-finite f32 downlink value".into(),
            recoverable: true,
        }),
        ScalarValue::F64(v) if !v.is_finite() => Err(CoreError::Protocol {
            message: "non-finite f64 downlink value".into(),
            recoverable: true,
        }),
        _ => Ok(()),
    }
}

fn write_scalar_value(value: ScalarValue, out: &mut BytesMut) -> CoreResult<()> {
    let mut buf = ByteWriter(out);
    match value {
        ScalarValue::Bool(v) => buf.write_str(if v { "1" } else { "0" }),
        ScalarValue::U8(v) => write!(buf, "{v}"),
        ScalarValue::U16(v) => write!(buf, "{v}"),
        ScalarValue::U32(v) => write!(buf, "{v}"),
        ScalarValue::U64(v) => write!(buf, "{v}"),
        ScalarValue::I8(v) => write!(buf, "{v}"),
        ScalarValue::I16(v) => write!(buf, "{v}"),
        ScalarValue::I32(v) => write!(buf, "{v}"),
        ScalarValue::I64(v) => write!(buf, "{v}"),
        ScalarValue::F32(v) => write!(buf, "{v}"),
        ScalarValue::F64(v) => write!(buf, "{v}"),
    }
    .map_err(|e| CoreError::Protocol {
        message: format!("failed formatting downlink scalar: {e}"),
        recoverable: true,
    })
}

struct ByteWriter<'a>(&'a mut BytesMut);

impl std::fmt::Write for ByteWriter<'_> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0.extend_from_slice(s.as_bytes());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_all_downlink_payload_variants_and_hex() {
        let enc = BasicDownlinkEncoder::default();
        let mut out = BytesMut::new();

        enc.encode(&DownlinkPayload::RawBytes(vec![0xAA, 0x55]), &mut out)
            .unwrap();
        assert_eq!(&out[..], &[0xAA, 0x55]);
        out.clear();

        enc.encode(&DownlinkPayload::AsciiLine("GLITCH".into()), &mut out)
            .unwrap();
        assert_eq!(&out[..], b"GLITCH\n");
        out.clear();

        enc.encode(&DownlinkPayload::AsciiLine("RATE=1000\n".into()), &mut out)
            .unwrap();
        assert_eq!(&out[..], b"RATE=1000\n");
        out.clear();

        enc.encode(
            &DownlinkPayload::KeyValue {
                key: "kp".into(),
                value: ScalarValue::F64(1.25),
            },
            &mut out,
        )
        .unwrap();
        assert_eq!(&out[..], b"kp=1.25\n");
        out.clear();

        enc.encode(
            &DownlinkPayload::CommandFields {
                command_id: "PID".into(),
                fields: vec![
                    ("kp".into(), ScalarValue::F32(0.5)),
                    ("en".into(), ScalarValue::Bool(true)),
                ],
            },
            &mut out,
        )
        .unwrap();
        assert_eq!(&out[..], b"PID kp=0.5,en=1\n");
        out.clear();

        encode_hex_string("0xDE 0Xad BE ef", &mut out).unwrap();
        assert_eq!(&out[..], &[0xDE, 0xAD, 0xBE, 0xEF]);
        out.clear();

        encode_hex_string("0x010203", &mut out).unwrap();
        assert_eq!(&out[..], &[0x01, 0x02, 0x03]);
    }

    #[test]
    fn rejects_invalid_hex_and_non_finite_values() {
        let mut out = BytesMut::from(&b"keep"[..]);
        assert!(encode_hex_string("0xZZ", &mut out).is_err());
        assert_eq!(&out[..], b"keep");

        assert!(encode_hex_string("0x123", &mut out).is_err());
        assert_eq!(&out[..], b"keep");

        let enc = BasicDownlinkEncoder::default();
        assert!(enc
            .encode(
                &DownlinkPayload::KeyValue {
                    key: "kp".into(),
                    value: ScalarValue::F64(f64::NAN),
                },
                &mut out,
            )
            .is_err());
    }
}
