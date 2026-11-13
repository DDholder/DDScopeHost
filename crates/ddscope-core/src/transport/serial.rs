//! Asynchronous UART / serial port transport, OS port enumeration, and virtual loopback pair.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::{Buf, Bytes};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;

use super::buffer_pool::BufferPoolConfig;
use super::scheduler::{host_monotonic_ns, RxBackpressurePolicy, RxPacketEmitter};
use super::tx_queue::TxStreamConsumer;
use super::{
    OutboundReceiver, PacketSender, ShutdownReceiver, Transport, TransportCaps, TransportKind,
    TransportState,
};
use crate::types::{CoreError, CoreResult};

/// Maximum supported UART baud rate (`12 Mbps` for high-speed USB-CDC / FTDI HS adapters).
pub const MAX_SERIAL_BAUD_RATE: u32 = 12_000_000;

/// Default delay in milliseconds between hot-plug reconnect attempts.
pub const DEFAULT_SERIAL_RECONNECT_DELAY_MS: u64 = 500;

/// Default read chunk size per async serial read call.
const DEFAULT_SERIAL_READ_CHUNK_BYTES: usize = 8 * 1024;

fn default_reconnect_delay_ms() -> u64 {
    DEFAULT_SERIAL_RECONNECT_DELAY_MS
}

/// UART parity configuration matching `TransportConfigDto::Serial`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerialParity {
    #[default]
    None,
    Odd,
    Even,
}

/// UART stop-bit configuration matching `TransportConfigDto::Serial`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerialStopBits {
    #[default]
    One,
    Two,
}

/// UART flow-control configuration matching `TransportConfigDto::Serial`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerialFlowControl {
    #[default]
    None,
    HardwareRtsCts,
    SoftwareXonXoff,
}

/// Configuration for [`SerialTransport`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SerialConfig {
    pub port_name: String,
    pub baud_rate: u32,
    pub data_bits: u8,
    #[serde(default)]
    pub parity: SerialParity,
    #[serde(default)]
    pub stop_bits: SerialStopBits,
    #[serde(default)]
    pub flow_control: SerialFlowControl,
    #[serde(default)]
    pub dtr_on_open: bool,
    #[serde(default)]
    pub auto_reconnect: bool,
    #[serde(default = "default_reconnect_delay_ms")]
    pub reconnect_delay_ms: u64,
}

impl Default for SerialConfig {
    fn default() -> Self {
        Self {
            port_name: String::new(),
            baud_rate: 115_200,
            data_bits: 8,
            parity: SerialParity::None,
            stop_bits: SerialStopBits::One,
            flow_control: SerialFlowControl::None,
            dtr_on_open: false,
            auto_reconnect: false,
            reconnect_delay_ms: DEFAULT_SERIAL_RECONNECT_DELAY_MS,
        }
    }
}

impl SerialConfig {
    /// Validates serial parameters against supported hardware limits.
    pub fn validate(&self) -> CoreResult<()> {
        if self.port_name.trim().is_empty() {
            return Err(CoreError::InvalidConfig(
                "serial port_name must not be empty".into(),
            ));
        }
        if self.baud_rate == 0 || self.baud_rate > MAX_SERIAL_BAUD_RATE {
            return Err(CoreError::InvalidConfig(format!(
                "serial baud_rate {} is out of range (1..={MAX_SERIAL_BAUD_RATE})",
                self.baud_rate
            )));
        }
        if !(5..=8).contains(&self.data_bits) {
            return Err(CoreError::InvalidConfig(format!(
                "serial data_bits {} must be in 5..=8",
                self.data_bits
            )));
        }
        Ok(())
    }
}

/// Discovered host serial port metadata matching `SerialPortInfoDto`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SerialPortInfo {
    pub port_name: String,
    pub description: Option<String>,
    pub vid: Option<u16>,
    pub pid: Option<u16>,
}

/// Parses USB `VID_xxxx` and `PID_xxxx` hexadecimal identifiers from a PnP instance string.
pub fn parse_usb_vid_pid(instance_id: &str) -> (Option<u16>, Option<u16>) {
    let upper = instance_id.to_ascii_uppercase();
    let vid = extract_hex_tag(&upper, "VID_").or_else(|| extract_hex_tag(&upper, "VID:"));
    let pid = extract_hex_tag(&upper, "PID_").or_else(|| extract_hex_tag(&upper, "PID:"));
    (vid, pid)
}

fn extract_hex_tag(haystack: &str, prefix: &str) -> Option<u16> {
    let idx = haystack.find(prefix)?;
    let rest = &haystack[idx + prefix.len()..];
    let hex_digits: String = rest
        .chars()
        .take(4)
        .take_while(|c| c.is_ascii_hexdigit())
        .collect();
    if hex_digits.len() == 4 {
        u16::from_str_radix(&hex_digits, 16).ok()
    } else {
        None
    }
}

/// Enumerates serial ports currently visible to the host operating system.
pub fn list_serial_ports() -> CoreResult<Vec<SerialPortInfo>> {
    list_serial_ports_os()
}

#[cfg(windows)]
type PnpPortMeta = (Option<String>, Option<u16>, Option<u16>);

