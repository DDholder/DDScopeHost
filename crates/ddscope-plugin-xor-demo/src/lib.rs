//! Sample C-ABI (`cdylib`) protocol plugin for DDScopeHost.
//!
//! Compiles completely standalone (only depends on `ddscope-plugin-api`) into
//! `ddscope_plugin_xor_demo.dll` / `.so` / `.dylib`.
//!
//! Wire format (15 bytes total):
//! - Bytes `0..2`: unmasked frame header `[0xDD, 0x7E]`
//! - Bytes `2..14`: XOR-masked 12-byte payload (`b ^ xor_key`), unpacking:
//!   - `[0..4]`: `ch0` (`f32` little-endian)
//!   - `[4..8]`: `ch1` (`f32` little-endian)
//!   - `[8..12]`: `seq` (`u32` little-endian)
//! - Byte `14`: 1-byte XOR checksum of all 12 unmasked payload bytes

use core::ffi::c_void;

use ddscope_plugin_api::{
    CBatchSink, CByteSlice, CChannelDesc, CChannelSink, CErrorSink, CScalarType,
    ProtocolPluginVTableV1, DDSCOPE_C_ABI_VERSION,
};
use serde::Deserialize;

const MAGIC: [u8; 2] = [0xDD, 0x7E];
const FRAME_LEN: usize = 15;
const PLUGIN_ID_BYTES: &[u8] = b"custom_xor_frame";
const DISPLAY_NAME_BYTES: &[u8] = b"XOR Masked Frame (C-ABI Demo)";

fn default_xor_key() -> u8 {
    0x5A
}

fn default_ch0() -> String {
    "xor_ia".to_string()
}

fn default_ch1() -> String {
    "xor_ib".to_string()
}

fn default_seq() -> String {
    "xor_seq".to_string()
}

#[derive(Debug, Deserialize)]
struct XorPluginConfig {
    #[serde(default = "default_xor_key")]
    xor_key: u8,
    #[serde(default = "default_ch0")]
    ch0_name: String,
    #[serde(default = "default_ch1")]
    ch1_name: String,
    #[serde(default = "default_seq")]
    seq_name: String,
}

impl Default for XorPluginConfig {
    fn default() -> Self {
        Self {
            xor_key: default_xor_key(),
            ch0_name: default_ch0(),
            ch1_name: default_ch1(),
            seq_name: default_seq(),
        }
    }
}

struct XorPluginInstance {
    xor_key: u8,
    ch0_name: String,
    ch1_name: String,
    seq_name: String,
    decoded_frames: u64,
    bad_checksums: u64,
}

unsafe fn write_err(err: *const CErrorSink, msg: &str) {
    if !err.is_null() {
        let sink = unsafe { &*err };
        unsafe {
            (sink.set_error)(sink.ctx, CByteSlice::from_str(msg));
        }
    }
}

unsafe extern "C" fn create_instance(
    config_json: CByteSlice,
    err: *const CErrorSink,
) -> *mut c_void {
    let raw = unsafe { config_json.as_slice() };
    let text = core::str::from_utf8(raw).unwrap_or("").trim();
    let cfg: XorPluginConfig = if text.is_empty() {
        XorPluginConfig::default()
    } else {
        match serde_json::from_str(text) {
            Ok(c) => c,
            Err(e) => {
                unsafe {
                    write_err(err, &format!("invalid custom_xor_frame config JSON: {e}"));
                }
                return core::ptr::null_mut();
            }
        }
    };

    if cfg.ch0_name.trim().is_empty()
        || cfg.ch1_name.trim().is_empty()
        || cfg.seq_name.trim().is_empty()
    {
        unsafe {
            write_err(err, "custom_xor_frame channel names must not be empty");
        }
        return core::ptr::null_mut();
    }

    let inst = Box::new(XorPluginInstance {
        xor_key: cfg.xor_key,
        ch0_name: cfg.ch0_name.trim().to_string(),
        ch1_name: cfg.ch1_name.trim().to_string(),
        seq_name: cfg.seq_name.trim().to_string(),
        decoded_frames: 0,
        bad_checksums: 0,
    });
    Box::into_raw(inst).cast()
}

unsafe extern "C" fn destroy_instance(instance: *mut c_void) {
    if !instance.is_null() {
        drop(unsafe { Box::from_raw(instance.cast::<XorPluginInstance>()) });
    }
}

unsafe extern "C" fn reset_instance(instance: *mut c_void) {
    if !instance.is_null() {
        let inst = unsafe { &mut *(instance.cast::<XorPluginInstance>()) };
        inst.decoded_frames = 0;
        inst.bad_checksums = 0;
    }
}

