//! Windows WebView2 `ICoreWebView2SharedBuffer` triple-buffered zero-copy ring (`TD2`).
//!
//! Safety & COM threading model:
//! - `ICoreWebView2SharedBuffer` is `!Send + !Sync` and must remain on the UI STA thread.
//!   We keep COM handles in a UI-thread `thread_local!` pool keyed by webview label so reloads
//!   reuse the existing `3 × 8 MiB` allocation and `WindowEvent::Destroyed` closes them cleanly.
//! - For background thread frame publishing, `ShmSlotMapping` duplicates the underlying Win32
//!   `FileMappingHandle` via `DuplicateHandle` + `MapViewOfFile(FILE_MAP_WRITE)`, giving the ring
//!   its own kernel-refcounted memory mapping independent of WebView teardown.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ptr::NonNull;
use std::sync::atomic::{fence, AtomicU32, Ordering};
use std::sync::Arc;

use ddscope_core::ddsf::DDSF_HEADER_BYTES;
use parking_lot::Mutex;
use tauri::ipc::Channel;
use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2Environment12, ICoreWebView2SharedBuffer, ICoreWebView2_17,
    COREWEBVIEW2_SHARED_BUFFER_ACCESS_READ_ONLY,
};
use windows_core::{Interface, PCWSTR};
use windows_sys::Win32::Foundation::{
    CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, UnmapViewOfFile, FILE_MAP_READ, FILE_MAP_WRITE,
    MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READWRITE,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

use super::{
    DataBridgeError, DataBridgeModeDto, DdsfSlotReadyDto, FrameSink, RawDdsfFrame,
    DDSF_SHM_PROTOCOL_TAG, DDSF_SHM_SLOT_BYTES, DDSF_SHM_SLOT_COUNT,
};

/// Read-only Win32 shared-memory mapping used by tests to mirror WebView2's JS `ArrayBuffer` view.
pub struct ReadOnlyShmView {
    ptr: NonNull<u8>,
    capacity: usize,
    mapping_handle: HANDLE,
    mapped_view: MEMORY_MAPPED_VIEW_ADDRESS,
}

// SAFETY: `ReadOnlyShmView` exposes only immutable read access over a kernel-refcounted mapping.
unsafe impl Send for ReadOnlyShmView {}
// SAFETY: Immutable shared reads across threads are safe (consumers validate magic/seqlock).
unsafe impl Sync for ReadOnlyShmView {}

impl std::fmt::Debug for ReadOnlyShmView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadOnlyShmView")
            .field("capacity", &self.capacity)
            .finish()
    }
}

impl ReadOnlyShmView {
    /// Returns the immutable slice backing this read-only view.
    #[inline]
    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: `ptr` points to `capacity` valid mapped bytes for the lifetime of `self`.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.capacity) }
    }
}

impl Drop for ReadOnlyShmView {
    fn drop(&mut self) {
        // SAFETY: Handles were created via `DuplicateHandle`/`MapViewOfFile`.
        unsafe {
            if !self.mapped_view.Value.is_null() {
                let _ = UnmapViewOfFile(self.mapped_view);
            }
            if !self.mapping_handle.is_null() {
                let _ = CloseHandle(self.mapping_handle);
            }
        }
    }
}

/// Send/Sync writable mapping of one `8 MiB` shared-memory slot.
pub struct ShmSlotMapping {
    ptr: NonNull<u8>,
    capacity: usize,
    mapping_handle: HANDLE,
    mapped_view: MEMORY_MAPPED_VIEW_ADDRESS,
}

// SAFETY: `ShmSlotMapping` owns a duplicated kernel file-mapping handle and virtual mapping whose
// lifetime is self-contained inside `self`, synchronized per slot by `ShmRingBuffer` mutexes.
unsafe impl Send for ShmSlotMapping {}
// SAFETY: Mutating access is exclusively serialized through `parking_lot::Mutex<ShmSlotMapping>`.
unsafe impl Sync for ShmSlotMapping {}

impl std::fmt::Debug for ShmSlotMapping {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShmSlotMapping")
            .field("capacity", &self.capacity)
            .field("has_owned_mapping", &(!self.mapping_handle.is_null()))
            .finish()
    }
}

