//! Stable C-ABI v1 vtable contracts for dynamically loaded native protocol plugins (`cdylib`).
//!
//! Safety rule: No Rust-allocated `Vec`, `String`, `Box`, or trait objects ever cross
//! the `.dll` / `.so` / `.dylib` boundary. All decoded samples are streamed back to the
//! host via callback pointers on [`CBatchSink`].

use core::ffi::c_void;

/// Current C-ABI contract version checked by the host upon symbol load.
pub const DDSCOPE_C_ABI_VERSION: u32 = 1;

/// Null-terminated exported symbol name looked up via `libloading`.
pub const DDSCOPE_PROTOCOL_SYMBOL_V1: &[u8] = b"ddscope_protocol_plugin_v1\0";

/// Borrowed byte slice across the C-ABI boundary.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct CByteSlice {
    pub ptr: *const u8,
    pub len: usize,
}

impl CByteSlice {
    /// Returns an empty slice.
    #[inline]
    pub const fn empty() -> Self {
        Self {
            ptr: core::ptr::null(),
            len: 0,
        }
    }

    /// Wraps a Rust byte slice for passing across the C-ABI boundary during the call.
    #[inline]
    pub const fn from_slice(bytes: &[u8]) -> Self {
        Self {
            ptr: if bytes.is_empty() {
                core::ptr::null()
            } else {
                bytes.as_ptr()
            },
            len: bytes.len(),
        }
    }

    /// Wraps a UTF-8 string slice for passing across the C-ABI boundary during the call.
    #[inline]
    pub const fn from_str(text: &str) -> Self {
        Self::from_slice(text.as_bytes())
    }

    /// Reconstructs a borrowed Rust byte slice from a valid [`CByteSlice`].
    ///
    /// # Safety
    /// The caller must guarantee `self.ptr` is valid for reads of `self.len` bytes
    /// for the duration of `'a` whenever `self.len > 0`.
    #[inline]
    pub unsafe fn as_slice<'a>(self) -> &'a [u8] {
        if self.len == 0 || self.ptr.is_null() {
            &[]
        } else {
            unsafe { core::slice::from_raw_parts(self.ptr, self.len) }
        }
    }
}

/// Scalar sample types supported across the C-ABI v1 boundary.
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CScalarType {
    F32 = 0,
    I32 = 1,
    U32 = 2,
    F64 = 3,
    Bool = 4,
}

impl CScalarType {
    /// Converts a raw ABI byte tag into a [`CScalarType`].
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::F32),
            1 => Some(Self::I32),
            2 => Some(Self::U32),
            3 => Some(Self::F64),
            4 => Some(Self::Bool),
            _ => None,
        }
    }

    /// Returns the lowercase wire/display string identifier.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::F32 => "f32",
            Self::I32 => "i32",
            Self::U32 => "u32",
            Self::F64 => "f64",
            Self::Bool => "bool",
        }
    }
}

/// Static channel declaration emitted from a C-ABI plugin via [`CChannelSink`].
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct CChannelDesc {
    pub name: CByteSlice,
    pub scalar_type: CScalarType,
    pub unit: CByteSlice,
}

/// Host callback sink invoked by `declare_channels`.
#[repr(C)]
pub struct CChannelSink {
    pub ctx: *mut c_void,
    pub push_channel: unsafe extern "C" fn(ctx: *mut c_void, desc: CChannelDesc),
}

/// Host Structure-of-Arrays (SoA) batch sink invoked per decoded sample row.
#[repr(C)]
pub struct CBatchSink {
    pub ctx: *mut c_void,
    pub push_f32: unsafe extern "C" fn(ctx: *mut c_void, col_idx: usize, val: f32),
    pub push_i32: unsafe extern "C" fn(ctx: *mut c_void, col_idx: usize, val: i32),
    pub push_u32: unsafe extern "C" fn(ctx: *mut c_void, col_idx: usize, val: u32),
    pub push_f64: unsafe extern "C" fn(ctx: *mut c_void, col_idx: usize, val: f64),
    pub push_bool: unsafe extern "C" fn(ctx: *mut c_void, col_idx: usize, val: u8),
    pub push_hw_tick: unsafe extern "C" fn(ctx: *mut c_void, tick: u64),
    pub finish_row: unsafe extern "C" fn(ctx: *mut c_void),
    pub record_framing_error: unsafe extern "C" fn(ctx: *mut c_void, count: u64),
}

/// Host callback sink used by C-ABI functions to return human-readable UTF-8 error strings.
#[repr(C)]
pub struct CErrorSink {
    pub ctx: *mut c_void,
    pub set_error: unsafe extern "C" fn(ctx: *mut c_void, msg: CByteSlice),
}

/// Exported vtable returned by `extern "C" fn ddscope_protocol_plugin_v1()`.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct ProtocolPluginVTableV1 {
    pub abi_version: u32,
    pub plugin_id: CByteSlice,
    pub display_name: CByteSlice,
    pub create_instance:
        unsafe extern "C" fn(config_json: CByteSlice, err: *const CErrorSink) -> *mut c_void,
    pub destroy_instance: unsafe extern "C" fn(instance: *mut c_void),
    pub reset_instance: unsafe extern "C" fn(instance: *mut c_void),
    pub declare_channels: unsafe extern "C" fn(instance: *mut c_void, sink: *const CChannelSink),
    /// Decodes complete frames from `input`, writing decoded samples via `sink`.
    ///
    /// Returns `>= 0` indicating the number of leading bytes consumed from `input`
    /// (any remaining `input[consumed..]` bytes are buffered by the host as frame carry).
    /// Returns `< 0` on a fatal codec error after writing an error message to `err`.
    pub decode_chunk: unsafe extern "C" fn(
        instance: *mut c_void,
        input: CByteSlice,
        is_eof: u8,
        sink: *const CBatchSink,
        err: *const CErrorSink,
    ) -> isize,
}

/// Entry-point function signature expected for symbol [`DDSCOPE_PROTOCOL_SYMBOL_V1`].
pub type ProtocolPluginEntryFnV1 = unsafe extern "C" fn() -> ProtocolPluginVTableV1;
