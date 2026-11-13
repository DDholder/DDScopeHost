//! Zero-copy memory-mapped raw file replay transport (`FR-IO-04`).
//!
//! Supports both unframed raw `.bin`/`.log` byte streams and `.ddraw` v1 captures,
//! in either full-speed offline `mmap` mode or `0.1x..=20x` paced/seekable playback mode.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use crossbeam_channel::{SendTimeoutError, TrySendError};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use super::recorder::{
    DDRAW_FILE_HEADER_BYTES, DDRAW_MAGIC, DDRAW_RECORD_HEADER_BYTES, DDRAW_VERSION,
};
use super::scheduler::{host_monotonic_ns, TransportMetrics};
use super::{
    OutboundReceiver, PacketSender, RawPacket, ShutdownReceiver, Transport, TransportCaps,
    TransportKind,
};
use crate::types::{CoreError, CoreResult};

/// Default chunk size (`1 MiB`) used for zero-copy `mmap` offline full-speed emission.
pub const DEFAULT_REPLAY_CHUNK_BYTES: usize = 1024 * 1024;

/// Default nominal byte rate (`1 MB/s`) used in paced mode when `nominal_byte_rate` is `None`.
pub const DEFAULT_NOMINAL_BYTE_RATE: f64 = 1_000_000.0;

/// Minimum supported paced playback multiplier.
pub const MIN_PLAYBACK_SPEED: f64 = 0.1;

/// Maximum supported paced playback multiplier.
pub const MAX_PLAYBACK_SPEED: f64 = 20.0;

const PACED_MAX_SLICE_BYTES: usize = 64 * 1024;

const fn default_playback_speed() -> f64 {
    1.0
}

const fn default_chunk_bytes() -> usize {
    DEFAULT_REPLAY_CHUNK_BYTES
}

/// Safe RAII read-only OS memory-mapped file view (`CreateFileMappingW`/`MapViewOfFile` on
/// Windows, `mmap`/`munmap` on Unix).
///
/// Zero-length files are represented safely as an empty slice without calling OS mapping APIs.
pub struct ReadOnlyMmap {
    ptr: NonNull<u8>,
    len: usize,
    #[cfg(windows)]
    mapping: windows_sys::Win32::Foundation::HANDLE,
    #[cfg(windows)]
    view: windows_sys::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS,
}

impl std::fmt::Debug for ReadOnlyMmap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadOnlyMmap")
            .field("len", &self.len)
            .finish()
    }
}

// SAFETY: `ReadOnlyMmap` provides immutable read-only access to an OS page-backed mapping
// whose lifetime is tied to `self` and cleaned up on `Drop`.
unsafe impl Send for ReadOnlyMmap {}
// SAFETY: Shared immutable references to the read-only mapped bytes are thread-safe.
unsafe impl Sync for ReadOnlyMmap {}

impl ReadOnlyMmap {
    /// Opens `path` and maps its entire contents read-only into memory.
    pub fn open(path: &Path) -> CoreResult<Self> {
        let file = File::open(path)?;
        let meta_len = file.metadata()?.len();
        if meta_len == 0 {
            return Ok(Self::empty());
        }

        let len = usize::try_from(meta_len).map_err(|_| {
            CoreError::Storage(format!(
                "file size {meta_len} exceeds process virtual address space"
            ))
        })?;

        Self::map_non_empty(&file, len)
    }

    fn empty() -> Self {
        Self {
            ptr: NonNull::dangling(),
            len: 0,
            #[cfg(windows)]
            mapping: std::ptr::null_mut(),
            #[cfg(windows)]
            view: windows_sys::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS {
                Value: std::ptr::null_mut(),
            },
        }
    }