#[cfg(windows)]
fn list_serial_ports_os() -> CoreResult<Vec<SerialPortInfo>> {
    use std::collections::{BTreeMap, HashMap};
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ};
    use winreg::RegKey;

    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let serialcomm = match hklm.open_subkey_with_flags(r"HARDWARE\DEVICEMAP\SERIALCOMM", KEY_READ) {
        Ok(key) => key,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(CoreError::Io(err)),
    };

    let mut pnp_metadata: HashMap<String, PnpPortMeta> = HashMap::new();
    for bus in [
        r"SYSTEM\CurrentControlSet\Enum\USB",
        r"SYSTEM\CurrentControlSet\Enum\FTDIBUS",
        r"SYSTEM\CurrentControlSet\Enum\ACPI",
        r"SYSTEM\CurrentControlSet\Enum\BTHENUM",
    ] {
        if let Ok(bus_key) = hklm.open_subkey_with_flags(bus, KEY_READ) {
            for dev_name in bus_key.enum_keys().flatten() {
                if let Ok(dev_key) = bus_key.open_subkey_with_flags(&dev_name, KEY_READ) {
                    for inst_name in dev_key.enum_keys().flatten() {
                        if let Ok(inst_key) = dev_key.open_subkey_with_flags(&inst_name, KEY_READ) {
                            let port_name = inst_key
                                .open_subkey_with_flags("Device Parameters", KEY_READ)
                                .ok()
                                .and_then(|dp| dp.get_value::<String, _>("PortName").ok());
                            if let Some(port) = port_name {
                                let desc = inst_key
                                    .get_value::<String, _>("FriendlyName")
                                    .ok()
                                    .or_else(|| inst_key.get_value::<String, _>("DeviceDesc").ok())
                                    .map(|s| {
                                        s.rsplit(';').next().unwrap_or(&s).trim().to_string()
                                    });
                                let (vid, pid) = parse_usb_vid_pid(&dev_name);
                                let (vid2, pid2) = if vid.is_none() || pid.is_none() {
                                    parse_usb_vid_pid(&inst_name)
                                } else {
                                    (vid, pid)
                                };
                                pnp_metadata.insert(
                                    port.trim().to_ascii_uppercase(),
                                    (desc, vid.or(vid2), pid.or(pid2)),
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    let mut ports: BTreeMap<String, SerialPortInfo> = BTreeMap::new();
    for item in serialcomm.enum_values() {
        let (device_name, value) = match item {
            Ok(pair) => pair,
            Err(_) => continue,
        };
        let port_name = value.to_string().trim().to_string();
        if port_name.is_empty() {
            continue;
        }
        let key_upper = port_name.to_ascii_uppercase();
        let (description, vid, pid) = pnp_metadata
            .get(&key_upper)
            .cloned()
            .unwrap_or((Some(device_name), None, None));
        ports.entry(key_upper).or_insert(SerialPortInfo {
            port_name,
            description,
            vid,
            pid,
        });
    }

    Ok(ports.into_values().collect())
}

#[cfg(unix)]
fn list_serial_ports_os() -> CoreResult<Vec<SerialPortInfo>> {
    use std::collections::BTreeMap;
    use std::path::Path;

    let mut ports: BTreeMap<String, SerialPortInfo> = BTreeMap::new();
    let entries = match std::fs::read_dir("/dev") {
        Ok(rd) => rd,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(CoreError::Io(err)),
    };

    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        let is_serial = name.starts_with("ttyUSB")
            || name.starts_with("ttyACM")
            || name.starts_with("ttyAMA")
            || name.starts_with("ttyS")
            || name.starts_with("tty.")
            || name.starts_with("cu.");
        if !is_serial {
            continue;
        }
        let full_path = format!("/dev/{name}");
        let sys_base = format!("/sys/class/tty/{name}/device");
        let vid = read_sysfs_hex(Path::new(&sys_base).join("../idVendor"))
            .or_else(|| read_sysfs_hex(Path::new(&sys_base).join("../../idVendor")));
        let pid = read_sysfs_hex(Path::new(&sys_base).join("../idProduct"))
            .or_else(|| read_sysfs_hex(Path::new(&sys_base).join("../../idProduct")));
        ports.entry(full_path.clone()).or_insert(SerialPortInfo {
            port_name: full_path,
            description: Some(name.into_owned()),
            vid,
            pid,
        });
    }

    Ok(ports.into_values().collect())
}

#[cfg(unix)]
fn read_sysfs_hex(path: std::path::PathBuf) -> Option<u16> {
    let raw = std::fs::read_to_string(path).ok()?;
    u16::from_str_radix(raw.trim().trim_start_matches("0x"), 16).ok()
}

#[cfg(not(any(windows, unix)))]
fn list_serial_ports_os() -> CoreResult<Vec<SerialPortInfo>> {
    Ok(Vec::new())
}

/// Opened bidirectional async serial port streams.
pub struct SerialPortStreams {
    pub reader: Box<dyn AsyncRead + Unpin + Send>,
    pub writer: Box<dyn AsyncWrite + Unpin + Send>,
    pub source_tag: Option<Arc<str>>,
}

/// Abstraction over OS serial port opening vs. virtual in-memory serial pairs.
pub trait SerialIoProvider: Send + Sync + 'static {
    fn open<'a>(
        &'a self,
        config: &'a SerialConfig,
    ) -> Pin<Box<dyn Future<Output = CoreResult<SerialPortStreams>> + Send + 'a>>;
}

/// Production OS serial backend using Win32 Comm (`windows-sys`) on Windows and `libc::termios` on Unix.
#[derive(Debug, Default)]
pub struct OsSerialProvider;

impl SerialIoProvider for OsSerialProvider {
    fn open<'a>(
        &'a self,
        config: &'a SerialConfig,
    ) -> Pin<Box<dyn Future<Output = CoreResult<SerialPortStreams>> + Send + 'a>> {
        let cfg = config.clone();
        Box::pin(async move {
            cfg.validate()?;
            tokio::task::spawn_blocking(move || open_os_serial_port(&cfg))
                .await
                .map_err(|e| CoreError::Transport(format!("serial open task failed: {e}")))?
        })
    }
}

/// Channel-backed [`AsyncRead`] bridge fed by a dedicated blocking serial reader thread.
struct ChannelAsyncReader {
    rx: mpsc::Receiver<io::Result<Bytes>>,
    pending: Bytes,
}

impl ChannelAsyncReader {
    fn new(rx: mpsc::Receiver<io::Result<Bytes>>) -> Self {
        Self {
            rx,
            pending: Bytes::new(),
        }
    }
}

impl AsyncRead for ChannelAsyncReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if !self.pending.is_empty() {
            let to_copy = self.pending.len().min(buf.remaining());
            buf.put_slice(&self.pending[..to_copy]);
            self.pending.advance(to_copy);
            return Poll::Ready(Ok(()));
        }

        match self.rx.poll_recv(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => Poll::Ready(Ok(())),
            Poll::Ready(Some(Err(err))) => Poll::Ready(Err(err)),
            Poll::Ready(Some(Ok(mut bytes))) => {
                if bytes.is_empty() {
                    return Poll::Ready(Ok(()));
                }
                let to_copy = bytes.len().min(buf.remaining());
                buf.put_slice(&bytes[..to_copy]);
                bytes.advance(to_copy);
                self.pending = bytes;
                Poll::Ready(Ok(()))
            }
        }
    }
}

