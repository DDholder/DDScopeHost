//! Non-blocking bypass raw byte recorder (`.ddraw` v1 and raw `.bin`) implementing [`RawByteTap`].

use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use bytes::{Bytes, BytesMut};
use crossbeam_channel::{Receiver, Sender, TrySendError};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use super::scheduler::{host_monotonic_ns, RawByteTap, RawTapDirection};
use crate::types::{CoreError, CoreResult, TimestampNs};

/// Magic bytes identifying a `.ddraw` v1 capture file (`"DDRW"`).
pub const DDRAW_MAGIC: [u8; 4] = *b"DDRW";

/// Current `.ddraw` binary format version.
pub const DDRAW_VERSION: u16 = 1;

/// Fixed byte size of the `.ddraw` v1 file header.
pub const DDRAW_FILE_HEADER_BYTES: usize = 32;

/// Fixed byte size of each `.ddraw` v1 record header preceding its payload.
pub const DDRAW_RECORD_HEADER_BYTES: usize = 20;

/// Default bounded channel depth between the IO hot path and the background disk writer thread.
pub const DEFAULT_RECORDER_CHANNEL_CAPACITY: usize = 8_192;

/// Default `BufWriter` capacity (`256 KiB`) for sequential disk writes.
pub const DEFAULT_RECORDER_WRITER_BUFFER_BYTES: usize = 256 * 1024;

const fn default_channel_capacity() -> usize {
    DEFAULT_RECORDER_CHANNEL_CAPACITY
}

const fn default_writer_buffer_bytes() -> usize {
    DEFAULT_RECORDER_WRITER_BUFFER_BYTES
}

/// On-disk encoding format produced by [`RawByteRecorder`].
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RawRecordFormat {
    /// Structured `.ddraw` v1 container with per-record nanosecond timestamps, direction, and CRC32.
    #[default]
    Ddraw,
    /// Unframed raw RX payload byte stream (`.bin` capture).
    RawRxBin,
}

impl RawRecordFormat {
    /// Infers the capture format from `path`'s extension (`.bin` -> `RawRxBin`, otherwise `Ddraw`).
    pub fn infer_from_path(path: &Path) -> Self {
        match path.extension().and_then(|ext| ext.to_str()) {
            Some(ext) if ext.eq_ignore_ascii_case("bin") => Self::RawRxBin,
            _ => Self::Ddraw,
        }
    }
}

/// Configuration for creating a [`RawByteRecorder`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawRecorderConfig {
    pub output_path: PathBuf,
    /// Explicit capture format (`None` auto-selects `.bin` -> `RawRxBin`, otherwise `Ddraw`).
    #[serde(default)]
    pub format: Option<RawRecordFormat>,
    /// Bounded non-blocking channel capacity between IO callbacks and the writer thread.
    #[serde(default = "default_channel_capacity")]
    pub channel_capacity: usize,
    /// Size in bytes of the background `BufWriter` buffer.
    #[serde(default = "default_writer_buffer_bytes")]
    pub writer_buffer_bytes: usize,
}

impl RawRecorderConfig {
    /// Creates a recorder configuration targeting `output_path` with format auto-detection.
    pub fn new(output_path: impl Into<PathBuf>) -> Self {
        Self {
            output_path: output_path.into(),
            format: None,
            channel_capacity: DEFAULT_RECORDER_CHANNEL_CAPACITY,
            writer_buffer_bytes: DEFAULT_RECORDER_WRITER_BUFFER_BYTES,
        }
    }

    /// Overrides the recording format instead of inferring from file extension.
    pub fn with_format(mut self, format: RawRecordFormat) -> Self {
        self.format = Some(format);
        self
    }

    /// Overrides the bounded channel capacity.
    pub fn with_channel_capacity(mut self, capacity: usize) -> Self {
        self.channel_capacity = capacity;
        self
    }

    /// Overrides the writer buffer capacity in bytes.
    pub fn with_writer_buffer_bytes(mut self, bytes: usize) -> Self {
        self.writer_buffer_bytes = bytes;
        self
    }

    /// Resolves the effective [`RawRecordFormat`].
    pub fn resolved_format(&self) -> RawRecordFormat {
        self.format
            .unwrap_or_else(|| RawRecordFormat::infer_from_path(&self.output_path))
    }

