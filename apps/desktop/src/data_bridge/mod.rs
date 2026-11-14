//! Zero-copy data plane bridge (`TD2`: Windows WebView2 SharedBuffer / Tauri Raw Binary IPC).

#[cfg(windows)]
pub mod shm_windows;

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use ddscope_core::ddsf::{DDSF_HEADER_BYTES, DDSF_MAGIC};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::ipc::{Channel, InvokeResponseBody, IpcResponse};

use crate::commands::dto::ApiErrorDto;

/// Number of ring-buffer slots allocated for `CoreWebView2SharedBuffer` zero-copy frames.
pub const DDSF_SHM_SLOT_COUNT: usize = 3;

/// Fixed byte capacity per shared-memory ring slot (`8 MiB`, total `24 MiB` triple-buffered).
pub const DDSF_SHM_SLOT_BYTES: usize = 8 * 1024 * 1024;

/// Protocol identifier stamped into `PostSharedBufferToScript` JSON metadata.
pub const DDSF_SHM_PROTOCOL_TAG: &str = "ddscope-ddsf-shm-v1";

/// Minimum supported data-plane frame push rate (`Hz`).
pub const MIN_DATA_BRIDGE_FPS: u32 = 15;

/// Default data-plane frame push rate (`Hz`).
pub const DEFAULT_DATA_BRIDGE_FPS: u32 = 120;

/// Maximum supported data-plane frame push rate (`Hz`).
pub const MAX_DATA_BRIDGE_FPS: u32 = 240;

/// Active high-speed data plane transport selected by the host platform.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum DataBridgeModeDto {
    WebView2SharedBuffer,
    RawIpcChannel,
}

/// Lightweight slot-ready notification sent to the frontend after writing a `DDSF v1` frame.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DdsfSlotReadyDto {
    pub slot: u8,
    pub byte_len: u32,
    pub frame_seq: u32,
}

/// Handshake descriptor returned by `attach_data_bridge`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DataBridgeAttachDto {
    pub active_mode: DataBridgeModeDto,
    pub slot_count: u8,
    pub slot_bytes: u32,
}

/// Telemetry counters reported by `get_data_bridge_stats`.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
pub struct DataBridgeStatsDto {
    pub active_mode: Option<DataBridgeModeDto>,
    pub frames_pushed: f64,
    pub bytes_pushed: f64,
    pub shm_frames_pushed: f64,
    pub raw_ipc_frames_pushed: f64,
    pub oversized_fallbacks: u32,
    pub send_errors: u32,
    pub last_frame_seq: u32,
    pub target_fps: u32,
}

/// Binary `DDSF v1` frame payload sent over `tauri::ipc::Channel` as `InvokeResponseBody::Raw`,
/// completely bypassing JSON array serialization.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawDdsfFrame(pub Vec<u8>);

impl IpcResponse for RawDdsfFrame {
    fn body(self) -> tauri::Result<InvokeResponseBody> {
        Ok(InvokeResponseBody::Raw(self.0))
    }
}

impl Type for RawDdsfFrame {
    fn definition(types: &mut specta::Types) -> specta::datatype::DataType {
        <Vec<u8> as Type>::definition(types)
    }
}

/// Errors returned by [`ZeroCopyDataBridge`] and platform [`FrameSink`] implementations.
#[derive(Debug, thiserror::Error)]
pub enum DataBridgeError {
    #[error("data bridge sink is not attached")]
    NotAttached,
    #[error("invalid DDSF v1 frame: {0}")]
    InvalidDdsfFrame(String),
    #[error("frame size ({byte_len} bytes) exceeds SHM slot capacity ({slot_capacity} bytes)")]
    FrameTooLarge {
        byte_len: usize,
        slot_capacity: usize,
    },
    #[error("failed to deliver frame notification/payload over Tauri IPC channel: {0}")]
    IpcSend(String),
    #[error("platform shared-memory bridge error: {0}")]
    PlatformShm(String),
}

impl From<DataBridgeError> for ApiErrorDto {
    fn from(err: DataBridgeError) -> Self {
        let code = match &err {
            DataBridgeError::NotAttached => "BRIDGE_NOT_ATTACHED",
            DataBridgeError::InvalidDdsfFrame(_) => "INVALID_DDSF_FRAME",
            DataBridgeError::FrameTooLarge { .. } => "FRAME_TOO_LARGE",
            DataBridgeError::IpcSend(_) => "IPC_SEND_ERROR",
            DataBridgeError::PlatformShm(_) => "PLATFORM_SHM_ERROR",
        };
        Self {
            code: code.to_string(),
            message: err.to_string(),
        }
    }
}