/// Channel-backed [`AsyncWrite`] bridge feeding a dedicated blocking serial writer thread.
struct ChannelAsyncWriter {
    tx: crossbeam_channel::Sender<Vec<u8>>,
}

impl AsyncWrite for ChannelAsyncWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        match self.tx.send(buf.to_vec()) {
            Ok(()) => Poll::Ready(Ok(buf.len())),
            Err(_) => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "serial writer channel closed",
            ))),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[cfg(windows)]
fn open_os_serial_port(config: &SerialConfig) -> CoreResult<SerialPortStreams> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Devices::Communication::{
        EscapeCommFunction, GetCommState, PurgeComm, SetCommState, SetCommTimeouts, CLRDTR,
        COMMTIMEOUTS, DCB, EVENPARITY, NOPARITY, ODDPARITY, ONESTOPBIT, PURGE_RXCLEAR,
        PURGE_TXCLEAR, SETDTR, TWOSTOPBITS,
    };
    use windows_sys::Win32::Foundation::{
        CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FlushFileBuffers, ReadFile, WriteFile, FILE_ATTRIBUTE_NORMAL, OPEN_EXISTING,
    };

    struct OwnedWin32Handle(HANDLE);
    // SAFETY: Win32 HANDLE is exclusively owned by the dedicated serial worker thread.
    unsafe impl Send for OwnedWin32Handle {}
    impl Drop for OwnedWin32Handle {
        fn drop(&mut self) {
            if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
                // SAFETY: Valid handle opened via CreateFileW and closed exactly once on drop.
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
    }

    let trimmed = config.port_name.trim();
    let device_path = if trimmed.starts_with(r"\\.\") || trimmed.starts_with(r"\\?\") {
        trimmed.to_string()
    } else {
        format!(r"\\.\{trimmed}")
    };
    let wide_path: Vec<u16> = std::ffi::OsStr::new(&device_path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // SAFETY: `wide_path` is NUL-terminated UTF-16; parameters match Win32 CreateFileW contract.
    let raw_handle = unsafe {
        CreateFileW(
            wide_path.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    if raw_handle == INVALID_HANDLE_VALUE || raw_handle.is_null() {
        return Err(CoreError::Io(io::Error::last_os_error()));
    }
    let handle = OwnedWin32Handle(raw_handle);

    // SAFETY: `dcb` is zero-initialized with `DCBlength` set before `GetCommState`.
    let mut dcb: DCB = unsafe { std::mem::zeroed() };
    dcb.DCBlength = std::mem::size_of::<DCB>() as u32;
    // SAFETY: `handle.0` is an open COM device handle and `dcb` is valid.
    if unsafe { GetCommState(handle.0, &mut dcb) } == 0 {
        return Err(CoreError::Io(io::Error::last_os_error()));
    }

    dcb.BaudRate = config.baud_rate;
    dcb.ByteSize = config.data_bits;
    dcb.Parity = match config.parity {
        SerialParity::None => NOPARITY,
        SerialParity::Odd => ODDPARITY,
        SerialParity::Even => EVENPARITY,
    };
    dcb.StopBits = match config.stop_bits {
        SerialStopBits::One => ONESTOPBIT,
        SerialStopBits::Two => TWOSTOPBITS,
    };

    // Configure DCB _bitfield:
    // bit 0: fBinary = 1
    // bit 1: fParity
    // bit 2: fOutxCtsFlow
    // bits 4..=5: fDtrControl (0 = disable, 1 = enable)
    // bit 8: fOutX, bit 9: fInX
    // bits 12..=13: fRtsControl (0 = disable, 1 = enable, 2 = handshake)
    let clear_mask: u32 = (1 << 0)
        | (1 << 1)
        | (1 << 2)
        | (1 << 3)
        | (0b11 << 4)
        | (1 << 6)
        | (1 << 8)
        | (1 << 9)
        | (0b11 << 12)
        | (1 << 14);
    dcb._bitfield &= !clear_mask;
    dcb._bitfield |= 1 << 0; // fBinary
    if config.parity != SerialParity::None {
        dcb._bitfield |= 1 << 1; // fParity
    }
    if config.dtr_on_open {
        dcb._bitfield |= 1 << 4; // DTR_CONTROL_ENABLE
    }
    match config.flow_control {
        SerialFlowControl::None => {
            dcb._bitfield |= 1 << 12; // RTS_CONTROL_ENABLE
        }
        SerialFlowControl::HardwareRtsCts => {
            dcb._bitfield |= 1 << 2; // fOutxCtsFlow
            dcb._bitfield |= 2 << 12; // RTS_CONTROL_HANDSHAKE
        }
        SerialFlowControl::SoftwareXonXoff => {
            dcb._bitfield |= (1 << 8) | (1 << 9); // fOutX | fInX
            dcb._bitfield |= 1 << 12;
        }
    }

    // SAFETY: `dcb` contains valid Win32 serial parameters for `handle.0`.
    if unsafe { SetCommState(handle.0, &dcb) } == 0 {
        return Err(CoreError::Io(io::Error::last_os_error()));
    }

    let timeouts = COMMTIMEOUTS {
        ReadIntervalTimeout: u32::MAX,
        ReadTotalTimeoutMultiplier: u32::MAX,
        ReadTotalTimeoutConstant: 5,
        WriteTotalTimeoutMultiplier: 0,
        WriteTotalTimeoutConstant: 1_000,
    };
    // SAFETY: `timeouts` is a valid initialized `COMMTIMEOUTS`.
    if unsafe { SetCommTimeouts(handle.0, &timeouts) } == 0 {
        return Err(CoreError::Io(io::Error::last_os_error()));
    }

    // SAFETY: `handle.0` is valid; discard stale boot/driver bytes.
    unsafe {
        PurgeComm(handle.0, PURGE_RXCLEAR | PURGE_TXCLEAR);
        EscapeCommFunction(handle.0, if config.dtr_on_open { SETDTR } else { CLRDTR });
    }

    let (rx_tx, rx_rx) = mpsc::channel::<io::Result<Bytes>>(256);
    let (tx_tx, tx_rx) = crossbeam_channel::unbounded::<Vec<u8>>();
    let port_label = config.port_name.clone();

    std::thread::Builder::new()
        .name(format!("ddscope-serial-{port_label}"))
        .spawn(move || {
            let _owned = handle;
            let mut read_buf = vec![0u8; DEFAULT_SERIAL_READ_CHUNK_BYTES];
            loop {
                let mut tx_disconnected = false;
                loop {
                    match tx_rx.try_recv() {
                        Ok(chunk) => {
                            let mut offset = 0usize;
                            while offset < chunk.len() {
                                let mut written = 0u32;
                                // SAFETY: `_owned.0` is open; `chunk[offset..]` is valid for read.
                                let ok = unsafe {
                                    WriteFile(
                                        _owned.0,
                                        chunk[offset..].as_ptr(),
                                        (chunk.len() - offset) as u32,
                                        &mut written,
                                        std::ptr::null_mut(),
                                    )
                                };
                                if ok == 0 {
                                    let _ = rx_tx.blocking_send(Err(io::Error::last_os_error()));
                                    return;
                                }
                                if written == 0 {
                                    let _ = rx_tx.blocking_send(Err(io::Error::new(
                                        io::ErrorKind::WriteZero,
                                        "serial WriteFile wrote 0 bytes",
                                    )));
                                    return;
                                }
                                offset += written as usize;
                            }
                            // SAFETY: flush outbound serial ring buffer.
                            unsafe {
                                FlushFileBuffers(_owned.0);
                            }
                        }
                        Err(crossbeam_channel::TryRecvError::Empty) => break,
                        Err(crossbeam_channel::TryRecvError::Disconnected) => {
                            tx_disconnected = true;
                            break;
                        }
                    }
                }

                if tx_disconnected || rx_tx.is_closed() {
                    break;
                }

                let mut bytes_read = 0u32;
                // SAFETY: `_owned.0` is open; `read_buf` is valid writable memory of `len` bytes.
                let ok = unsafe {
                    ReadFile(
                        _owned.0,
                        read_buf.as_mut_ptr(),
                        read_buf.len() as u32,
                        &mut bytes_read,
                        std::ptr::null_mut(),
                    )
                };
                if ok == 0 {
                    let _ = rx_tx.blocking_send(Err(io::Error::last_os_error()));
                    break;
                }
                if bytes_read > 0 {
                    let pkt = Bytes::copy_from_slice(&read_buf[..bytes_read as usize]);
                    if rx_tx.blocking_send(Ok(pkt)).is_err() {
                        break;
                    }
                }
            }
        })
        .map_err(|e| CoreError::Transport(format!("failed to spawn serial worker thread: {e}")))?;

    Ok(SerialPortStreams {
        reader: Box::new(ChannelAsyncReader::new(rx_rx)),
        writer: Box::new(ChannelAsyncWriter { tx: tx_tx }),
        source_tag: Some(Arc::from(config.port_name.as_str())),
    })
}

#[cfg(unix)]
fn open_os_serial_port(config: &SerialConfig) -> CoreResult<SerialPortStreams> {
    use std::ffi::CString;

    struct OwnedFd(libc::c_int);
    impl Drop for OwnedFd {
        fn drop(&mut self) {
            if self.0 >= 0 {
                // SAFETY: Valid file descriptor closed once on drop.
                unsafe {
                    libc::close(self.0);
                }
            }
        }
    }

    let c_path = CString::new(config.port_name.as_str())
        .map_err(|_| CoreError::InvalidConfig("serial port_name contains interior NUL".into()))?;
    // SAFETY: `c_path` is a valid NUL-terminated C string.
    let raw_fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_RDWR | libc::O_NOCTTY | libc::O_NONBLOCK,
        )
    };
    if raw_fd < 0 {
        return Err(CoreError::Io(io::Error::last_os_error()));
    }
    let fd = OwnedFd(raw_fd);

    // Clear O_NONBLOCK after open so blocking reads respect VMIN/VTIME cleanly.
    // SAFETY: `fd.0` is an open file descriptor.
    unsafe {
        let flags = libc::fcntl(fd.0, libc::F_GETFL);
        if flags >= 0 {
            libc::fcntl(fd.0, libc::F_SETFL, flags & !libc::O_NONBLOCK);
        }
    }

    // SAFETY: `termios` is initialized by `tcgetattr` before modification.
    let mut termios: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: `fd.0` is valid and `termios` points to valid memory.
    if unsafe { libc::tcgetattr(fd.0, &mut termios) } != 0 {
        return Err(CoreError::Io(io::Error::last_os_error()));
    }

    // SAFETY: `cfmakeraw` configures raw UART mode on `termios`.
    unsafe {
        libc::cfmakeraw(&mut termios);
    }

    termios.c_cflag &= !(libc::CSIZE | libc::PARENB | libc::PARODD | libc::CSTOPB | libc::CRTSCTS);
    termios.c_cflag |= libc::CLOCAL | libc::CREAD;
    termios.c_cflag |= match config.data_bits {
        5 => libc::CS5,
        6 => libc::CS6,
        7 => libc::CS7,
        _ => libc::CS8,
    };
    match config.parity {
        SerialParity::None => {}
        SerialParity::Odd => termios.c_cflag |= libc::PARENB | libc::PARODD,
        SerialParity::Even => termios.c_cflag |= libc::PARENB,
    }
    if config.stop_bits == SerialStopBits::Two {
        termios.c_cflag |= libc::CSTOPB;
    }

    termios.c_iflag &= !(libc::IXON | libc::IXOFF | libc::IXANY);
    match config.flow_control {
        SerialFlowControl::None => {}
        SerialFlowControl::HardwareRtsCts => termios.c_cflag |= libc::CRTSCTS,
        SerialFlowControl::SoftwareXonXoff => termios.c_iflag |= libc::IXON | libc::IXOFF,
    }

    termios.c_cc[libc::VMIN] = 0;
    termios.c_cc[libc::VTIME] = 1; // 100ms read timeout for responsive shutdown

    let speed = unix_baud_constant(config.baud_rate).unwrap_or(libc::B115200);
    // SAFETY: `termios` is initialized and `speed` is a valid `speed_t`.
    unsafe {
        libc::cfsetispeed(&mut termios, speed);
        libc::cfsetospeed(&mut termios, speed);
        if libc::tcsetattr(fd.0, libc::TCSANOW, &termios) != 0 {
            return Err(CoreError::Io(io::Error::last_os_error()));
        }
        let mut modem_bits: libc::c_int = 0;
        if libc::ioctl(fd.0, libc::TIOCMGET as _, &mut modem_bits) == 0 {
            if config.dtr_on_open {
                modem_bits |= libc::TIOCM_DTR;
            } else {
                modem_bits &= !libc::TIOCM_DTR;
            }
            let _ = libc::ioctl(fd.0, libc::TIOCMSET as _, &modem_bits);
        }
        libc::tcflush(fd.0, libc::TCIOFLUSH);
    }

    let (rx_tx, rx_rx) = mpsc::channel::<io::Result<Bytes>>(256);
    let (tx_tx, tx_rx) = crossbeam_channel::unbounded::<Vec<u8>>();
    let port_label = config.port_name.clone();

    std::thread::Builder::new()
        .name(format!("ddscope-serial-{port_label}"))
        .spawn(move || {
            let _owned = fd;
            let mut read_buf = vec![0u8; DEFAULT_SERIAL_READ_CHUNK_BYTES];
            loop {
                let mut tx_disconnected = false;
                loop {
                    match tx_rx.try_recv() {
                        Ok(chunk) => {
                            let mut offset = 0usize;
                            while offset < chunk.len() {
                                // SAFETY: `_owned.0` is open; slice pointer & length are valid.
                                let n = unsafe {
                                    libc::write(
                                        _owned.0,
                                        chunk[offset..].as_ptr().cast(),
                                        chunk.len() - offset,
                                    )
                                };
                                if n < 0 {
                                    let err = io::Error::last_os_error();
                                    if err.kind() == io::ErrorKind::Interrupted {
                                        continue;
                                    }
                                    let _ = rx_tx.blocking_send(Err(err));
                                    return;
                                }
                                offset += n as usize;
                            }
                        }
                        Err(crossbeam_channel::TryRecvError::Empty) => break,
                        Err(crossbeam_channel::TryRecvError::Disconnected) => {
                            tx_disconnected = true;
                            break;
                        }
                    }
                }

                if tx_disconnected || rx_tx.is_closed() {
                    break;
                }

                // SAFETY: `_owned.0` is open; `read_buf` is valid writable memory.
                let n = unsafe {
                    libc::read(
                        _owned.0,
                        read_buf.as_mut_ptr().cast(),
                        read_buf.len(),
                    )
                };
                if n < 0 {
                    let err = io::Error::last_os_error();
                    if err.kind() == io::ErrorKind::Interrupted
                        || err.kind() == io::ErrorKind::WouldBlock
                    {
                        continue;
                    }
                    let _ = rx_tx.blocking_send(Err(err));
                    break;
                }
                if n > 0 {
                    let pkt = Bytes::copy_from_slice(&read_buf[..n as usize]);
                    if rx_tx.blocking_send(Ok(pkt)).is_err() {
                        break;
                    }
                }
            }
        })
        .map_err(|e| CoreError::Transport(format!("failed to spawn serial worker thread: {e}")))?;

    Ok(SerialPortStreams {
        reader: Box::new(ChannelAsyncReader::new(rx_rx)),
        writer: Box::new(ChannelAsyncWriter { tx: tx_tx }),
        source_tag: Some(Arc::from(config.port_name.as_str())),
    })
}

#[cfg(unix)]
fn unix_baud_constant(baud: u32) -> Option<libc::speed_t> {
    match baud {
        9_600 => Some(libc::B9600),
        19_200 => Some(libc::B19200),
        38_400 => Some(libc::B38400),
        57_600 => Some(libc::B57600),
        115_200 => Some(libc::B115200),
        230_400 => Some(libc::B230400),
        #[cfg(target_os = "linux")]
        460_800 => Some(libc::B460800),
        #[cfg(target_os = "linux")]
        921_600 => Some(libc::B921600),
        #[cfg(target_os = "linux")]
        1_000_000 => Some(libc::B1000000),
        #[cfg(target_os = "linux")]
        1_500_000 => Some(libc::B1500000),
        #[cfg(target_os = "linux")]
        2_000_000 => Some(libc::B2000000),
        #[cfg(target_os = "linux")]
        3_000_000 => Some(libc::B3000000),
        #[cfg(target_os = "linux")]
        4_000_000 => Some(libc::B4000000),
        _ => None,
    }
}

#[cfg(not(any(windows, unix)))]
fn open_os_serial_port(_config: &SerialConfig) -> CoreResult<SerialPortStreams> {
    Err(CoreError::Unsupported(
        "OS serial port is not supported on this platform".into(),
    ))
}

/// Peer handle returned by [`VirtualSerialPair::take_peer`] / [`VirtualSerialPair::replug`].
pub struct VirtualSerialPeer {
    pub reader: tokio::io::ReadHalf<tokio::io::DuplexStream>,
    pub writer: tokio::io::WriteHalf<tokio::io::DuplexStream>,
}

impl VirtualSerialPeer {
    /// Writes bytes to the simulated serial device so [`SerialTransport`] receives them as RX.
    pub async fn send_rx(&mut self, data: &[u8]) -> io::Result<()> {
        self.writer.write_all(data).await?;
        self.writer.flush().await
    }

    /// Reads bytes transmitted by [`SerialTransport`] TX.
    pub async fn recv_tx(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.reader.read(buf).await
    }
}

pub(crate) struct KillableReader<R> {
    inner: R,
    kill_rx: Option<tokio::sync::oneshot::Receiver<()>>,
    killed: bool,
}

impl<R> KillableReader<R> {
    pub(crate) fn new(inner: R, kill_rx: tokio::sync::oneshot::Receiver<()>) -> Self {
        Self {
            inner,
            kill_rx: Some(kill_rx),
            killed: false,
        }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for KillableReader<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.killed {
            return Poll::Ready(Ok(()));
        }
        if let Some(ref mut rx) = self.kill_rx {
            if Pin::new(rx).poll(cx).is_ready() {
                self.kill_rx = None;
                self.killed = true;
                return Poll::Ready(Ok(()));
            }
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

pub(crate) struct KillableWriter<W> {
    inner: W,
    kill_rx: Option<tokio::sync::oneshot::Receiver<()>>,
    killed: bool,
}

impl<W> KillableWriter<W> {
    pub(crate) fn new(inner: W, kill_rx: tokio::sync::oneshot::Receiver<()>) -> Self {
        Self {
            inner,
            kill_rx: Some(kill_rx),
            killed: false,
        }
    }
}

impl<W: AsyncWrite + Unpin> AsyncWrite for KillableWriter<W> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.killed {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "virtual serial port unplugged",
            )));
        }
        if let Some(ref mut rx) = self.kill_rx {
            if Pin::new(rx).poll(cx).is_ready() {
                self.kill_rx = None;
                self.killed = true;
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "virtual serial port unplugged",
                )));
            }
        }
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.killed {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "virtual serial port unplugged",
            )));
        }
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