    /// Validates configuration parameters before opening the output file.
    pub fn validate(&self) -> CoreResult<()> {
        if self.output_path.as_os_str().is_empty() {
            return Err(CoreError::InvalidConfig(
                "raw recorder output_path must not be empty".into(),
            ));
        }
        if self.channel_capacity == 0 {
            return Err(CoreError::InvalidConfig(
                "raw recorder channel_capacity must be >= 1".into(),
            ));
        }
        if self.writer_buffer_bytes == 0 {
            return Err(CoreError::InvalidConfig(
                "raw recorder writer_buffer_bytes must be >= 1".into(),
            ));
        }
        Ok(())
    }
}

/// Point-in-time counters reported by [`RawByteRecorder`].
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecorderStats {
    pub records_written: u64,
    pub records_dropped: u64,
    pub payload_bytes_written: u64,
    pub file_bytes_written: u64,
}

#[derive(Debug, Default)]
struct RecorderCounters {
    records_written: AtomicU64,
    records_dropped: AtomicU64,
    payload_bytes_written: AtomicU64,
    file_bytes_written: AtomicU64,
}

impl RecorderCounters {
    fn snapshot(&self) -> RecorderStats {
        RecorderStats {
            records_written: self.records_written.load(Ordering::Relaxed),
            records_dropped: self.records_dropped.load(Ordering::Relaxed),
            payload_bytes_written: self.payload_bytes_written.load(Ordering::Relaxed),
            file_bytes_written: self.file_bytes_written.load(Ordering::Relaxed),
        }
    }
}

enum RecorderCmd {
    Record {
        direction: RawTapDirection,
        timestamp_ns: TimestampNs,
        payload: Bytes,
    },
    Flush(Sender<CoreResult<()>>),
    Stop(Option<Sender<CoreResult<()>>>),
}

/// Non-blocking bypass raw byte capture recorder implementing [`RawByteTap`].
///
/// The IO hot path (`on_raw_bytes`) performs only an O(1) `Bytes` refcount clone and a
/// non-blocking `try_send` into a bounded channel drained by a dedicated background writer thread.
pub struct RawByteRecorder {
    output_path: PathBuf,
    format: RawRecordFormat,
    tx: Sender<RecorderCmd>,
    stopped: Arc<AtomicBool>,
    counters: Arc<RecorderCounters>,
    writer_error: Arc<Mutex<Option<String>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl std::fmt::Debug for RawByteRecorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawByteRecorder")
            .field("output_path", &self.output_path)
            .field("format", &self.format)
            .field("stopped", &self.is_stopped())
            .field("stats", &self.stats())
            .finish()
    }
}

impl RawByteRecorder {
    /// Opens `output_path` for recording using format auto-detection (`.bin` vs `.ddraw`).
    pub fn open(output_path: impl AsRef<Path>) -> CoreResult<Self> {
        Self::with_config(RawRecorderConfig::new(output_path.as_ref()))
    }

    /// Creates and starts a recorder using an explicit [`RawRecorderConfig`].
    pub fn with_config(config: RawRecorderConfig) -> CoreResult<Self> {
        config.validate()?;
        let format = config.resolved_format();

        if let Some(parent) = config.output_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&config.output_path)?;

        let mut writer = BufWriter::with_capacity(config.writer_buffer_bytes, file);
        let counters = Arc::new(RecorderCounters::default());

        if format == RawRecordFormat::Ddraw {
            let header = encode_ddraw_file_header(host_monotonic_ns());
            writer.write_all(&header)?;
            writer.flush()?;
            counters
                .file_bytes_written
                .fetch_add(DDRAW_FILE_HEADER_BYTES as u64, Ordering::Relaxed);
        }

        let (tx, rx) = crossbeam_channel::bounded(config.channel_capacity);
        let stopped = Arc::new(AtomicBool::new(false));
        let writer_error = Arc::new(Mutex::new(None));

        let thread_counters = Arc::clone(&counters);
        let thread_error = Arc::clone(&writer_error);
        let thread_stopped = Arc::clone(&stopped);
        let worker = std::thread::Builder::new()
            .name("ddscope-raw-recorder".to_string())
            .spawn(move || {
                writer_loop(
                    writer,
                    format,
                    rx,
                    thread_counters,
                    thread_error,
                    thread_stopped,
                );
            })
            .map_err(|err| CoreError::Io(std::io::Error::other(err)))?;