/// Unified platform data-plane sink abstraction (`WebView2SharedBuffer` or `RawIpcChannel`).
pub trait FrameSink: Send + Sync {
    /// Primary transport mode advertised by this sink.
    fn mode(&self) -> DataBridgeModeDto;

    /// Publishes one validated `DDSF v1` binary frame and returns the actual transport mode used
    /// (for example `RawIpcChannel` if an oversized frame triggered per-frame fallback).
    fn push_ddsf_frame(
        &self,
        frame_bytes: &[u8],
        frame_seq: u32,
    ) -> Result<DataBridgeModeDto, DataBridgeError>;
}

/// Cross-platform fallback sink pushing raw binary `Vec<u8>` bytes via Tauri v2 `Channel`.
pub struct RawIpcFrameSink {
    channel: Channel<RawDdsfFrame>,
}

impl std::fmt::Debug for RawIpcFrameSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawIpcFrameSink")
            .field("channel_id", &self.channel.id())
            .finish()
    }
}

impl RawIpcFrameSink {
    /// Wraps a `tauri::ipc::Channel<RawDdsfFrame>` for raw binary frame streaming.
    pub fn new(channel: Channel<RawDdsfFrame>) -> Self {
        Self { channel }
    }
}

impl FrameSink for RawIpcFrameSink {
    fn mode(&self) -> DataBridgeModeDto {
        DataBridgeModeDto::RawIpcChannel
    }

    fn push_ddsf_frame(
        &self,
        frame_bytes: &[u8],
        _frame_seq: u32,
    ) -> Result<DataBridgeModeDto, DataBridgeError> {
        self.channel
            .send(RawDdsfFrame(frame_bytes.to_vec()))
            .map_err(|e| DataBridgeError::IpcSend(e.to_string()))?;
        Ok(DataBridgeModeDto::RawIpcChannel)
    }
}

/// Host-side coordinator managing the active [`FrameSink`], frame header checks, and atomic stats.
pub struct ZeroCopyDataBridge {
    sink: RwLock<Option<Arc<dyn FrameSink>>>,
    frames_pushed: AtomicU64,
    bytes_pushed: AtomicU64,
    shm_frames_pushed: AtomicU64,
    raw_ipc_frames_pushed: AtomicU64,
    oversized_fallbacks: AtomicU64,
    send_errors: AtomicU64,
    last_frame_seq: AtomicU32,
    target_fps: AtomicU32,
}

impl std::fmt::Debug for ZeroCopyDataBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZeroCopyDataBridge")
            .field("active_mode", &self.active_mode())
            .field("frames_pushed", &self.frames_pushed.load(Ordering::Relaxed))
            .field("target_fps", &self.target_fps())
            .finish()
    }
}

impl Default for ZeroCopyDataBridge {
    fn default() -> Self {
        Self::new()
    }
}

impl ZeroCopyDataBridge {
    /// Creates a detached [`ZeroCopyDataBridge`] initialized at `120 Hz` target rate.
    pub fn new() -> Self {
        Self {
            sink: RwLock::new(None),
            frames_pushed: AtomicU64::new(0),
            bytes_pushed: AtomicU64::new(0),
            shm_frames_pushed: AtomicU64::new(0),
            raw_ipc_frames_pushed: AtomicU64::new(0),
            oversized_fallbacks: AtomicU64::new(0),
            send_errors: AtomicU64::new(0),
            last_frame_seq: AtomicU32::new(0),
            target_fps: AtomicU32::new(DEFAULT_DATA_BRIDGE_FPS),
        }
    }

    /// Installs (or replaces) the active [`FrameSink`].
    pub fn attach_sink(&self, sink: Arc<dyn FrameSink>) {
        *self.sink.write() = Some(sink);
    }

    /// Detaches any active [`FrameSink`].
    pub fn detach(&self) {
        *self.sink.write() = None;
    }

    /// Returns `true` if a [`FrameSink`] is currently attached.
    #[inline]
    pub fn is_attached(&self) -> bool {
        self.sink.read().is_some()
    }

    /// Returns the primary transport mode of the currently attached sink, if any.
    pub fn active_mode(&self) -> Option<DataBridgeModeDto> {
        self.sink.read().as_ref().map(|s| s.mode())
    }

    /// Clamps and updates the target frame emission rate (`15..=240 Hz`).
    pub fn set_target_fps(&self, fps: u32) -> u32 {
        let clamped = fps.clamp(MIN_DATA_BRIDGE_FPS, MAX_DATA_BRIDGE_FPS);
        self.target_fps.store(clamped, Ordering::Release);
        clamped
    }