struct VirtualSerialState {
    port_name: String,
    online: AtomicBool,
    open_count: AtomicU64,
    last_config: Mutex<Option<SerialConfig>>,
    active_killers: Mutex<Vec<tokio::sync::oneshot::Sender<()>>>,
    pending_host_stream: Mutex<Option<tokio::io::DuplexStream>>,
    pending_peer_stream: Mutex<Option<tokio::io::DuplexStream>>,
}

/// Deterministic in-memory virtual serial port pair supporting hot-plug unplug/replug tests.
#[derive(Clone)]
pub struct VirtualSerialPair {
    state: Arc<VirtualSerialState>,
}

impl VirtualSerialPair {
    /// Creates an initially attached virtual serial port matching `port_name`.
    pub fn new(port_name: impl Into<String>) -> Self {
        let (host_stream, peer_stream) = tokio::io::duplex(64 * 1024);
        Self {
            state: Arc::new(VirtualSerialState {
                port_name: port_name.into(),
                online: AtomicBool::new(true),
                open_count: AtomicU64::new(0),
                last_config: Mutex::new(None),
                active_killers: Mutex::new(Vec::new()),
                pending_host_stream: Mutex::new(Some(host_stream)),
                pending_peer_stream: Mutex::new(Some(peer_stream)),
            }),
        }
    }