        Ok(Self {
            output_path: config.output_path,
            format,
            tx,
            stopped,
            counters,
            writer_error,
            worker: Mutex::new(Some(worker)),
        })
    }

    /// Returns the output file path for this recorder.
    #[inline]
    pub fn output_path(&self) -> &Path {
        &self.output_path
    }

    /// Returns the resolved [`RawRecordFormat`].
    #[inline]
    pub fn format(&self) -> RawRecordFormat {
        self.format
    }

    /// Returns `true` once recording has been stopped.
    #[inline]
    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }

    /// Returns a lock-free snapshot of recording statistics.
    #[inline]
    pub fn stats(&self) -> RecorderStats {
        self.counters.snapshot()
    }

    /// Blocks until all previously queued records have been written and flushed to the OS file.
    pub fn flush(&self) -> CoreResult<()> {
        if let Some(msg) = self.writer_error.lock().as_ref() {
            return Err(CoreError::Storage(msg.clone()));
        }
        if self.is_stopped() {
            // Ensure stop_and_flush has finished joining if another caller initiated it.
            let _guard = self.worker.lock();
            if let Some(msg) = self.writer_error.lock().as_ref() {
                return Err(CoreError::Storage(msg.clone()));
            }
            return Ok(());
        }

        let (ack_tx, ack_rx) = crossbeam_channel::bounded(1);
        if self.tx.send(RecorderCmd::Flush(ack_tx)).is_err() {
            if let Some(msg) = self.writer_error.lock().as_ref() {
                return Err(CoreError::Storage(msg.clone()));
            }
            return Ok(());
        }

        match ack_rx.recv() {
            Ok(res) => res,
            Err(_) => {
                if let Some(msg) = self.writer_error.lock().as_ref() {
                    Err(CoreError::Storage(msg.clone()))
                } else {
                    Ok(())
                }
            }
        }
    }

    /// Stops accepting new records, flushes all queued records to disk, joins the writer thread,
    /// and returns final [`RecorderStats`]. Idempotent if called multiple times.
    pub fn stop_and_flush(&self) -> CoreResult<RecorderStats> {
        self.stopped.store(true, Ordering::Release);
        let mut worker_guard = self.worker.lock();
        if let Some(handle) = worker_guard.take() {
            let (ack_tx, ack_rx) = crossbeam_channel::bounded(1);
            let sent = self.tx.send(RecorderCmd::Stop(Some(ack_tx))).is_ok();
            let ack_res = if sent {
                ack_rx.recv().unwrap_or(Ok(()))
            } else {
                Ok(())
            };
            let _ = handle.join();
            ack_res?;
        }
        if let Some(msg) = self.writer_error.lock().as_ref() {
            return Err(CoreError::Storage(msg.clone()));
        }
        Ok(self.stats())
    }
}