    /// Returns the configured target frame emission rate (`Hz`).
    #[inline]
    pub fn target_fps(&self) -> u32 {
        self.target_fps.load(Ordering::Acquire)
    }

    /// Returns a lock-free snapshot of data-plane bridge telemetry.
    pub fn stats_dto(&self) -> DataBridgeStatsDto {
        DataBridgeStatsDto {
            active_mode: self.active_mode(),
            frames_pushed: self.frames_pushed.load(Ordering::Relaxed) as f64,
            bytes_pushed: self.bytes_pushed.load(Ordering::Relaxed) as f64,
            shm_frames_pushed: self.shm_frames_pushed.load(Ordering::Relaxed) as f64,
            raw_ipc_frames_pushed: self.raw_ipc_frames_pushed.load(Ordering::Relaxed) as f64,
            oversized_fallbacks: self
                .oversized_fallbacks
                .load(Ordering::Relaxed)
                .min(u32::MAX as u64) as u32,
            send_errors: self.send_errors.load(Ordering::Relaxed).min(u32::MAX as u64) as u32,
            last_frame_seq: self.last_frame_seq.load(Ordering::Relaxed),
            target_fps: self.target_fps(),
        }
    }

    /// Validates `frame_bytes` as `DDSF v1`, pushes it into the active [`FrameSink`], and records
    /// delivery counters.
    pub fn push_frame(&self, frame_bytes: &[u8]) -> Result<DataBridgeModeDto, DataBridgeError> {
        let frame_seq = validate_ddsf_header_seq(frame_bytes)?;
        let sink = self
            .sink
            .read()
            .as_ref()
            .cloned()
            .ok_or(DataBridgeError::NotAttached)?;

        let primary_mode = sink.mode();
        match sink.push_ddsf_frame(frame_bytes, frame_seq) {
            Ok(actual_mode) => {
                self.frames_pushed.fetch_add(1, Ordering::Relaxed);
                self.bytes_pushed
                    .fetch_add(frame_bytes.len() as u64, Ordering::Relaxed);
                self.last_frame_seq.store(frame_seq, Ordering::Relaxed);

                match actual_mode {
                    DataBridgeModeDto::WebView2SharedBuffer => {
                        self.shm_frames_pushed.fetch_add(1, Ordering::Relaxed);
                    }
                    DataBridgeModeDto::RawIpcChannel => {
                        self.raw_ipc_frames_pushed.fetch_add(1, Ordering::Relaxed);
                        if primary_mode == DataBridgeModeDto::WebView2SharedBuffer {
                            self.oversized_fallbacks.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                Ok(actual_mode)
            }
            Err(err) => {
                self.send_errors.fetch_add(1, Ordering::Relaxed);
                Err(err)
            }
        }
    }
}

/// Validates minimum length, `DDSF_MAGIC`, and extracts little-endian `frame_seq` from the 64B header.
#[inline]
pub fn validate_ddsf_header_seq(frame_bytes: &[u8]) -> Result<u32, DataBridgeError> {
    if frame_bytes.len() < DDSF_HEADER_BYTES {
        return Err(DataBridgeError::InvalidDdsfFrame(format!(
            "frame length {} < DDSF_HEADER_BYTES ({})",
            frame_bytes.len(),
            DDSF_HEADER_BYTES
        )));
    }
    let magic = u32::from_le_bytes([
        frame_bytes[0],
        frame_bytes[1],
        frame_bytes[2],
        frame_bytes[3],
    ]);
    if magic != DDSF_MAGIC {
        return Err(DataBridgeError::InvalidDdsfFrame(format!(
            "invalid magic 0x{magic:08X}, expected 0x{DDSF_MAGIC:08X}"
        )));
    }
    let declared_total_len = u64::from_le_bytes([
        frame_bytes[24],
        frame_bytes[25],
        frame_bytes[26],
        frame_bytes[27],
        frame_bytes[28],
        frame_bytes[29],
        frame_bytes[30],
        frame_bytes[31],
    ]);
    if declared_total_len < DDSF_HEADER_BYTES as u64
        || declared_total_len > frame_bytes.len() as u64
    {
        return Err(DataBridgeError::InvalidDdsfFrame(format!(
            "invalid declared total_byte_len ({declared_total_len}) for slice length {}",
            frame_bytes.len()
        )));
    }
    Ok(u32::from_le_bytes([
        frame_bytes[8],
        frame_bytes[9],
        frame_bytes[10],
        frame_bytes[11],
    ]))
}