    /// Returns a [`SerialIoProvider`] handle backed by this virtual pair.
    pub fn provider(&self) -> Arc<dyn SerialIoProvider> {
        Arc::new(self.clone())
    }

    /// Takes the current peer endpoint for reading TX and injecting RX bytes.
    pub fn take_peer(&self) -> Option<VirtualSerialPeer> {
        let peer = self.state.pending_peer_stream.lock().take()?;
        let (reader, writer) = tokio::io::split(peer);
        Some(VirtualSerialPeer { reader, writer })
    }

    /// Simulates physical USB/UART cable unplug by closing any active connection and marking offline.
    pub fn unplug(&self) {
        self.state.online.store(false, Ordering::Release);
        self.state.pending_host_stream.lock().take();
        self.state.pending_peer_stream.lock().take();
        let killers = std::mem::take(&mut *self.state.active_killers.lock());
        for tx in killers {
            let _ = tx.send(());
        }
    }

    /// Simulates physical cable re-insertion and returns the newly attached peer endpoint.
    pub fn replug(&self) -> VirtualSerialPeer {
        let (host_stream, peer_stream) = tokio::io::duplex(64 * 1024);
        *self.state.pending_host_stream.lock() = Some(host_stream);
        *self.state.pending_peer_stream.lock() = None;
        self.state.online.store(true, Ordering::Release);
        let (reader, writer) = tokio::io::split(peer_stream);
        VirtualSerialPeer { reader, writer }
    }