unsafe extern "C" fn declare_channels(instance: *mut c_void, sink_ptr: *const CChannelSink) {
    if instance.is_null() || sink_ptr.is_null() {
        return;
    }
    let inst = unsafe { &*(instance.cast::<XorPluginInstance>()) };
    let sink = unsafe { &*sink_ptr };
    let defs = [
        CChannelDesc {
            name: CByteSlice::from_str(&inst.ch0_name),
            scalar_type: CScalarType::F32,
            unit: CByteSlice::from_str("A"),
        },
        CChannelDesc {
            name: CByteSlice::from_str(&inst.ch1_name),
            scalar_type: CScalarType::F32,
            unit: CByteSlice::from_str("A"),
        },
        CChannelDesc {
            name: CByteSlice::from_str(&inst.seq_name),
            scalar_type: CScalarType::U32,
            unit: CByteSlice::empty(),
        },
    ];
    for desc in defs {
        unsafe {
            (sink.push_channel)(sink.ctx, desc);
        }
    }
}

unsafe extern "C" fn decode_chunk(
    instance: *mut c_void,
    input: CByteSlice,
    _is_eof: u8,
    sink_ptr: *const CBatchSink,
    err: *const CErrorSink,
) -> isize {
    if instance.is_null() || sink_ptr.is_null() {
        unsafe {
            write_err(err, "null instance or sink passed to custom_xor_frame");
        }
        return -1;
    }

    let inst = unsafe { &mut *(instance.cast::<XorPluginInstance>()) };
    let sink = unsafe { &*sink_ptr };
    let data = unsafe { input.as_slice() };

    let mut pos = 0usize;
    while pos + FRAME_LEN <= data.len() {
        if data[pos] != MAGIC[0] || data[pos + 1] != MAGIC[1] {
            if let Some(rel) = data[pos..].windows(2).position(|w| w == MAGIC) {
                pos += rel;
                if pos + FRAME_LEN > data.len() {
                    break;
                }
            } else {
                // Retain trailing byte in case it equals MAGIC[0] across chunk splits.
                pos = if *data.last().unwrap_or(&0) == MAGIC[0] {
                    data.len() - 1
                } else {
                    data.len()
                };
                break;
            }
        }

        let frame = &data[pos..pos + FRAME_LEN];
        let mut unmasked = [0u8; 12];
        let mut xor_acc = 0u8;
        for (dst, &src) in unmasked.iter_mut().zip(&frame[2..14]) {
            let b = src ^ inst.xor_key;
            *dst = b;
            xor_acc ^= b;
        }

        if xor_acc != frame[14] {
            inst.bad_checksums += 1;
            unsafe {
                (sink.record_framing_error)(sink.ctx, 1);
            }
            pos += 1;
            continue;
        }

        let ch0 = f32::from_le_bytes([unmasked[0], unmasked[1], unmasked[2], unmasked[3]]);
        let ch1 = f32::from_le_bytes([unmasked[4], unmasked[5], unmasked[6], unmasked[7]]);
        let seq = u32::from_le_bytes([unmasked[8], unmasked[9], unmasked[10], unmasked[11]]);

        unsafe {
            (sink.push_f32)(sink.ctx, 0, ch0);
            (sink.push_f32)(sink.ctx, 1, ch1);
            (sink.push_u32)(sink.ctx, 2, seq);
            (sink.finish_row)(sink.ctx);
        }
        inst.decoded_frames += 1;
        pos += FRAME_LEN;
    }

    pos as isize
}

/// Exported C-ABI v1 entry point loaded via `libloading`.
#[no_mangle]
pub unsafe extern "C" fn ddscope_protocol_plugin_v1() -> ProtocolPluginVTableV1 {
    ProtocolPluginVTableV1 {
        abi_version: DDSCOPE_C_ABI_VERSION,
        plugin_id: CByteSlice::from_slice(PLUGIN_ID_BYTES),
        display_name: CByteSlice::from_slice(DISPLAY_NAME_BYTES),
        create_instance,
        destroy_instance,
        reset_instance,
        declare_channels,
        decode_chunk,
    }
}

/// Helper for tests constructing wire frames matching this plugin's encoding.
pub fn encode_xor_demo_frame(xor_key: u8, ch0: f32, ch1: f32, seq: u32) -> [u8; FRAME_LEN] {
    let mut plain = [0u8; 12];
    plain[0..4].copy_from_slice(&ch0.to_le_bytes());
    plain[4..8].copy_from_slice(&ch1.to_le_bytes());
    plain[8..12].copy_from_slice(&seq.to_le_bytes());
    let mut xor_acc = 0u8;
    for b in plain {
        xor_acc ^= b;
    }
    let mut out = [0u8; FRAME_LEN];
    out[0..2].copy_from_slice(&MAGIC);
    for (dst, src) in out[2..14].iter_mut().zip(plain) {
        *dst = src ^ xor_key;
    }
    out[14] = xor_acc;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_and_vtable_contract_is_well_formed() {
        let vt = unsafe { ddscope_protocol_plugin_v1() };
        assert_eq!(vt.abi_version, DDSCOPE_C_ABI_VERSION);
        let frame = encode_xor_demo_frame(0x5A, 12.5, -3.25, 42);
        assert_eq!(frame[0..2], MAGIC);
    }
}