impl ShmSlotMapping {
    /// Allocates an anonymous Win32 page-file-backed shared-memory slot (`capacity` bytes).
    /// Used primarily for headless tests and high-throughput benchmarks without a live WebView2 HWND.
    pub fn create_anonymous_os_shm(capacity: usize) -> Result<Self, DataBridgeError> {
        if capacity == 0 {
            return Err(DataBridgeError::PlatformShm(
                "shared buffer capacity must be non-zero".to_string(),
            ));
        }

        let size_hi = ((capacity as u64) >> 32) as u32;
        let size_lo = ((capacity as u64) & 0xFFFF_FFFF) as u32;

        // SAFETY: Passing `INVALID_HANDLE_VALUE` creates an anonymous page-file-backed section.
        let mapping_handle = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                std::ptr::null(),
                PAGE_READWRITE,
                size_hi,
                size_lo,
                std::ptr::null(),
            )
        };
        if mapping_handle.is_null() {
            return Err(DataBridgeError::PlatformShm(format!(
                "CreateFileMappingW({capacity}) failed: {}",
                std::io::Error::last_os_error()
            )));
        }

        // SAFETY: `mapping_handle` was created with `PAGE_READWRITE` and size `capacity`.
        let mapped_view = unsafe { MapViewOfFile(mapping_handle, FILE_MAP_WRITE, 0, 0, capacity) };
        if mapped_view.Value.is_null() {
            let err = std::io::Error::last_os_error();
            // SAFETY: Close `mapping_handle` on failure before returning.
            unsafe {
                let _ = CloseHandle(mapping_handle);
            }
            return Err(DataBridgeError::PlatformShm(format!(
                "MapViewOfFile(FILE_MAP_WRITE, {capacity}) failed: {err}"
            )));
        }

        let ptr = NonNull::new(mapped_view.Value.cast::<u8>()).ok_or_else(|| {
            DataBridgeError::PlatformShm("MapViewOfFile returned null view address".to_string())
        })?;

        Ok(Self {
            ptr,
            capacity,
            mapping_handle,
            mapped_view,
        })
    }

    /// Maps an `ICoreWebView2SharedBuffer` into a kernel-refcounted `Send + Sync` writable slot.
    ///
    /// # Safety
    ///
    /// Must be invoked on the UI STA thread owning `shared_buf`, and `shared_buf` must remain a
    /// valid live COM interface pointer for the duration of the call.
    pub unsafe fn from_webview2_shared_buffer(
        shared_buf: &ICoreWebView2SharedBuffer,
        expected_capacity: usize,
    ) -> Result<Self, DataBridgeError> {
        let mut reported_size: u64 = 0;
        // SAFETY: `shared_buf` is a valid STA COM object.
        unsafe { shared_buf.Size(&mut reported_size) }.map_err(|e| {
            DataBridgeError::PlatformShm(format!("ICoreWebView2SharedBuffer::Size failed: {e}"))
        })?;
        let capacity = (reported_size as usize).min(expected_capacity);
        if capacity < DDSF_HEADER_BYTES {
            return Err(DataBridgeError::PlatformShm(format!(
                "WebView2 SharedBuffer size {reported_size} is smaller than DDSF header"
            )));
        }

        let mut wv2_handle = windows::Win32::Foundation::HANDLE::default();
        let has_handle = unsafe { shared_buf.FileMappingHandle(&mut wv2_handle) }.is_ok()
            && !wv2_handle.0.is_null()
            && !std::ptr::eq(wv2_handle.0 as HANDLE, INVALID_HANDLE_VALUE);

        if has_handle {
            let current_proc = unsafe { GetCurrentProcess() };
            let mut dup_handle: HANDLE = std::ptr::null_mut();
            let dup_ok = unsafe {
                DuplicateHandle(
                    current_proc,
                    wv2_handle.0 as HANDLE,
                    current_proc,
                    &mut dup_handle,
                    0,
                    0,
                    DUPLICATE_SAME_ACCESS,
                )
            };
            if dup_ok != 0 && !dup_handle.is_null() {
                let mapped_view =
                    unsafe { MapViewOfFile(dup_handle, FILE_MAP_WRITE, 0, 0, capacity) };
                if !mapped_view.Value.is_null() {
                    if let Some(ptr) = NonNull::new(mapped_view.Value.cast::<u8>()) {
                        return Ok(Self {
                            ptr,
                            capacity,
                            mapping_handle: dup_handle,
                            mapped_view,
                        });
                    }
                    unsafe {
                        let _ = UnmapViewOfFile(mapped_view);
                    }
                }
                unsafe {
                    let _ = CloseHandle(dup_handle);
                }
            }
        }

        // Fallback when `FileMappingHandle` is unavailable: obtain raw pointer directly.
        // Lifetime remains bound by the STA `StaSharedBufferSet` pool.
        let mut raw_ptr: *mut u8 = std::ptr::null_mut();
        unsafe { shared_buf.Buffer(&mut raw_ptr) }.map_err(|e| {
            DataBridgeError::PlatformShm(format!("ICoreWebView2SharedBuffer::Buffer failed: {e}"))
        })?;
        let ptr = NonNull::new(raw_ptr).ok_or_else(|| {
            DataBridgeError::PlatformShm("WebView2 SharedBuffer returned null pointer".to_string())
        })?;
        Ok(Self {
            ptr,
            capacity,
            mapping_handle: std::ptr::null_mut(),
            mapped_view: MEMORY_MAPPED_VIEW_ADDRESS {
                Value: std::ptr::null_mut(),
            },
        })
    }

    /// Opens an independent read-only OS mapping to this slot (used in tests to verify JS visibility).
    pub fn open_readonly_view(&self) -> Result<ReadOnlyShmView, DataBridgeError> {
        if self.mapping_handle.is_null() {
            return Err(DataBridgeError::PlatformShm(
                "cannot open duplicate read-only view without owned file mapping handle"
                    .to_string(),
            ));
        }

        let current_proc = unsafe { GetCurrentProcess() };
        let mut dup_handle: HANDLE = std::ptr::null_mut();
        let dup_ok = unsafe {
            DuplicateHandle(
                current_proc,
                self.mapping_handle,
                current_proc,
                &mut dup_handle,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        if dup_ok == 0 || dup_handle.is_null() {
            return Err(DataBridgeError::PlatformShm(format!(
                "DuplicateHandle failed: {}",
                std::io::Error::last_os_error()
            )));
        }

        let mapped_view =
            unsafe { MapViewOfFile(dup_handle, FILE_MAP_READ, 0, 0, self.capacity) };
        if mapped_view.Value.is_null() {
            let err = std::io::Error::last_os_error();
            unsafe {
                let _ = CloseHandle(dup_handle);
            }
            return Err(DataBridgeError::PlatformShm(format!(
                "MapViewOfFile(FILE_MAP_READ) failed: {err}"
            )));
        }

        let ptr = NonNull::new(mapped_view.Value.cast::<u8>()).ok_or_else(|| {
            DataBridgeError::PlatformShm("MapViewOfFile(READ) returned null pointer".to_string())
        })?;

        Ok(ReadOnlyShmView {
            ptr,
            capacity: self.capacity,
            mapping_handle: dup_handle,
            mapped_view,
        })
    }

    #[inline]
    fn capacity(&self) -> usize {
        self.capacity
    }

    #[inline]
    fn write_seqlocked_frame(&mut self, frame_bytes: &[u8]) {
        debug_assert!(frame_bytes.len() >= DDSF_HEADER_BYTES);
        debug_assert!(frame_bytes.len() <= self.capacity);

        let dst = self.ptr.as_ptr();
        let tail_len = frame_bytes.len() - 4;
        let magic_native =
            u32::from_ne_bytes([frame_bytes[0], frame_bytes[1], frame_bytes[2], frame_bytes[3]]);

        // 1. Invalidate magic word with Release ordering before mutating the slot payload so any
        //    lagging reader inspects `magic == 0` and safely drops the stale slot rather than parsing torn bytes.
        // SAFETY: `dst` has `self.capacity >= 64` writable bytes.
        unsafe { write_u32_release(dst, 0) };

        // 2. Copy payload + remainder of header (bytes `4..frame_bytes.len()`).
        // SAFETY: `src` and `dst + 4` are non-overlapping valid regions of length `tail_len`.
        unsafe {
            std::ptr::copy_nonoverlapping(frame_bytes.as_ptr().add(4), dst.add(4), tail_len);
        }

        // 3. Publish completed frame by restoring `DDSF_MAGIC` with Release ordering.
        // SAFETY: `dst` has `>= 4` writable bytes and `tail_len` bytes are already committed before this store.
        unsafe { write_u32_release(dst, magic_native) };
    }
}

impl Drop for ShmSlotMapping {
    fn drop(&mut self) {
        // SAFETY: Unmap and close only owned Win32 handles.
        unsafe {
            if !self.mapped_view.Value.is_null() {
                let _ = UnmapViewOfFile(self.mapped_view);
            }
            if !self.mapping_handle.is_null() {
                let _ = CloseHandle(self.mapping_handle);
            }
        }
    }
}

#[inline]
unsafe fn write_u32_release(ptr: *mut u8, val: u32) {
    if ((ptr as usize) & (std::mem::align_of::<AtomicU32>() - 1)) == 0 {
        // SAFETY: Caller verified non-null, writable, 4-byte aligned pointer.
        let atomic = unsafe { &*(ptr as *const AtomicU32) };
        atomic.store(val, Ordering::Release);
    } else {
        fence(Ordering::Release);
        // SAFETY: Caller guarantees 4 writable bytes at `ptr`.
        unsafe { std::ptr::write_unaligned(ptr.cast::<u32>(), val) };
        fence(Ordering::Release);
    }
}

/// Triple-buffered (`3 × 8 MiB`) lock-free/per-slot-serialized OS shared-memory ring buffer.
#[derive(Debug)]
pub struct ShmRingBuffer {
    slots: [Mutex<ShmSlotMapping>; DDSF_SHM_SLOT_COUNT],
}

impl ShmRingBuffer {
    /// Wraps 3 pre-allocated [`ShmSlotMapping`] buffers (slots `0`, `1`, `2`).
    pub fn from_slots(slots: [ShmSlotMapping; DDSF_SHM_SLOT_COUNT]) -> Self {
        let [s0, s1, s2] = slots;
        Self {
            slots: [Mutex::new(s0), Mutex::new(s1), Mutex::new(s2)],
        }
    }

    /// Allocates 3 anonymous `8 MiB` Win32 OS shared-memory slots for headless testing/benchmarking.
    pub fn new_anonymous_for_test() -> Result<Arc<Self>, DataBridgeError> {
        let s0 = ShmSlotMapping::create_anonymous_os_shm(DDSF_SHM_SLOT_BYTES)?;
        let s1 = ShmSlotMapping::create_anonymous_os_shm(DDSF_SHM_SLOT_BYTES)?;
        let s2 = ShmSlotMapping::create_anonymous_os_shm(DDSF_SHM_SLOT_BYTES)?;
        Ok(Arc::new(Self::from_slots([s0, s1, s2])))
    }

    /// Opens read-only views across all 3 slots for integration test assertions.
    pub fn open_readonly_views_for_test(
        &self,
    ) -> Result<[ReadOnlyShmView; DDSF_SHM_SLOT_COUNT], DataBridgeError> {
        let v0 = self.slots[0].lock().open_readonly_view()?;
        let v1 = self.slots[1].lock().open_readonly_view()?;
        let v2 = self.slots[2].lock().open_readonly_view()?;
        Ok([v0, v1, v2])
    }

    /// Writes `frame_bytes` into ring slot `(frame_seq as usize) % 3` and returns its notification DTO.
    pub fn write_ddsf_frame(
        &self,
        frame_bytes: &[u8],
        frame_seq: u32,
    ) -> Result<DdsfSlotReadyDto, DataBridgeError> {
        let slot_idx = (frame_seq as usize) % DDSF_SHM_SLOT_COUNT;
        let mut slot = self.slots[slot_idx].lock();
        let slot_cap = slot.capacity();
        if frame_bytes.len() > slot_cap {
            return Err(DataBridgeError::FrameTooLarge {
                byte_len: frame_bytes.len(),
                slot_capacity: slot_cap,
            });
        }
        let byte_len = u32::try_from(frame_bytes.len()).map_err(|_| {
            DataBridgeError::InvalidDdsfFrame("frame exceeds u32 byte length".to_string())
        })?;
        slot.write_seqlocked_frame(frame_bytes);
        Ok(DdsfSlotReadyDto {
            slot: slot_idx as u8,
            byte_len,
            frame_seq,
        })
    }
}

/// Primary Windows [`FrameSink`] writing into triple-buffered `SharedBuffer` and emitting small
/// `DdsfSlotReadyDto` notifications, with transparent automatic fallback to `RawIpcChannel` on
/// frames exceeding `8 MiB`.
pub struct WebView2SharedBufferSink {
    ring: Arc<ShmRingBuffer>,
    slot_notify_channel: Channel<DdsfSlotReadyDto>,
    oversized_raw_channel: Channel<RawDdsfFrame>,
}

impl std::fmt::Debug for WebView2SharedBufferSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebView2SharedBufferSink")
            .field("slot_notify_channel_id", &self.slot_notify_channel.id())
            .field("oversized_raw_channel_id", &self.oversized_raw_channel.id())
            .finish()
    }
}