    /// Returns how many times [`SerialIoProvider::open`] succeeded on this virtual port.
    pub fn open_count(&self) -> u64 {
        self.state.open_count.load(Ordering::Acquire)
    }

    /// Returns the most recent [`SerialConfig`] passed to [`SerialIoProvider::open`].
    pub fn last_config(&self) -> Option<SerialConfig> {
        self.state.last_config.lock().clone()
    }
}

impl SerialIoProvider for VirtualSerialPair {
    fn open<'a>(
        &'a self,
        config: &'a SerialConfig,
    ) -> Pin<Box<dyn Future<Output = CoreResult<SerialPortStreams>> + Send + 'a>> {
        let cfg = config.clone();
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            cfg.validate()?;
            if cfg.port_name != state.port_name {
                return Err(CoreError::Io(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!(
                        "virtual serial port {} not found (expected {})",
                        cfg.port_name, state.port_name
                    ),
                )));
            }
            if !state.online.load(Ordering::Acquire) {
                return Err(CoreError::Io(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("virtual serial port {} is unplugged", cfg.port_name),
                )));
            }

            let host_stream = {
                let mut guard = state.pending_host_stream.lock();
                if let Some(stream) = guard.take() {
                    stream
                } else {
                    let (host, peer) = tokio::io::duplex(64 * 1024);
                    *state.pending_peer_stream.lock() = Some(peer);
                    host
                }
            };

            *state.last_config.lock() = Some(cfg.clone());
            state.open_count.fetch_add(1, Ordering::AcqRel);

            let (kill_r_tx, kill_r_rx) = tokio::sync::oneshot::channel();
            let (kill_w_tx, kill_w_rx) = tokio::sync::oneshot::channel();
            {
                let mut killers = state.active_killers.lock();
                killers.push(kill_r_tx);
                killers.push(kill_w_tx);
            }

            let (reader, writer) = tokio::io::split(host_stream);
            Ok(SerialPortStreams {
                reader: Box::new(KillableReader {
                    inner: reader,
                    kill_rx: Some(kill_r_rx),
                    killed: false,
                }),
                writer: Box::new(KillableWriter {
                    inner: writer,
                    kill_rx: Some(kill_w_rx),
                    killed: false,
                }),
                source_tag: Some(Arc::from(cfg.port_name.as_str())),
            })
        })
    }
}