    #[cfg(windows)]
    fn map_non_empty(file: &File, len: usize) -> CoreResult<Self> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, MapViewOfFile, FILE_MAP_READ, PAGE_READONLY,
        };

        let raw_handle = file.as_raw_handle() as HANDLE;
        // SAFETY: `raw_handle` is a valid open read-only file handle with non-zero length.
        let mapping = unsafe {
            CreateFileMappingW(
                raw_handle,
                std::ptr::null(),
                PAGE_READONLY,
                0,
                0,
                std::ptr::null(),
            )
        };
        if mapping.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }

        // SAFETY: `mapping` is a valid file mapping object; mapping `len` bytes from offset 0.
        let view = unsafe { MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, len) };
        if view.Value.is_null() {
            let err = std::io::Error::last_os_error();
            // SAFETY: `mapping` was successfully created above and must be closed on failure.
            unsafe {
                CloseHandle(mapping);
            }
            return Err(err.into());
        }

        let ptr = NonNull::new(view.Value.cast::<u8>()).ok_or_else(|| {
            CoreError::Storage("MapViewOfFile returned null view address".into())
        })?;

        Ok(Self {
            ptr,
            len,
            mapping,
            view,
        })
    }

    #[cfg(unix)]
    fn map_non_empty(file: &File, len: usize) -> CoreResult<Self> {
        use std::os::unix::io::AsRawFd;

        let fd = file.as_raw_fd();
        // SAFETY: `fd` is a valid open file descriptor of length `len > 0` mapped `PROT_READ`.
        let mapped = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ,
                libc::MAP_PRIVATE,
                fd,
                0,
            )
        };
        if mapped == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error().into());
        }

        let ptr = NonNull::new(mapped.cast::<u8>())
            .ok_or_else(|| CoreError::Storage("mmap returned null pointer".into()))?;

        Ok(Self { ptr, len })
    }

    /// Returns the mapped file size in bytes.
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` if the mapped file is empty (`0` bytes).
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns an immutable slice over the mapped file contents.
    #[inline]
    pub fn as_slice(&self) -> &[u8] {
        if self.len == 0 {
            &[]
        } else {
            // SAFETY: `ptr` points to `len` valid immutable mapped bytes for `self`'s lifetime.
            unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
        }
    }
}

impl AsRef<[u8]> for ReadOnlyMmap {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl Drop for ReadOnlyMmap {
    fn drop(&mut self) {
        if self.len == 0 {
            return;
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::Foundation::CloseHandle;
            use windows_sys::Win32::System::Memory::UnmapViewOfFile;

            // SAFETY: `view` and `mapping` were created by `MapViewOfFile`/`CreateFileMappingW`.
            unsafe {
                if !self.view.Value.is_null() {
                    let _ = UnmapViewOfFile(self.view);
                }
                if !self.mapping.is_null() {
                    let _ = CloseHandle(self.mapping);
                }
            }
        }
        #[cfg(unix)]
        {
            // SAFETY: `ptr` and `len` came directly from a successful `libc::mmap` call.
            unsafe {
                let _ = libc::munmap(self.ptr.as_ptr().cast(), self.len);
            }
        }
    }
}

/// Configuration for [`RawFileReplayTransport`], compatible with `TransportConfigDto::RawFileReplay`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RawFileReplayConfig {
    pub file_path: PathBuf,
    #[serde(default)]
    pub full_speed_offline: bool,
    #[serde(default = "default_playback_speed")]
    pub playback_speed: f64,
    #[serde(default)]
    pub loop_playback: bool,
    #[serde(default)]
    pub nominal_byte_rate: Option<f64>,
    #[serde(default = "default_chunk_bytes")]
    pub chunk_bytes: usize,
}

impl RawFileReplayConfig {
    /// Creates an offline full-speed replay config with `1 MiB` zero-copy chunks.
    pub fn offline(file_path: impl Into<PathBuf>) -> Self {
        Self {
            file_path: file_path.into(),
            full_speed_offline: true,
            playback_speed: 1.0,
            loop_playback: false,
            nominal_byte_rate: None,
            chunk_bytes: DEFAULT_REPLAY_CHUNK_BYTES,
        }
    }

    /// Creates a paced replay config starting at `1.0x` playback speed.
    pub fn paced(file_path: impl Into<PathBuf>, nominal_byte_rate: Option<f64>) -> Self {
        Self {
            file_path: file_path.into(),
            full_speed_offline: false,
            playback_speed: 1.0,
            loop_playback: false,
            nominal_byte_rate,
            chunk_bytes: DEFAULT_REPLAY_CHUNK_BYTES,
        }
    }

    /// Validates replay configuration constraints.
    pub fn validate(&self) -> CoreResult<()> {
        if self.file_path.as_os_str().is_empty() {
            return Err(CoreError::InvalidConfig(
                "replay file_path must not be empty".into(),
            ));
        }
        if self.chunk_bytes == 0 {
            return Err(CoreError::InvalidConfig(
                "replay chunk_bytes must be >= 1".into(),
            ));
        }
        if !self.full_speed_offline {
            validate_playback_speed(self.playback_speed)?;
        }
        if let Some(rate) = self.nominal_byte_rate {
            if !rate.is_finite() || rate <= 0.0 {
                return Err(CoreError::InvalidConfig(format!(
                    "nominal_byte_rate must be finite and > 0, got {rate}"
                )));
            }
        }
        Ok(())
    }
}

fn validate_playback_speed(speed: f64) -> CoreResult<()> {
    if !speed.is_finite() || !(MIN_PLAYBACK_SPEED..=MAX_PLAYBACK_SPEED).contains(&speed) {
        return Err(CoreError::InvalidConfig(format!(
            "playback_speed must be within {MIN_PLAYBACK_SPEED}..={MAX_PLAYBACK_SPEED}, got {speed}"
        )));
    }
    Ok(())
}

#[derive(Debug)]
struct ReplayControlInner {
    paused: AtomicBool,
    loop_playback: AtomicBool,
    speed_bits: AtomicU64,
    progress_bits: AtomicU64,
    bytes_replayed: AtomicU64,
    total_bytes: AtomicU64,
    seek_epoch: AtomicU64,
    consumed_seek_epoch: AtomicU64,
    seek_state: Mutex<(u64, f64)>,
    notify: Notify,
}

/// Thread-safe live control handle for seeking, pausing, and adjusting playback speed/looping.
#[derive(Clone, Debug)]
pub struct ReplayControlHandle {
    inner: Arc<ReplayControlInner>,
}

impl ReplayControlHandle {
    fn new(playback_speed: f64, loop_playback: bool) -> Self {
        let initial_speed = if playback_speed.is_finite() && playback_speed > 0.0 {
            playback_speed
        } else {
            1.0
        };
        Self {
            inner: Arc::new(ReplayControlInner {
                paused: AtomicBool::new(false),
                loop_playback: AtomicBool::new(loop_playback),
                speed_bits: AtomicU64::new(initial_speed.to_bits()),
                progress_bits: AtomicU64::new(0.0_f64.to_bits()),
                bytes_replayed: AtomicU64::new(0),
                total_bytes: AtomicU64::new(0),
                seek_epoch: AtomicU64::new(0),
                consumed_seek_epoch: AtomicU64::new(0),
                seek_state: Mutex::new((0, 0.0)),
                notify: Notify::new(),
            }),
        }
    }

    /// Requests an immediate seek to `offset_ratio` (clamped to `0.0..=1.0`).
    pub fn seek(&self, offset_ratio: f64) {
        let clamped = if offset_ratio.is_nan() {
            0.0
        } else {
            offset_ratio.clamp(0.0, 1.0)
        };
        {
            let mut guard = self.inner.seek_state.lock();
            guard.0 = guard.0.wrapping_add(1);
            guard.1 = clamped;
            self.inner.seek_epoch.store(guard.0, Ordering::Release);
            self.inner
                .progress_bits
                .store(clamped.to_bits(), Ordering::Release);
        }
        self.inner.notify.notify_waiters();
    }

    /// Pauses or resumes paced playback.
    pub fn set_paused(&self, paused: bool) {
        self.inner.paused.store(paused, Ordering::Release);
        self.inner.notify.notify_waiters();
    }

    /// Returns `true` if playback is currently paused.
    #[inline]
    pub fn is_paused(&self) -> bool {
        self.inner.paused.load(Ordering::Acquire)
    }

    /// Updates the paced playback speed multiplier (`0.1..=20.0`).
    pub fn set_playback_speed(&self, speed: f64) -> CoreResult<()> {
        validate_playback_speed(speed)?;
        self.inner
            .speed_bits
            .store(speed.to_bits(), Ordering::Release);
        self.inner.notify.notify_waiters();
        Ok(())
    }

    /// Returns the active playback speed multiplier.
    #[inline]
    pub fn playback_speed(&self) -> f64 {
        f64::from_bits(self.inner.speed_bits.load(Ordering::Acquire))
    }

    /// Enables or disables automatic loop-back on EOF.
    pub fn set_loop_playback(&self, loop_playback: bool) {
        self.inner
            .loop_playback
            .store(loop_playback, Ordering::Release);
        self.inner.notify.notify_waiters();
    }

    /// Returns `true` if automatic looping is currently enabled.
    #[inline]
    pub fn loop_playback(&self) -> bool {
        self.inner.loop_playback.load(Ordering::Acquire)
    }

    /// Returns the current normalized replay progress in `0.0..=1.0`.
    #[inline]
    pub fn progress_ratio(&self) -> f64 {
        f64::from_bits(self.inner.progress_bits.load(Ordering::Acquire))
    }

    /// Returns the monotonically incrementing seek counter.
    #[inline]
    pub fn seek_epoch(&self) -> u64 {
        self.inner.seek_epoch.load(Ordering::Acquire)
    }

    /// Returns total RX payload bytes emitted so far across all loops/seeks.
    #[inline]
    pub fn bytes_replayed(&self) -> u64 {
        self.inner.bytes_replayed.load(Ordering::Relaxed)
    }

    /// Returns total replayable RX payload bytes in the loaded file (`0` before `start`).
    #[inline]
    pub fn total_bytes(&self) -> u64 {
        self.inner.total_bytes.load(Ordering::Acquire)
    }

    fn set_progress(&self, ratio: f64) {
        self.inner
            .progress_bits
            .store(ratio.clamp(0.0, 1.0).to_bits(), Ordering::Release);
    }

    fn set_total_bytes(&self, total: u64) {
        self.inner.total_bytes.store(total, Ordering::Release);
    }

    fn add_replayed_bytes(&self, bytes: u64) {
        self.inner
            .bytes_replayed
            .fetch_add(bytes, Ordering::Relaxed);
    }

    fn consumed_seek_epoch(&self) -> u64 {
        self.inner.consumed_seek_epoch.load(Ordering::Acquire)
    }

    fn take_seek_if_changed(&self, observed_epoch: &mut u64) -> Option<f64> {
        let current = self.inner.seek_epoch.load(Ordering::Acquire);
        if current == *observed_epoch {
            return None;
        }
        let guard = self.inner.seek_state.lock();
        *observed_epoch = guard.0;
        self.inner
            .consumed_seek_epoch
            .store(guard.0, Ordering::Release);
        Some(guard.1)
    }
}

#[derive(Copy, Clone, Debug)]
struct DdrawIndexEntry {
    offset: usize,
    len: usize,
    timestamp_ns: u64,
    cum_bytes: u64,
}

enum ReplaySource {
    Raw {
        data: Bytes,
    },
    Ddraw {
        data: Bytes,
        entries: Vec<DdrawIndexEntry>,
        total_payload_bytes: u64,
    },
}

impl ReplaySource {
    fn from_mmap(mmap: ReadOnlyMmap) -> CoreResult<Self> {
        if mmap.is_empty() {
            return Ok(Self::Raw { data: Bytes::new() });
        }

        let data = Bytes::from_owner(mmap);
        if data.len() >= DDRAW_FILE_HEADER_BYTES && data[0..4] == DDRAW_MAGIC {
            let version = u16::from_le_bytes([data[4], data[5]]);
            if version != DDRAW_VERSION {
                return Err(CoreError::Storage(format!(
                    "unsupported .ddraw version: {version}"
                )));
            }
            let (entries, total_payload_bytes) = index_ddraw_rx_records(&data)?;
            Ok(Self::Ddraw {
                data,
                entries,
                total_payload_bytes,
            })
        } else {
            Ok(Self::Raw { data })
        }
    }

    fn total_bytes(&self) -> u64 {
        match self {
            Self::Raw { data } => data.len() as u64,
            Self::Ddraw {
                total_payload_bytes,
                ..
            } => *total_payload_bytes,
        }
    }
}

fn index_ddraw_rx_records(data: &[u8]) -> CoreResult<(Vec<DdrawIndexEntry>, u64)> {
    let mut entries = Vec::new();
    let mut pos = DDRAW_FILE_HEADER_BYTES;
    let mut total_rx_bytes = 0_u64;

    while pos < data.len() {
        let remaining = data.len() - pos;
        if remaining < DDRAW_RECORD_HEADER_BYTES {
            return Err(CoreError::Storage(
                "truncated .ddraw record header at EOF".into(),
            ));
        }

        let hdr = &data[pos..pos + DDRAW_RECORD_HEADER_BYTES];
        let timestamp_ns = u64::from_le_bytes([
            hdr[0], hdr[1], hdr[2], hdr[3], hdr[4], hdr[5], hdr[6], hdr[7],
        ]);
        let byte_len = u32::from_le_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]) as usize;
        let direction = hdr[12];
        if direction > 1 {
            return Err(CoreError::Storage(format!(
                "invalid .ddraw direction tag: {direction}"
            )));
        }
        let expected_crc = u32::from_le_bytes([hdr[16], hdr[17], hdr[18], hdr[19]]);

        let payload_start = pos + DDRAW_RECORD_HEADER_BYTES;
        let payload_end = payload_start.checked_add(byte_len).ok_or_else(|| {
            CoreError::Storage(".ddraw record length overflow".into())
        })?;
        if payload_end > data.len() {
            return Err(CoreError::Storage(
                "truncated .ddraw record payload at EOF".into(),
            ));
        }

        let actual_crc = crc32fast::hash(&data[payload_start..payload_end]);
        if actual_crc != expected_crc {
            return Err(CoreError::Storage(format!(
                "ddraw CRC32 mismatch at offset {pos}: expected {expected_crc:#010x}, got {actual_crc:#010x}"
            )));
        }

        if direction == 0 {
            entries.push(DdrawIndexEntry {
                offset: payload_start,
                len: byte_len,
                timestamp_ns,
                cum_bytes: total_rx_bytes,
            });
            total_rx_bytes = total_rx_bytes.saturating_add(byte_len as u64);
        }

        pos = payload_end;
    }

    Ok((entries, total_rx_bytes))
}

/// Memory-mapped raw file replay transport supporting offline full-speed (`mmap`)
/// and `0.1x..=20x` paced/seekable playback for `.bin`/`.log` and `.ddraw` captures.
pub struct RawFileReplayTransport {
    config: RawFileReplayConfig,
    control: ReplayControlHandle,
    metrics: Arc<TransportMetrics>,
    source_tag: Option<Arc<str>>,
}

impl std::fmt::Debug for RawFileReplayTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawFileReplayTransport")
            .field("config", &self.config)
            .field("metrics", &self.metrics.snapshot())
            .finish()
    }
}

impl RawFileReplayTransport {
    /// Creates and validates a new [`RawFileReplayTransport`].
    pub fn new(config: RawFileReplayConfig) -> CoreResult<Self> {
        config.validate()?;
        let control = ReplayControlHandle::new(config.playback_speed, config.loop_playback);
        let source_tag = config
            .file_path
            .file_name()
            .and_then(|n| n.to_str())
            .map(Arc::from);
        Ok(Self {
            config,
            control,
            metrics: Arc::new(TransportMetrics::new()),
            source_tag,
        })
    }

    /// Returns the transport's immutable configuration.
    #[inline]
    pub fn config(&self) -> &RawFileReplayConfig {
        &self.config
    }

    /// Returns a thread-safe [`ReplayControlHandle`] for live seek/pause/speed/loop control.
    #[inline]
    pub fn control_handle(&self) -> ReplayControlHandle {
        self.control.clone()
    }

    /// Returns the shared atomic [`TransportMetrics`] handle.
    #[inline]
    pub fn metrics(&self) -> &Arc<TransportMetrics> {
        &self.metrics
    }
}

/// Factory function instantiating [`RawFileReplayTransport`] from JSON configuration.
pub fn create_replay_transport(json_config: &str) -> CoreResult<Box<dyn Transport>> {
    let config: RawFileReplayConfig = serde_json::from_str(json_config)
        .map_err(|err| CoreError::InvalidConfig(format!("invalid replay config JSON: {err}")))?;
    Ok(Box::new(RawFileReplayTransport::new(config)?))
}

impl Transport for RawFileReplayTransport {
    fn capabilities(&self) -> TransportCaps {
        TransportCaps {
            kind: TransportKind::RawFileReplay,
            supports_tx: false,
            supports_seek: true,
            supports_raw_record: false,
            supports_auto_reconnect: false,
        }
    }

    fn start(
        &mut self,
        rx_sink: PacketSender,
        tx_rx: OutboundReceiver,
        shutdown_rx: ShutdownReceiver,
    ) -> CoreResult<JoinHandle<CoreResult<()>>> {
        self.config.validate()?;
        let mmap = ReadOnlyMmap::open(&self.config.file_path)?;
        let source = ReplaySource::from_mmap(mmap)?;
        self.control.set_total_bytes(source.total_bytes());
        if self.control.seek_epoch() == self.control.consumed_seek_epoch() {
            self.control.set_progress(0.0);
        }

        let control = self.control.clone();
        let metrics = Arc::clone(&self.metrics);
        let source_tag = self.source_tag.clone();
        let chunk_bytes = self.config.chunk_bytes;
        let full_speed = self.config.full_speed_offline;
        let nominal_byte_rate = self.config.nominal_byte_rate;

        if full_speed {
            Ok(tokio::task::spawn_blocking(move || {
                let _tx_rx = tx_rx;
                run_offline_replay(
                    source,
                    rx_sink,
                    shutdown_rx,
                    control,
                    metrics,
                    source_tag,
                    chunk_bytes,
                )
            }))
        } else {
            Ok(tokio::spawn(async move {
                let _tx_rx = tx_rx;
                run_paced_replay(
                    source,
                    rx_sink,
                    shutdown_rx,
                    control,
                    metrics,
                    source_tag,
                    chunk_bytes,
                    nominal_byte_rate,
                )
                .await
            }))
        }
    }
}

fn emit_blocking_interruptible(
    rx_sink: &PacketSender,
    shutdown_rx: &ShutdownReceiver,
    metrics: &TransportMetrics,
    control: &ReplayControlHandle,
    mut packet: RawPacket,
) -> CoreResult<bool> {
    let byte_len = packet.bytes.len();
    match rx_sink.try_send(packet) {
        Ok(()) => {
            metrics.record_rx(byte_len);
            control.add_replayed_bytes(byte_len as u64);
            return Ok(true);
        }
        Err(TrySendError::Disconnected(_)) => return Err(CoreError::ChannelClosed),
        Err(TrySendError::Full(returned)) => {
            packet = returned;
        }
    }

    loop {
        if *shutdown_rx.borrow() {
            return Ok(false);
        }
        match rx_sink.send_timeout(packet, Duration::from_millis(5)) {
            Ok(()) => {
                metrics.record_rx(byte_len);
                control.add_replayed_bytes(byte_len as u64);
                return Ok(true);
            }
            Err(SendTimeoutError::Disconnected(_)) => return Err(CoreError::ChannelClosed),
            Err(SendTimeoutError::Timeout(returned)) => {
                packet = returned;
            }
        }
    }
}

async fn emit_async_interruptible(
    rx_sink: &PacketSender,
    shutdown_rx: &mut ShutdownReceiver,
    metrics: &TransportMetrics,
    control: &ReplayControlHandle,
    mut packet: RawPacket,
) -> CoreResult<bool> {
    let byte_len = packet.bytes.len();
    loop {
        match rx_sink.try_send(packet) {
            Ok(()) => {
                metrics.record_rx(byte_len);
                control.add_replayed_bytes(byte_len as u64);
                return Ok(true);
            }
            Err(TrySendError::Disconnected(_)) => return Err(CoreError::ChannelClosed),
            Err(TrySendError::Full(returned)) => {
                packet = returned;
                tokio::select! {
                    biased;
                    res = shutdown_rx.changed() => {
                        if res.is_err() || *shutdown_rx.borrow() {
                            return Ok(false);
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_millis(1)) => {}
                }
            }
        }
    }
}

fn seek_raw_offset(total_len: usize, ratio: f64) -> usize {
    if total_len == 0 {
        return 0;
    }
    ((total_len as f64) * ratio.clamp(0.0, 1.0)).round() as usize
}

fn seek_ddraw_index(entries: &[DdrawIndexEntry], total_payload_bytes: u64, ratio: f64) -> usize {
    if entries.is_empty() {
        return 0;
    }
    let ratio = ratio.clamp(0.0, 1.0);
    if ratio <= 0.0 {
        return 0;
    }
    if ratio >= 1.0 || total_payload_bytes == 0 {
        return entries.len();
    }
    let target_byte = ((total_payload_bytes as f64) * ratio).round() as u64;
    let pos = entries.partition_point(|entry| entry.cum_bytes < target_byte);
    pos.min(entries.len())
}

fn run_offline_replay(
    source: ReplaySource,
    rx_sink: PacketSender,
    shutdown_rx: ShutdownReceiver,
    control: ReplayControlHandle,
    metrics: Arc<TransportMetrics>,
    source_tag: Option<Arc<str>>,
    chunk_bytes: usize,
) -> CoreResult<()> {
    let mut observed_seek_epoch = control.consumed_seek_epoch();

    match source {
        ReplaySource::Raw { data } => {
            let total_len = data.len();
            if total_len == 0 {
                control.set_progress(1.0);
                return Ok(());
            }

            let mut offset = if let Some(ratio) = control.take_seek_if_changed(&mut observed_seek_epoch)
            {
                seek_raw_offset(total_len, ratio)
            } else {
                0
            };

            loop {
                if *shutdown_rx.borrow() {
                    break;
                }
                if let Some(ratio) = control.take_seek_if_changed(&mut observed_seek_epoch) {
                    offset = seek_raw_offset(total_len, ratio);
                }
                if offset >= total_len {
                    control.set_progress(1.0);
                    if control.loop_playback() {
                        offset = 0;
                        control.set_progress(0.0);
                        continue;
                    }
                    break;
                }

                let end = offset.saturating_add(chunk_bytes).min(total_len);
                let packet = RawPacket {
                    bytes: data.slice(offset..end),
                    host_recv_ns: host_monotonic_ns(),
                    source_tag: source_tag.clone(),
                };
                if !emit_blocking_interruptible(&rx_sink, &shutdown_rx, &metrics, &control, packet)?
                {
                    break;
                }
                offset = end;
                control.set_progress(offset as f64 / total_len as f64);
            }
        }
        ReplaySource::Ddraw {
            data,
            entries,
            total_payload_bytes,
        } => {
            if entries.is_empty() {
                control.set_progress(1.0);
                return Ok(());
            }

            let first_ts = entries[0].timestamp_ns;
            let last_ts = entries[entries.len() - 1].timestamp_ns;
            let loop_span_ns = last_ts.saturating_sub(first_ts).saturating_add(1);
            let mut loop_offset_ns = 0_u64;

            let mut idx = if let Some(ratio) = control.take_seek_if_changed(&mut observed_seek_epoch)
            {
                seek_ddraw_index(&entries, total_payload_bytes, ratio)
            } else {
                0
            };

            loop {
                if *shutdown_rx.borrow() {
                    break;
                }
                if let Some(ratio) = control.take_seek_if_changed(&mut observed_seek_epoch) {
                    idx = seek_ddraw_index(&entries, total_payload_bytes, ratio);
                }
                if idx >= entries.len() {
                    control.set_progress(1.0);
                    if control.loop_playback() {
                        idx = 0;
                        loop_offset_ns = loop_offset_ns.saturating_add(loop_span_ns);
                        control.set_progress(0.0);
                        continue;
                    }
                    break;
                }

                let entry = entries[idx];
                let packet = RawPacket {
                    bytes: data.slice(entry.offset..entry.offset + entry.len),
                    host_recv_ns: entry.timestamp_ns.saturating_add(loop_offset_ns),
                    source_tag: source_tag.clone(),
                };
                if !emit_blocking_interruptible(&rx_sink, &shutdown_rx, &metrics, &control, packet)?
                {
                    break;
                }
                idx += 1;
                let progress = if total_payload_bytes > 0 {
                    entry.cum_bytes.saturating_add(entry.len as u64) as f64
                        / total_payload_bytes as f64
                } else {
                    idx as f64 / entries.len() as f64
                };
                control.set_progress(progress);
            }
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_paced_replay(
    source: ReplaySource,
    rx_sink: PacketSender,
    mut shutdown_rx: ShutdownReceiver,
    control: ReplayControlHandle,
    metrics: Arc<TransportMetrics>,
    source_tag: Option<Arc<str>>,
    chunk_bytes: usize,
    nominal_byte_rate: Option<f64>,
) -> CoreResult<()> {
    let mut observed_seek_epoch = control.consumed_seek_epoch();
    let nominal_rate = nominal_byte_rate.unwrap_or(DEFAULT_NOMINAL_BYTE_RATE);

    match source {
        ReplaySource::Raw { data } => {
            let total_len = data.len();
            if total_len == 0 {
                control.set_progress(1.0);
                return Ok(());
            }

            let mut offset = if let Some(ratio) = control.take_seek_if_changed(&mut observed_seek_epoch)
            {
                seek_raw_offset(total_len, ratio)
            } else {
                0
            };

            let mut anchor_wall = Instant::now();
            let mut anchor_bytes: u64 = 0;
            let mut last_speed = control.playback_speed();

            loop {
                if *shutdown_rx.borrow() {
                    break;
                }

                if control.is_paused() {
                    wait_while_paused(&control, &mut shutdown_rx).await;
                    if *shutdown_rx.borrow() {
                        break;
                    }
                    if let Some(ratio) = control.take_seek_if_changed(&mut observed_seek_epoch) {
                        offset = seek_raw_offset(total_len, ratio);
                    }
                    anchor_wall = Instant::now();
                    anchor_bytes = 0;
                    last_speed = control.playback_speed();
                    continue;
                }

                if let Some(ratio) = control.take_seek_if_changed(&mut observed_seek_epoch) {
                    offset = seek_raw_offset(total_len, ratio);
                    anchor_wall = Instant::now();
                    anchor_bytes = 0;
                }

                if offset >= total_len {
                    control.set_progress(1.0);
                    if control.loop_playback() {
                        offset = 0;
                        control.set_progress(0.0);
                        anchor_wall = Instant::now();
                        anchor_bytes = 0;
                        continue;
                    }
                    break;
                }

                let speed = control.playback_speed();
                if (speed - last_speed).abs() > f64::EPSILON {
                    anchor_wall = Instant::now();
                    anchor_bytes = 0;
                    last_speed = speed;
                }

                let effective_rate = (nominal_rate * speed).max(1.0);
                let max_slice = chunk_bytes.clamp(1, PACED_MAX_SLICE_BYTES);
                let slice_len = ((effective_rate * 0.01).round() as usize).clamp(1, max_slice);
                let end = offset.saturating_add(slice_len).min(total_len);
                let emitted_len = end - offset;

                let packet = RawPacket {
                    bytes: data.slice(offset..end),
                    host_recv_ns: host_monotonic_ns(),
                    source_tag: source_tag.clone(),
                };
                if !emit_async_interruptible(
                    &rx_sink,
                    &mut shutdown_rx,
                    &metrics,
                    &control,
                    packet,
                )
                .await?
                {
                    break;
                }

                offset = end;
                control.set_progress(offset as f64 / total_len as f64);

                anchor_bytes = anchor_bytes.saturating_add(emitted_len as u64);
                let target_ns = ((anchor_bytes as f64 / effective_rate) * 1_000_000_000.0) as u64;
                let target_instant = anchor_wall + Duration::from_nanos(target_ns);

                if let PacedWaitOutcome::ControlChanged =
                    wait_until_or_interrupt(target_instant, &control, &mut shutdown_rx).await
                {
                    if let Some(ratio) = control.take_seek_if_changed(&mut observed_seek_epoch) {
                        offset = seek_raw_offset(total_len, ratio);
                    }
                    anchor_wall = Instant::now();
                    anchor_bytes = 0;
                    last_speed = control.playback_speed();
                }
            }
        }
        ReplaySource::Ddraw {
            data,
            entries,
            total_payload_bytes,
        } => {
            if entries.is_empty() {
                control.set_progress(1.0);
                return Ok(());
            }

            let mut idx = if let Some(ratio) = control.take_seek_if_changed(&mut observed_seek_epoch)
            {
                seek_ddraw_index(&entries, total_payload_bytes, ratio)
            } else {
                0
            };

            let mut anchor_wall = Instant::now();
            let mut anchor_file_ts = entries.get(idx).map_or(0, |e| e.timestamp_ns);
            let mut last_speed = control.playback_speed();

            loop {
                if *shutdown_rx.borrow() {
                    break;
                }

                if control.is_paused() {
                    wait_while_paused(&control, &mut shutdown_rx).await;
                    if *shutdown_rx.borrow() {
                        break;
                    }
                    if let Some(ratio) = control.take_seek_if_changed(&mut observed_seek_epoch) {
                        idx = seek_ddraw_index(&entries, total_payload_bytes, ratio);
                    }
                    anchor_wall = Instant::now();
                    anchor_file_ts = entries.get(idx).map_or(0, |e| e.timestamp_ns);
                    last_speed = control.playback_speed();
                    continue;
                }

                if let Some(ratio) = control.take_seek_if_changed(&mut observed_seek_epoch) {
                    idx = seek_ddraw_index(&entries, total_payload_bytes, ratio);
                    anchor_wall = Instant::now();
                    anchor_file_ts = entries.get(idx).map_or(0, |e| e.timestamp_ns);
                }

                if idx >= entries.len() {
                    control.set_progress(1.0);
                    if control.loop_playback() {
                        idx = 0;
                        control.set_progress(0.0);
                        anchor_wall = Instant::now();
                        anchor_file_ts = entries[0].timestamp_ns;
                        continue;
                    }
                    break;
                }

                let speed = control.playback_speed();
                if (speed - last_speed).abs() > f64::EPSILON {
                    anchor_wall = Instant::now();
                    anchor_file_ts = entries[idx].timestamp_ns;
                    last_speed = speed;
                }

                let entry = entries[idx];
                let packet = RawPacket {
                    bytes: data.slice(entry.offset..entry.offset + entry.len),
                    host_recv_ns: host_monotonic_ns(),
                    source_tag: source_tag.clone(),
                };
                if !emit_async_interruptible(
                    &rx_sink,
                    &mut shutdown_rx,
                    &metrics,
                    &control,
                    packet,
                )
                .await?
                {
                    break;
                }

                idx += 1;
                let progress = if total_payload_bytes > 0 {
                    entry.cum_bytes.saturating_add(entry.len as u64) as f64
                        / total_payload_bytes as f64
                } else {
                    idx as f64 / entries.len() as f64
                };
                control.set_progress(progress);

                if idx < entries.len() {
                    let delta_file_ns = entries[idx].timestamp_ns.saturating_sub(anchor_file_ts);
                    let wait_ns = if delta_file_ns > 0 {
                        ((delta_file_ns as f64) / speed).round() as u64
                    } else if let Some(rate) = nominal_byte_rate {
                        (((entry.len as f64) / (rate * speed)) * 1_000_000_000.0).round() as u64
                    } else {
                        0
                    };

                    if wait_ns > 0 {
                        let target_instant = anchor_wall + Duration::from_nanos(wait_ns);
                        if let PacedWaitOutcome::ControlChanged =
                            wait_until_or_interrupt(target_instant, &control, &mut shutdown_rx)
                                .await
                        {
                            if let Some(ratio) =
                                control.take_seek_if_changed(&mut observed_seek_epoch)
                            {
                                idx = seek_ddraw_index(&entries, total_payload_bytes, ratio);
                            }
                            anchor_wall = Instant::now();
                            anchor_file_ts = entries.get(idx).map_or(0, |e| e.timestamp_ns);
                            last_speed = control.playback_speed();
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

enum PacedWaitOutcome {
    Elapsed,
    Shutdown,
    ControlChanged,
}

async fn wait_while_paused(control: &ReplayControlHandle, shutdown_rx: &mut ShutdownReceiver) {
    while control.is_paused() && !*shutdown_rx.borrow() {
        let notified = control.inner.notify.notified();
        tokio::select! {
            biased;
            res = shutdown_rx.changed() => {
                if res.is_err() || *shutdown_rx.borrow() {
                    break;
                }
            }
            _ = notified => {}
        }
    }
}

async fn wait_until_or_interrupt(
    deadline: Instant,
    control: &ReplayControlHandle,
    shutdown_rx: &mut ShutdownReceiver,
) -> PacedWaitOutcome {
    let now = Instant::now();
    if deadline <= now {
        return PacedWaitOutcome::Elapsed;
    }

    let sleep = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline));
    let notified = control.inner.notify.notified();
    tokio::select! {
        biased;
        res = shutdown_rx.changed() => {
            if res.is_err() || *shutdown_rx.borrow() {
                PacedWaitOutcome::Shutdown
            } else {
                PacedWaitOutcome::ControlChanged
            }
        }
        _ = notified => PacedWaitOutcome::ControlChanged,
        _ = sleep => PacedWaitOutcome::Elapsed,
    }
}