impl WebView2SharedBufferSink {
    /// Creates a new [`WebView2SharedBufferSink`] over `ring` and two Tauri IPC channels.
    pub fn new(
        ring: Arc<ShmRingBuffer>,
        slot_notify_channel: Channel<DdsfSlotReadyDto>,
        oversized_raw_channel: Channel<RawDdsfFrame>,
    ) -> Self {
        Self {
            ring,
            slot_notify_channel,
            oversized_raw_channel,
        }
    }
}

impl FrameSink for WebView2SharedBufferSink {
    fn mode(&self) -> DataBridgeModeDto {
        DataBridgeModeDto::WebView2SharedBuffer
    }

    fn push_ddsf_frame(
        &self,
        frame_bytes: &[u8],
        frame_seq: u32,
    ) -> Result<DataBridgeModeDto, DataBridgeError> {
        match self.ring.write_ddsf_frame(frame_bytes, frame_seq) {
            Ok(ready) => {
                self.slot_notify_channel
                    .send(ready)
                    .map_err(|e| DataBridgeError::IpcSend(e.to_string()))?;
                Ok(DataBridgeModeDto::WebView2SharedBuffer)
            }
            Err(DataBridgeError::FrameTooLarge { .. }) => {
                // Automatic per-frame fallback for frames larger than 8 MiB.
                self.oversized_raw_channel
                    .send(RawDdsfFrame(frame_bytes.to_vec()))
                    .map_err(|e| DataBridgeError::IpcSend(e.to_string()))?;
                Ok(DataBridgeModeDto::RawIpcChannel)
            }
            Err(err) => Err(err),
        }
    }
}