pub(crate) struct StateTracker {
    current: AtomicU8,
    watch_tx: watch::Sender<TransportState>,
    events_tx: broadcast::Sender<TransportState>,
}

impl StateTracker {
    pub(crate) fn new() -> Self {
        let (watch_tx, _) = watch::channel(TransportState::Stopped);
        let (events_tx, _) = broadcast::channel(64);
        Self {
            current: AtomicU8::new(encode_state(TransportState::Stopped)),
            watch_tx,
            events_tx,
        }
    }

    pub(crate) fn get(&self) -> TransportState {
        decode_state(self.current.load(Ordering::Acquire))
    }

    pub(crate) fn set(&self, state: TransportState) {
        self.current.store(encode_state(state), Ordering::Release);
        let _ = self.watch_tx.send(state);
        let _ = self.events_tx.send(state);
    }

    pub(crate) fn subscribe_watch(&self) -> watch::Receiver<TransportState> {
        self.watch_tx.subscribe()
    }

    pub(crate) fn subscribe_events(&self) -> broadcast::Receiver<TransportState> {
        self.events_tx.subscribe()
    }
}

fn encode_state(state: TransportState) -> u8 {
    match state {
        TransportState::Stopped => 0,
        TransportState::Connecting => 1,
        TransportState::Running => 2,
        TransportState::Reconnecting => 3,
        TransportState::Error => 4,
    }
}

fn decode_state(code: u8) -> TransportState {
    match code {
        1 => TransportState::Connecting,
        2 => TransportState::Running,
        3 => TransportState::Reconnecting,
        4 => TransportState::Error,
        _ => TransportState::Stopped,
    }
}

/// Asynchronous UART / serial port [`Transport`] implementation with hot-plug auto-reconnect.
pub struct SerialTransport {
    config: SerialConfig,
    provider: Arc<dyn SerialIoProvider>,
    state: Arc<StateTracker>,
}

impl std::fmt::Debug for SerialTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SerialTransport")
            .field("config", &self.config)
            .field("state", &self.state())
            .finish()
    }
}

impl SerialTransport {
    /// Creates a [`SerialTransport`] backed by the host OS serial provider.
    pub fn new(config: SerialConfig) -> CoreResult<Self> {
        Self::with_provider(config, Arc::new(OsSerialProvider))
    }

    /// Creates a [`SerialTransport`] backed by a custom [`SerialIoProvider`] (e.g., [`VirtualSerialPair`]).
    pub fn with_provider(
        config: SerialConfig,
        provider: Arc<dyn SerialIoProvider>,
    ) -> CoreResult<Self> {
        config.validate()?;
        Ok(Self {
            config,
            provider,
            state: Arc::new(StateTracker::new()),
        })
    }

    /// Returns the validated [`SerialConfig`].
    pub fn config(&self) -> &SerialConfig {
        &self.config
    }

    /// Returns the transport's live internal connection state.
    pub fn state(&self) -> TransportState {
        self.state.get()
    }

    /// Subscribes to live [`TransportState`] updates via `tokio::sync::watch`.
    pub fn subscribe_state(&self) -> watch::Receiver<TransportState> {
        self.state.subscribe_watch()
    }

    /// Subscribes to every individual [`TransportState`] transition without coalescing.
    pub fn subscribe_state_events(&self) -> broadcast::Receiver<TransportState> {
        self.state.subscribe_events()
    }
}