impl RawByteTap for RawByteRecorder {
    #[inline]
    fn on_raw_bytes(&self, direction: RawTapDirection, timestamp_ns: TimestampNs, bytes: &Bytes) {
        if self.stopped.load(Ordering::Acquire) {
            return;
        }
        if self.format == RawRecordFormat::RawRxBin && direction != RawTapDirection::Rx {
            return;
        }
        if bytes.len() > u32::MAX as usize {
            self.counters.records_dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let cmd = RecorderCmd::Record {
            direction,
            timestamp_ns,
            payload: bytes.clone(),
        };
        if let Err(err) = self.tx.try_send(cmd) {
            match err {
                TrySendError::Full(_) | TrySendError::Disconnected(_) => {
                    if !self.stopped.load(Ordering::Acquire) {
                        self.counters.records_dropped.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }
    }
}

impl Drop for RawByteRecorder {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        let handle_opt = self.worker.get_mut().take();
        if let Some(handle) = handle_opt {
            let _ = self.tx.send(RecorderCmd::Stop(None));
            let _ = handle.join();
        }
    }
}

fn encode_ddraw_file_header(created_mono_ns: u64) -> [u8; DDRAW_FILE_HEADER_BYTES] {
    let mut header = [0u8; DDRAW_FILE_HEADER_BYTES];
    header[0..4].copy_from_slice(&DDRAW_MAGIC);
    header[4..6].copy_from_slice(&DDRAW_VERSION.to_le_bytes());
    // [6..8] flags = 0
    header[8..16].copy_from_slice(&created_mono_ns.to_le_bytes());
    // [16..32] reserved = 0
    header
}

fn encode_ddraw_record_header(
    direction: RawTapDirection,
    timestamp_ns: TimestampNs,
    payload: &[u8],
) -> [u8; DDRAW_RECORD_HEADER_BYTES] {
    let mut hdr = [0u8; DDRAW_RECORD_HEADER_BYTES];
    let dir_byte: u8 = match direction {
        RawTapDirection::Rx => 0,
        RawTapDirection::Tx => 1,
    };
    let crc = crc32fast::hash(payload);
    hdr[0..8].copy_from_slice(&timestamp_ns.to_le_bytes());
    hdr[8..12].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    hdr[12] = dir_byte;
    // [13..16] reserved = 0
    hdr[16..20].copy_from_slice(&crc.to_le_bytes());
    hdr
}

fn record_writer_error(
    err: std::io::Error,
    failed: &mut bool,
    stopped: &AtomicBool,
    writer_error: &Mutex<Option<String>>,
) -> String {
    *failed = true;
    stopped.store(true, Ordering::Release);
    let msg = err.to_string();
    let mut guard = writer_error.lock();
    if guard.is_none() {
        *guard = Some(msg.clone());
    }
    msg
}

fn writer_loop(
    mut writer: BufWriter<File>,
    format: RawRecordFormat,
    rx: Receiver<RecorderCmd>,
    counters: Arc<RecorderCounters>,
    writer_error: Arc<Mutex<Option<String>>>,
    stopped: Arc<AtomicBool>,
) {
    let mut failed = false;

    while let Ok(cmd) = rx.recv() {
        match cmd {
            RecorderCmd::Record {
                direction,
                timestamp_ns,
                payload,
            } => {
                if failed {
                    counters.records_dropped.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                let write_res = match format {
                    RawRecordFormat::Ddraw => {
                        let rec_hdr = encode_ddraw_record_header(direction, timestamp_ns, &payload);
                        writer
                            .write_all(&rec_hdr)
                            .and_then(|()| writer.write_all(&payload))
                            .map(|()| (DDRAW_RECORD_HEADER_BYTES + payload.len()) as u64)
                    }
                    RawRecordFormat::RawRxBin => {
                        if direction == RawTapDirection::Rx {
                            writer.write_all(&payload).map(|()| payload.len() as u64)
                        } else {
                            Ok(0)
                        }
                    }
                };

                match write_res {
                    Ok(bytes_written) => {
                        counters.records_written.fetch_add(1, Ordering::Relaxed);
                        counters
                            .payload_bytes_written
                            .fetch_add(payload.len() as u64, Ordering::Relaxed);
                        counters
                            .file_bytes_written
                            .fetch_add(bytes_written, Ordering::Relaxed);
                    }
                    Err(err) => {
                        counters.records_dropped.fetch_add(1, Ordering::Relaxed);
                        let _ = record_writer_error(err, &mut failed, &stopped, &writer_error);
                    }
                }
            }
            RecorderCmd::Flush(ack_tx) => {
                let res = if failed {
                    Err(CoreError::Storage(
                        writer_error
                            .lock()
                            .clone()
                            .unwrap_or_else(|| "recorder writer failed".into()),
                    ))
                } else if let Err(err) = writer.flush() {
                    let msg = record_writer_error(err, &mut failed, &stopped, &writer_error);
                    Err(CoreError::Storage(msg))
                } else {
                    Ok(())
                };
                let _ = ack_tx.send(res);
            }
            RecorderCmd::Stop(ack_opt) => {
                let res = if failed {
                    Err(CoreError::Storage(
                        writer_error
                            .lock()
                            .clone()
                            .unwrap_or_else(|| "recorder writer failed".into()),
                    ))
                } else if let Err(err) = writer.flush() {
                    let msg = record_writer_error(err, &mut failed, &stopped, &writer_error);
                    Err(CoreError::Storage(msg))
                } else {
                    Ok(())
                };
                if let Some(ack_tx) = ack_opt {
                    let _ = ack_tx.send(res);
                }
                break;
            }
        }
    }

    if !failed {
        let _ = writer.flush();
    }
}

/// Parsed `.ddraw` v1 file header.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DdrawHeader {
    pub version: u16,
    pub flags: u16,
    pub created_mono_ns: u64,
}

/// Single decoded record yielded by [`DdrawReader`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DdrawRecord {
    pub direction: RawTapDirection,
    pub timestamp_ns: TimestampNs,
    pub payload: Bytes,
}

/// Streaming `.ddraw` v1 reader validating file header magic/version and per-record CRC32.
#[derive(Debug)]
pub struct DdrawReader<R: Read> {
    reader: R,
    header: DdrawHeader,
    finished: bool,
}

impl DdrawReader<BufReader<File>> {
    /// Opens a `.ddraw` file from disk and validates its 32-byte header.
    pub fn open(path: impl AsRef<Path>) -> CoreResult<Self> {
        let file = File::open(path.as_ref())?;
        Self::from_reader(BufReader::new(file))
    }
}

impl<R: Read> DdrawReader<R> {
    /// Wraps an arbitrary `Read` stream and validates the 32-byte `.ddraw` header.
    pub fn from_reader(mut reader: R) -> CoreResult<Self> {
        let mut buf = [0u8; DDRAW_FILE_HEADER_BYTES];
        reader.read_exact(&mut buf).map_err(|err| {
            if err.kind() == std::io::ErrorKind::UnexpectedEof {
                CoreError::Storage("truncated .ddraw file header".into())
            } else {
                CoreError::Io(err)
            }
        })?;

        if buf[0..4] != DDRAW_MAGIC {
            return Err(CoreError::Storage(format!(
                "invalid .ddraw magic: expected {:?}, got {:?}",
                DDRAW_MAGIC,
                &buf[0..4]
            )));
        }

        let version = u16::from_le_bytes([buf[4], buf[5]]);
        if version != DDRAW_VERSION {
            return Err(CoreError::Storage(format!(
                "unsupported .ddraw version: {version}"
            )));
        }
        let flags = u16::from_le_bytes([buf[6], buf[7]]);
        let created_mono_ns = u64::from_le_bytes([
            buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14], buf[15],
        ]);

        Ok(Self {
            reader,
            header: DdrawHeader {
                version,
                flags,
                created_mono_ns,
            },
            finished: false,
        })
    }

    /// Returns the validated 32-byte `.ddraw` file header.
    #[inline]
    pub fn header(&self) -> DdrawHeader {
        self.header
    }
}

fn read_exact_or_clean_eof<R: Read>(reader: &mut R, dst: &mut [u8]) -> CoreResult<bool> {
    let mut offset = 0;
    while offset < dst.len() {
        match reader.read(&mut dst[offset..]) {
            Ok(0) => {
                if offset == 0 {
                    return Ok(false);
                }
                return Err(CoreError::Storage(
                    "truncated .ddraw record header at EOF".into(),
                ));
            }
            Ok(n) => offset += n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(CoreError::Io(err)),
        }
    }
    Ok(true)
}

impl<R: Read> Iterator for DdrawReader<R> {
    type Item = CoreResult<DdrawRecord>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }

        let mut hdr = [0u8; DDRAW_RECORD_HEADER_BYTES];
        match read_exact_or_clean_eof(&mut self.reader, &mut hdr) {
            Ok(true) => {}
            Ok(false) => {
                self.finished = true;
                return None;
            }
            Err(err) => {
                self.finished = true;
                return Some(Err(err));
            }
        }

        let timestamp_ns = u64::from_le_bytes([
            hdr[0], hdr[1], hdr[2], hdr[3], hdr[4], hdr[5], hdr[6], hdr[7],
        ]);
        let byte_len = u32::from_le_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]) as usize;
        let direction = match hdr[12] {
            0 => RawTapDirection::Rx,
            1 => RawTapDirection::Tx,
            other => {
                self.finished = true;
                return Some(Err(CoreError::Storage(format!(
                    "invalid .ddraw direction tag: {other}"
                ))));
            }
        };
        let expected_crc = u32::from_le_bytes([hdr[16], hdr[17], hdr[18], hdr[19]]);

        let mut payload = BytesMut::zeroed(byte_len);
        if let Err(err) = self.reader.read_exact(&mut payload) {
            self.finished = true;
            let mapped = if err.kind() == std::io::ErrorKind::UnexpectedEof {
                CoreError::Storage("truncated .ddraw record payload at EOF".into())
            } else {
                CoreError::Io(err)
            };
            return Some(Err(mapped));
        }

        let actual_crc = crc32fast::hash(&payload);
        if actual_crc != expected_crc {
            self.finished = true;
            return Some(Err(CoreError::Storage(format!(
                "ddraw CRC32 mismatch: expected {expected_crc:#010x}, got {actual_crc:#010x}"
            ))));
        }

        Some(Ok(DdrawRecord {
            direction,
            timestamp_ns,
            payload: payload.freeze(),
        }))
    }
}