struct StaSharedBufferSet {
    com_buffers: [ICoreWebView2SharedBuffer; DDSF_SHM_SLOT_COUNT],
    ring: Arc<ShmRingBuffer>,
}

thread_local! {
    static STA_SHARED_BUFFERS: RefCell<HashMap<String, StaSharedBufferSet>> =
        RefCell::new(HashMap::new());
}

fn encode_wide_null_terminated(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0u16)).collect()
}

fn post_slots_to_webview(
    wv2_17: &ICoreWebView2_17,
    com_buffers: &[ICoreWebView2SharedBuffer; DDSF_SHM_SLOT_COUNT],
) -> Result<(), DataBridgeError> {
    for (slot_idx, buf) in com_buffers.iter().enumerate() {
        let meta_json = format!(
            "{{\"protocol\":\"{DDSF_SHM_PROTOCOL_TAG}\",\"slot\":{slot_idx},\"capacityBytes\":{DDSF_SHM_SLOT_BYTES}}}"
        );
        let meta_wide = encode_wide_null_terminated(&meta_json);
        // SAFETY: Invoked on UI STA thread with valid `ICoreWebView2SharedBuffer` and null-terminated UTF-16 JSON.
        unsafe {
            wv2_17.PostSharedBufferToScript(
                buf,
                COREWEBVIEW2_SHARED_BUFFER_ACCESS_READ_ONLY,
                PCWSTR::from_raw(meta_wide.as_ptr()),
            )
        }
        .map_err(|e| {
            DataBridgeError::PlatformShm(format!(
                "PostSharedBufferToScript(slot={slot_idx}) failed: {e}"
            ))
        })?;
    }
    Ok(())
}