impl Transport for SerialTransport {
    fn capabilities(&self) -> TransportCaps {
        TransportCaps {
            kind: TransportKind::Serial,
            supports_tx: true,
            supports_seek: false,
            supports_raw_record: true,
            supports_auto_reconnect: self.config.auto_reconnect,
        }
    }

    fn start(
        &mut self,
        rx_sink: PacketSender,
        tx_rx: OutboundReceiver,
        mut shutdown_rx: ShutdownReceiver,
    ) -> CoreResult<JoinHandle<CoreResult<()>>> {
        self.config.validate()?;
        let config = self.config.clone();
        let provider = Arc::clone(&self.provider);
        let state = Arc::clone(&self.state);
        state.set(TransportState::Connecting);

        Ok(tokio::spawn(async move {
            let mut emitter = RxPacketEmitter::new(
                rx_sink,
                BufferPoolConfig::default(),
                RxBackpressurePolicy::DropNewest,
            );
            let mut tx_consumer = TxStreamConsumer::new(tx_rx);
            let mut rx_buf = vec![0u8; DEFAULT_SERIAL_READ_CHUNK_BYTES];
            let reconnect_delay = Duration::from_millis(config.reconnect_delay_ms.max(1));

            loop {
                if *shutdown_rx.borrow() {
                    state.set(TransportState::Stopped);
                    return Ok(());
                }

                let opened = tokio::select! {
                    biased;
                    res = shutdown_rx.changed() => {
                        if res.is_err() || *shutdown_rx.borrow() {
                            state.set(TransportState::Stopped);
                            return Ok(());
                        }
                        continue;
                    }
                    res = provider.open(&config) => res,
                };

                let mut streams = match opened {
                    Ok(streams) => {
                        state.set(TransportState::Running);
                        streams
                    }
                    Err(err) => {
                        if !config.auto_reconnect {
                            state.set(TransportState::Error);
                            return Err(err);
                        }
                        if state.get() != TransportState::Reconnecting {
                            state.set(TransportState::Reconnecting);
                        }
                        if wait_reconnect_or_shutdown(reconnect_delay, &mut shutdown_rx).await {
                            state.set(TransportState::Stopped);
                            return Ok(());
                        }
                        continue;
                    }
                };

                let session_result = run_serial_session(
                    &mut streams,
                    &mut emitter,
                    &mut tx_consumer,
                    &mut shutdown_rx,
                    &mut rx_buf,
                )
                .await;

                match session_result {
                    SessionEnd::Shutdown => {
                        state.set(TransportState::Stopped);
                        return Ok(());
                    }
                    SessionEnd::Disconnected(reason) => {
                        if !config.auto_reconnect {
                            state.set(TransportState::Error);
                            return Err(CoreError::Transport(reason));
                        }
                        state.set(TransportState::Reconnecting);
                        if wait_reconnect_or_shutdown(reconnect_delay, &mut shutdown_rx).await {
                            state.set(TransportState::Stopped);
                            return Ok(());
                        }
                    }
                }
            }
        }))
    }
}

enum SessionEnd {
    Shutdown,
    Disconnected(String),
}

async fn run_serial_session(
    streams: &mut SerialPortStreams,
    emitter: &mut RxPacketEmitter,
    tx_consumer: &mut TxStreamConsumer,
    shutdown_rx: &mut ShutdownReceiver,
    rx_buf: &mut [u8],
) -> SessionEnd {
    loop {
        tokio::select! {
            biased;
            res = shutdown_rx.changed() => {
                if res.is_err() || *shutdown_rx.borrow() {
                    return SessionEnd::Shutdown;
                }
            }
            read_res = streams.reader.read(rx_buf) => {
                match read_res {
                    Ok(0) => {
                        return SessionEnd::Disconnected("serial port EOF / disconnected".into());
                    }
                    Ok(n) => {
                        if let Err(CoreError::ChannelClosed) = emitter.emit_slice(
                            &rx_buf[..n],
                            host_monotonic_ns(),
                            streams.source_tag.clone(),
                        ) {
                            return SessionEnd::Shutdown;
                        }
                    }
                    Err(err) => {
                        return SessionEnd::Disconnected(format!("serial read error: {err}"));
                    }
                }
            }
            maybe_frame = tx_consumer.recv_next() => {
                match maybe_frame {
                    Some(frame) => {
                        if let Err(err) = streams.writer.write_all(&frame.payload).await {
                            return SessionEnd::Disconnected(format!("serial write error: {err}"));
                        }
                        if let Err(err) = streams.writer.flush().await {
                            return SessionEnd::Disconnected(format!("serial flush error: {err}"));
                        }
                    }
                    None => return SessionEnd::Shutdown,
                }
            }
        }
    }
}

pub(crate) async fn wait_reconnect_or_shutdown(
    delay: Duration,
    shutdown_rx: &mut ShutdownReceiver,
) -> bool {
    let sleep = tokio::time::sleep(delay);
    tokio::pin!(sleep);
    loop {
        tokio::select! {
            biased;
            res = shutdown_rx.changed() => {
                if res.is_err() || *shutdown_rx.borrow() {
                    return true;
                }
            }
            () = &mut sleep => {
                return *shutdown_rx.borrow();
            }
        }
    }
}

/// Static [`TransportFactoryFn`](crate::plugin::TransportFactoryFn) creating a [`SerialTransport`] from JSON.
pub fn create_serial_transport(json_config: &str) -> CoreResult<Box<dyn Transport>> {
    let cfg: SerialConfig = serde_json::from_str(json_config)
        .map_err(|e| CoreError::InvalidConfig(format!("invalid serial transport JSON: {e}")))?;
    Ok(Box::new(SerialTransport::new(cfg)?))
}
