//! Zero-copy data plane bridge contracts (WebView2 SharedBuffer / Tauri Raw Binary IPC).

use serde::{Deserialize, Serialize};
use specta::Type;

/// Number of ring-buffer slots allocated for `CoreWebView2SharedBuffer` zero-copy frames.
pub const DDSF_SHM_SLOT_COUNT: usize = 3;

/// Fixed byte capacity per shared-memory ring slot (`8 MiB`, total `24 MiB` triple-buffered).
pub const DDSF_SHM_SLOT_BYTES: usize = 8 * 1024 * 1024;

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