/// Allocates (if not already present) and posts `3 × 8 MiB` `ICoreWebView2SharedBuffer` slots
/// read-only to the WebView2 script context, returning the shared `Send + Sync` [`ShmRingBuffer`].
///
/// Must run inside `WebviewWindow::with_webview(...)` on the UI STA thread.
pub fn attach_or_repost_webview2_shm_on_sta(
    window_label: &str,
    platform_webview: tauri::webview::PlatformWebview,
) -> Result<Arc<ShmRingBuffer>, DataBridgeError> {
    // SAFETY: `platform_webview` was obtained from Tauri on the owning UI STA thread.
    let core_wv2 = unsafe { platform_webview.controller().CoreWebView2() }.map_err(|e| {
        DataBridgeError::PlatformShm(format!("CoreWebView2() failed: {e}"))
    })?;
    let wv2_17: ICoreWebView2_17 = core_wv2.cast().map_err(|e| {
        DataBridgeError::PlatformShm(format!(
            "ICoreWebView2_17 unsupported by current WebView2 Runtime: {e}"
        ))
    })?;

    STA_SHARED_BUFFERS.with(|cell| {
        let mut map = cell.borrow_mut();
        if let Some(existing) = map.get(window_label) {
            post_slots_to_webview(&wv2_17, &existing.com_buffers)?;
            return Ok(Arc::clone(&existing.ring));
        }

        let env12: ICoreWebView2Environment12 =
            platform_webview.environment().cast().map_err(|e| {
                DataBridgeError::PlatformShm(format!(
                    "ICoreWebView2Environment12 unsupported by current WebView2 Runtime: {e}"
                ))
            })?;

        let mut com_vec = Vec::with_capacity(DDSF_SHM_SLOT_COUNT);
        let mut mappings = Vec::with_capacity(DDSF_SHM_SLOT_COUNT);

        for slot_idx in 0..DDSF_SHM_SLOT_COUNT {
            // SAFETY: STA thread COM call allocating `8 MiB` shared buffer.
            let com_buf = unsafe { env12.CreateSharedBuffer(DDSF_SHM_SLOT_BYTES as u64) }
                .map_err(|e| {
                    DataBridgeError::PlatformShm(format!(
                        "CreateSharedBuffer(slot={slot_idx}, {DDSF_SHM_SLOT_BYTES} B) failed: {e}"
                    ))
                })?;
            // SAFETY: Called on STA thread owning `com_buf`.
            let mapping = unsafe {
                ShmSlotMapping::from_webview2_shared_buffer(&com_buf, DDSF_SHM_SLOT_BYTES)
            }?;
            com_vec.push(com_buf);
            mappings.push(mapping);
        }

        let com_buffers: [ICoreWebView2SharedBuffer; DDSF_SHM_SLOT_COUNT] =
            com_vec.try_into().map_err(|_| {
                DataBridgeError::PlatformShm("failed to collect COM shared buffer array".to_string())
            })?;
        let slot_mappings: [ShmSlotMapping; DDSF_SHM_SLOT_COUNT] =
            mappings.try_into().map_err(|_| {
                DataBridgeError::PlatformShm("failed to collect slot mapping array".to_string())
            })?;

        let ring = Arc::new(ShmRingBuffer::from_slots(slot_mappings));
        post_slots_to_webview(&wv2_17, &com_buffers)?;

        map.insert(
            window_label.to_string(),
            StaSharedBufferSet {
                com_buffers,
                ring: Arc::clone(&ring),
            },
        );
        Ok(ring)
    })
}

/// Closes and releases any STA COM `ICoreWebView2SharedBuffer` objects associated with `window_label`.
///
/// Must be called on the UI STA thread (e.g. from `on_window_event(WindowEvent::Destroyed)`).
pub fn release_webview2_shm_on_sta(window_label: &str) {
    STA_SHARED_BUFFERS.with(|cell| {
        if let Some(removed) = cell.borrow_mut().remove(window_label) {
            for buf in removed.com_buffers {
                // SAFETY: Closing `ICoreWebView2SharedBuffer` on owning STA thread.
                let _ = unsafe { buf.Close() };
            }
        }
    });
}
