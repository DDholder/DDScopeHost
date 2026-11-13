//! Asynchronous UDP and TCP (`TcpClient` / `TcpServer`) transports, `socket2` OS backends,
//! and deterministic virtual network backends for sandboxed testing.

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use super::buffer_pool::BufferPoolConfig;
use super::scheduler::{host_monotonic_ns, RxBackpressurePolicy, RxPacketEmitter};
use super::serial::{wait_reconnect_or_shutdown, KillableReader, KillableWriter, StateTracker};
use super::tx_queue::TxStreamConsumer;
use super::{
    OutboundReceiver, PacketSender, ShutdownReceiver, Transport, TransportCaps, TransportKind,
    TransportState,
};
use crate::types::{CoreError, CoreResult};

/// Default UDP socket receive buffer size (`4 MiB`).
pub const DEFAULT_UDP_RECV_BUFFER_BYTES: u32 = 4 * 1024 * 1024;

/// Default maximum datagram buffer size per UDP read (`64 KiB - 1`).
pub const DEFAULT_UDP_MAX_DATAGRAM_BYTES: usize = 65_535;

/// Default delay in milliseconds between `TcpClientTransport` reconnect attempts.
pub const DEFAULT_TCP_RECONNECT_DELAY_MS: u64 = 500;

/// Default read chunk size for TCP streams (`16 KiB`).
const DEFAULT_TCP_READ_CHUNK_BYTES: usize = 16 * 1024;

fn default_udp_recv_buffer_bytes() -> u32 {
    DEFAULT_UDP_RECV_BUFFER_BYTES
}

fn default_udp_max_datagram_bytes() -> usize {
    DEFAULT_UDP_MAX_DATAGRAM_BYTES
}

fn default_true() -> bool {
    true
}

fn default_tcp_reconnect_delay_ms() -> u64 {
    DEFAULT_TCP_RECONNECT_DELAY_MS
}

/// Configuration for [`UdpTransport`] matching `TransportConfigDto::Udp`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UdpConfig {
    pub bind_addr: String,
    #[serde(default)]
    pub multicast_group: Option<String>,
    #[serde(default = "default_udp_recv_buffer_bytes")]
    pub recv_buffer_bytes: u32,
    #[serde(default)]
    pub remote_tx_addr: Option<String>,
    #[serde(default)]
    pub broadcast: bool,
    #[serde(default = "default_udp_max_datagram_bytes")]
    pub max_datagram_bytes: usize,
}

impl Default for UdpConfig {
    fn default() -> Self {
        Self {
            bind_addr: "0.0.0.0:0".to_string(),
            multicast_group: None,
            recv_buffer_bytes: DEFAULT_UDP_RECV_BUFFER_BYTES,
            remote_tx_addr: None,
            broadcast: false,
            max_datagram_bytes: DEFAULT_UDP_MAX_DATAGRAM_BYTES,
        }
    }
}

impl UdpConfig {
    /// Parses and validates all configured UDP addresses and buffer bounds.
    pub fn validate(&self) -> CoreResult<ValidatedUdpConfig> {
        let bind_addr: SocketAddr = self.bind_addr.parse().map_err(|e| {
            CoreError::InvalidConfig(format!("invalid UDP bind_addr `{}`: {e}", self.bind_addr))
        })?;

        let multicast_group = match self.multicast_group.as_deref() {
            Some(raw) if !raw.trim().is_empty() => {
                let ip: IpAddr = raw.parse().map_err(|e| {
                    CoreError::InvalidConfig(format!("invalid UDP multicast_group `{raw}`: {e}"))
                })?;
                if !ip.is_multicast() {
                    return Err(CoreError::InvalidConfig(format!(
                        "UDP multicast_group `{raw}` is not a multicast IP address"
                    )));
                }
                match (bind_addr, ip) {
                    (SocketAddr::V4(_), IpAddr::V4(_)) | (SocketAddr::V6(_), IpAddr::V6(_)) => {}
                    _ => {
                        return Err(CoreError::InvalidConfig(format!(
                            "UDP bind_addr `{bind_addr}` and multicast_group `{ip}` address families differ"
                        )));
                    }
                }
                Some(ip)
            }
            _ => None,
        };

        let remote_tx_addr = match self.remote_tx_addr.as_deref() {
            Some(raw) if !raw.trim().is_empty() => {
                let addr: SocketAddr = raw.parse().map_err(|e| {
                    CoreError::InvalidConfig(format!(
                        "invalid UDP remote_tx_addr `{raw}`: {e}"
                    ))
                })?;
                Some(addr)
            }
            _ => None,
        };

        if self.max_datagram_bytes == 0 {
            return Err(CoreError::InvalidConfig(
                "UDP max_datagram_bytes must be > 0".into(),
            ));
        }

        Ok(ValidatedUdpConfig {
            bind_addr,
            multicast_group,
            recv_buffer_bytes: self.recv_buffer_bytes,
            remote_tx_addr,
            broadcast: self.broadcast,
            max_datagram_bytes: self.max_datagram_bytes,
        })
    }
}

/// Parsed and validated representation of [`UdpConfig`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedUdpConfig {
    pub bind_addr: SocketAddr,
    pub multicast_group: Option<IpAddr>,
    pub recv_buffer_bytes: u32,
    pub remote_tx_addr: Option<SocketAddr>,
    pub broadcast: bool,
    pub max_datagram_bytes: usize,
}

/// Boxed Send future alias used across transport backend traits.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

type VirtualUdpRx = Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<(Bytes, SocketAddr)>>>;

/// Async UDP socket handle abstracted over OS `socket2`/`tokio::net::UdpSocket` and virtual hubs.
pub trait UdpSocketHandle: Send + Sync + 'static {
    fn local_addr(&self) -> CoreResult<SocketAddr>;

    fn recv_from<'a>(&'a self, buf: &'a mut [u8]) -> BoxFuture<'a, io::Result<(usize, SocketAddr)>>;

    fn send_to<'a>(&'a self, data: &'a [u8], target: SocketAddr) -> BoxFuture<'a, io::Result<usize>>;
}

/// Factory abstraction for binding UDP sockets.
pub trait UdpSocketBackend: Send + Sync + 'static {
    fn bind<'a>(
        &'a self,
        config: &'a ValidatedUdpConfig,
    ) -> BoxFuture<'a, CoreResult<Box<dyn UdpSocketHandle>>>;
}

/// Creates and configures a real OS [`socket2::Socket`] bound according to `config`.
pub fn configure_os_udp_socket(config: &ValidatedUdpConfig) -> CoreResult<Socket> {
    let domain = Domain::for_address(config.bind_addr);
    let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    if config.recv_buffer_bytes > 0 {
        socket.set_recv_buffer_size(config.recv_buffer_bytes as usize)?;
    }
    if config.broadcast {
        socket.set_broadcast(true)?;
    }
    socket.set_nonblocking(true)?;
    socket.bind(&SockAddr::from(config.bind_addr))?;
    Ok(socket)
}

/// Production OS UDP backend using `socket2` + `tokio::net::UdpSocket`.
#[derive(Debug, Default)]
pub struct OsUdpBackend;

struct OsUdpSocketHandle {
    socket: tokio::net::UdpSocket,
}

impl UdpSocketHandle for OsUdpSocketHandle {
    fn local_addr(&self) -> CoreResult<SocketAddr> {
        Ok(self.socket.local_addr()?)
    }

    fn recv_from<'a>(
        &'a self,
        buf: &'a mut [u8],
    ) -> Pin<Box<dyn Future<Output = io::Result<(usize, SocketAddr)>> + Send + 'a>> {
        Box::pin(self.socket.recv_from(buf))
    }

    fn send_to<'a>(
        &'a self,
        data: &'a [u8],
        target: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = io::Result<usize>> + Send + 'a>> {
        Box::pin(self.socket.send_to(data, target))
    }
}

impl UdpSocketBackend for OsUdpBackend {
    fn bind<'a>(
        &'a self,
        config: &'a ValidatedUdpConfig,
    ) -> Pin<Box<dyn Future<Output = CoreResult<Box<dyn UdpSocketHandle>>> + Send + 'a>> {
        let cfg = config.clone();
        Box::pin(async move {
            let sock2 = configure_os_udp_socket(&cfg)?;
            let std_sock: std::net::UdpSocket = sock2.into();
            let tokio_sock = tokio::net::UdpSocket::from_std(std_sock)?;

            if let Some(mcast) = cfg.multicast_group {
                match (cfg.bind_addr, mcast) {
                    (SocketAddr::V4(v4_bind), IpAddr::V4(v4_mcast)) => {
                        let iface = if v4_bind.ip().is_loopback() {
                            *v4_bind.ip()
                        } else {
                            Ipv4Addr::UNSPECIFIED
                        };
                        tokio_sock.join_multicast_v4(v4_mcast, iface)?;
                    }
                    (SocketAddr::V6(_), IpAddr::V6(v6_mcast)) => {
                        tokio_sock.join_multicast_v6(&v6_mcast, 0)?;
                    }
                    _ => {
                        return Err(CoreError::InvalidConfig(
                            "UDP multicast address family mismatch".into(),
                        ));
                    }
                }
            }

            Ok(Box::new(OsUdpSocketHandle { socket: tokio_sock }) as Box<dyn UdpSocketHandle>)
        })
    }
}

/// Recorded socket configuration observed by [`VirtualUdpHub`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VirtualUdpBindingInfo {
    pub local_addr: SocketAddr,
    pub recv_buffer_bytes: u32,
    pub broadcast: bool,
    pub multicast_group: Option<IpAddr>,
}

struct VirtualUdpEndpointEntry {
    tx: mpsc::UnboundedSender<(Bytes, SocketAddr)>,
    info: VirtualUdpBindingInfo,
}

struct VirtualUdpHubInner {
    next_ephemeral_port: AtomicU16,
    endpoints: Mutex<HashMap<SocketAddr, VirtualUdpEndpointEntry>>,
    history: Mutex<Vec<VirtualUdpBindingInfo>>,
}

/// Deterministic in-memory UDP hub supporting unicast, IPv4/IPv6 multicast, and broadcast.
#[derive(Clone)]
pub struct VirtualUdpHub {
    inner: Arc<VirtualUdpHubInner>,
}

impl Default for VirtualUdpHub {
    fn default() -> Self {
        Self::new()
    }
}

impl VirtualUdpHub {
    /// Creates an empty [`VirtualUdpHub`].
    pub fn new() -> Self {
        Self {
            inner: Arc::new(VirtualUdpHubInner {
                next_ephemeral_port: AtomicU16::new(30_000),
                endpoints: Mutex::new(HashMap::new()),
                history: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Returns a [`UdpSocketBackend`] handle backed by this hub.
    pub fn backend(&self) -> Arc<dyn UdpSocketBackend> {
        Arc::new(self.clone())
    }

    /// Returns all socket bindings recorded by this hub in chronological order.
    pub fn binding_history(&self) -> Vec<VirtualUdpBindingInfo> {
        self.inner.history.lock().clone()
    }

    /// Binds a standalone peer endpoint on the virtual hub for sending/receiving datagrams in tests.
    pub fn bind_peer(
        &self,
        bind_addr: SocketAddr,
        broadcast: bool,
        multicast_group: Option<IpAddr>,
    ) -> CoreResult<VirtualUdpPeer> {
        let cfg = ValidatedUdpConfig {
            bind_addr,
            multicast_group,
            recv_buffer_bytes: DEFAULT_UDP_RECV_BUFFER_BYTES,
            remote_tx_addr: None,
            broadcast,
            max_datagram_bytes: DEFAULT_UDP_MAX_DATAGRAM_BYTES,
        };
        let (handle, rx) = self.bind_internal(&cfg)?;
        Ok(VirtualUdpPeer { handle, rx })
    }

    fn bind_internal(
        &self,
        config: &ValidatedUdpConfig,
    ) -> CoreResult<(VirtualUdpSocketHandle, VirtualUdpRx)> {
        let mut endpoints = self.inner.endpoints.lock();
        let mut actual_addr = config.bind_addr;
        if actual_addr.port() == 0 {
            loop {
                let candidate_port = self.inner.next_ephemeral_port.fetch_add(1, Ordering::Relaxed);
                let candidate = SocketAddr::new(actual_addr.ip(), candidate_port);
                if !endpoints.contains_key(&candidate) {
                    actual_addr = candidate;
                    break;
                }
            }
        } else if endpoints.contains_key(&actual_addr) {
            return Err(CoreError::Io(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!("virtual UDP address {actual_addr} already bound"),
            )));
        }

        let (tx, rx) = mpsc::unbounded_channel();
        let info = VirtualUdpBindingInfo {
            local_addr: actual_addr,
            recv_buffer_bytes: config.recv_buffer_bytes,
            broadcast: config.broadcast,
            multicast_group: config.multicast_group,
        };
        endpoints.insert(
            actual_addr,
            VirtualUdpEndpointEntry {
                tx,
                info: info.clone(),
            },
        );
        self.inner.history.lock().push(info);

        let rx = Arc::new(tokio::sync::Mutex::new(rx));
        let handle = VirtualUdpSocketHandle {
            inner: Arc::clone(&self.inner),
            local_addr: actual_addr,
            broadcast: config.broadcast,
            rx: Arc::clone(&rx),
        };
        Ok((handle, rx))
    }

    fn deliver_datagram(
        &self,
        src: SocketAddr,
        src_broadcast: bool,
        target: SocketAddr,
        data: &[u8],
    ) -> io::Result<usize> {
        let payload = Bytes::copy_from_slice(data);
        let endpoints = self.inner.endpoints.lock();

        if target.ip().is_multicast() {
            for entry in endpoints.values() {
                if entry.info.multicast_group == Some(target.ip())
                    && entry.info.local_addr.port() == target.port()
                {
                    let _ = entry.tx.send((payload.clone(), src));
                }
            }
            return Ok(data.len());
        }

        if is_ipv4_broadcast(target.ip()) {
            if !src_broadcast {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "UDP broadcast attempted without SO_BROADCAST enabled",
                ));
            }
            for entry in endpoints.values() {
                if entry.info.local_addr.is_ipv4()
                    && entry.info.local_addr.port() == target.port()
                    && entry.info.local_addr != src
                {
                    let _ = entry.tx.send((payload.clone(), src));
                }
            }
            return Ok(data.len());
        }

        if let Some(entry) = endpoints.get(&target) {
            let _ = entry.tx.send((payload, src));
            return Ok(data.len());
        }

        // Match wildcard bindings (`0.0.0.0:port` or `[::]:port`) on matching address family.
        let wildcard_ip = match target {
            SocketAddr::V4(_) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            SocketAddr::V6(_) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
        };
        if let Some(entry) = endpoints.get(&SocketAddr::new(wildcard_ip, target.port())) {
            let _ = entry.tx.send((payload, src));
        }

        Ok(data.len())
    }
}

fn is_ipv4_broadcast(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_broadcast() || v4.octets()[3] == 255,
        IpAddr::V6(_) => false,
    }
}

struct VirtualUdpSocketHandle {
    inner: Arc<VirtualUdpHubInner>,
    local_addr: SocketAddr,
    broadcast: bool,
    rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<(Bytes, SocketAddr)>>>,
}

impl Drop for VirtualUdpSocketHandle {
    fn drop(&mut self) {
        self.inner.endpoints.lock().remove(&self.local_addr);
    }
}

impl UdpSocketHandle for VirtualUdpSocketHandle {
    fn local_addr(&self) -> CoreResult<SocketAddr> {
        Ok(self.local_addr)
    }

    fn recv_from<'a>(
        &'a self,
        buf: &'a mut [u8],
    ) -> Pin<Box<dyn Future<Output = io::Result<(usize, SocketAddr)>> + Send + 'a>> {
        Box::pin(async move {
            let mut rx = self.rx.lock().await;
            match rx.recv().await {
                Some((pkt, peer)) => {
                    let copy_len = pkt.len().min(buf.len());
                    buf[..copy_len].copy_from_slice(&pkt[..copy_len]);
                    Ok((copy_len, peer))
                }
                None => Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "virtual UDP socket closed",
                )),
            }
        })
    }

    fn send_to<'a>(
        &'a self,
        data: &'a [u8],
        target: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = io::Result<usize>> + Send + 'a>> {
        let data_copy = data.to_vec();
        let hub = VirtualUdpHub {
            inner: Arc::clone(&self.inner),
        };
        let local = self.local_addr;
        let broadcast = self.broadcast;
        Box::pin(async move { hub.deliver_datagram(local, broadcast, target, &data_copy) })
    }
}

impl UdpSocketBackend for VirtualUdpHub {
    fn bind<'a>(
        &'a self,
        config: &'a ValidatedUdpConfig,
    ) -> Pin<Box<dyn Future<Output = CoreResult<Box<dyn UdpSocketHandle>>> + Send + 'a>> {
        let cfg = config.clone();
        let hub = self.clone();
        Box::pin(async move {
            let (handle, _) = hub.bind_internal(&cfg)?;
            Ok(Box::new(handle) as Box<dyn UdpSocketHandle>)
        })
    }
}

/// Test peer handle bound on a [`VirtualUdpHub`].
pub struct VirtualUdpPeer {
    handle: VirtualUdpSocketHandle,
    rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<(Bytes, SocketAddr)>>>,
}

impl VirtualUdpPeer {
    /// Returns the peer's assigned virtual [`SocketAddr`].
    pub fn local_addr(&self) -> SocketAddr {
        self.handle.local_addr
    }

    /// Sends a UDP datagram through the [`VirtualUdpHub`] to `target`.
    pub async fn send_to(&self, data: &[u8], target: SocketAddr) -> io::Result<usize> {
        self.handle.send_to(data, target).await
    }

    /// Receives the next datagram delivered to this peer.
    pub async fn recv(&self) -> Option<(Bytes, SocketAddr)> {
        self.rx.lock().await.recv().await
    }
}

/// Asynchronous UDP [`Transport`] supporting unicast, IPv4/IPv6 multicast, broadcast, and outbound TX.
pub struct UdpTransport {
    config: UdpConfig,
    validated: ValidatedUdpConfig,
    backend: Arc<dyn UdpSocketBackend>,
    bound_addr: Arc<Mutex<Option<SocketAddr>>>,
    state: Arc<StateTracker>,
}

impl std::fmt::Debug for UdpTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UdpTransport")
            .field("config", &self.config)
            .field("bound_addr", &self.bound_addr())
            .field("state", &self.state())
            .finish()
    }
}

impl UdpTransport {
    /// Creates a [`UdpTransport`] backed by the production OS `socket2` / `tokio::net` backend.
    pub fn new(config: UdpConfig) -> CoreResult<Self> {
        Self::with_backend(config, Arc::new(OsUdpBackend))
    }

    /// Creates a [`UdpTransport`] using a custom [`UdpSocketBackend`] (e.g., [`VirtualUdpHub`]).
    pub fn with_backend(
        config: UdpConfig,
        backend: Arc<dyn UdpSocketBackend>,
    ) -> CoreResult<Self> {
        let validated = config.validate()?;
        Ok(Self {
            config,
            validated,
            backend,
            bound_addr: Arc::new(Mutex::new(None)),
            state: Arc::new(StateTracker::new()),
        })
    }

    /// Returns the validated [`UdpConfig`].
    pub fn config(&self) -> &UdpConfig {
        &self.config
    }

    /// Returns the actual bound local socket address once started.
    pub fn bound_addr(&self) -> Option<SocketAddr> {
        *self.bound_addr.lock()
    }

    /// Returns the transport's live internal state.
    pub fn state(&self) -> TransportState {
        self.state.get()
    }

    /// Subscribes to live [`TransportState`] updates.
    pub fn subscribe_state(&self) -> watch::Receiver<TransportState> {
        self.state.subscribe_watch()
    }
}

impl Transport for UdpTransport {
    fn capabilities(&self) -> TransportCaps {
        TransportCaps {
            kind: TransportKind::Udp,
            supports_tx: self.validated.remote_tx_addr.is_some(),
            supports_seek: false,
            supports_raw_record: true,
            supports_auto_reconnect: false,
        }
    }

    fn start(
        &mut self,
        rx_sink: PacketSender,
        tx_rx: OutboundReceiver,
        mut shutdown_rx: ShutdownReceiver,
    ) -> CoreResult<JoinHandle<CoreResult<()>>> {
        let validated = self.config.validate()?;
        let backend = Arc::clone(&self.backend);
        let bound_addr_slot = Arc::clone(&self.bound_addr);
        let state = Arc::clone(&self.state);
        state.set(TransportState::Connecting);

        Ok(tokio::spawn(async move {
            let socket = match backend.bind(&validated).await {
                Ok(sock) => sock,
                Err(err) => {
                    state.set(TransportState::Error);
                    return Err(err);
                }
            };
            let local_addr = socket.local_addr()?;
            *bound_addr_slot.lock() = Some(local_addr);
            state.set(TransportState::Running);

            let mut emitter = RxPacketEmitter::new(
                rx_sink,
                BufferPoolConfig::default(),
                RxBackpressurePolicy::DropNewest,
            );
            let mut tx_consumer = TxStreamConsumer::new(tx_rx);
            let mut rx_buf = vec![0u8; validated.max_datagram_bytes];
            let remote_tx = validated.remote_tx_addr;

            loop {
                tokio::select! {
                    biased;
                    res = shutdown_rx.changed() => {
                        if res.is_err() || *shutdown_rx.borrow() {
                            state.set(TransportState::Stopped);
                            return Ok(());
                        }
                    }
                    recv_res = socket.recv_from(&mut rx_buf) => {
                        match recv_res {
                            Ok((n, peer_addr)) => {
                                let tag: Arc<str> = Arc::from(peer_addr.to_string());
                                if let Err(CoreError::ChannelClosed) = emitter.emit_slice(
                                    &rx_buf[..n],
                                    host_monotonic_ns(),
                                    Some(tag),
                                ) {
                                    state.set(TransportState::Stopped);
                                    return Ok(());
                                }
                            }
                            Err(err) => {
                                state.set(TransportState::Error);
                                return Err(CoreError::Io(err));
                            }
                        }
                    }
                    maybe_frame = tx_consumer.recv_next() => {
                        match maybe_frame {
                            Some(frame) => {
                                if let Some(target) = remote_tx {
                                    if let Err(err) = socket.send_to(&frame.payload, target).await {
                                        state.set(TransportState::Error);
                                        return Err(CoreError::Io(err));
                                    }
                                }
                            }
                            None => {
                                state.set(TransportState::Stopped);
                                return Ok(());
                            }
                        }
                    }
                }
            }
        }))
    }
}

/// Static [`TransportFactoryFn`](crate::plugin::TransportFactoryFn) creating a [`UdpTransport`] from JSON.
pub fn create_udp_transport(json_config: &str) -> CoreResult<Box<dyn Transport>> {
    let cfg: UdpConfig = serde_json::from_str(json_config)
        .map_err(|e| CoreError::InvalidConfig(format!("invalid UDP transport JSON: {e}")))?;
    Ok(Box::new(UdpTransport::new(cfg)?))
}

/// Configuration for [`TcpClientTransport`] matching `TransportConfigDto::TcpClient`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TcpClientConfig {
    pub remote_addr: String,
    #[serde(default)]
    pub auto_reconnect: bool,
    #[serde(default = "default_true")]
    pub tcp_nodelay: bool,
    #[serde(default = "default_tcp_reconnect_delay_ms")]
    pub reconnect_delay_ms: u64,
}

impl Default for TcpClientConfig {
    fn default() -> Self {
        Self {
            remote_addr: String::new(),
            auto_reconnect: false,
            tcp_nodelay: true,
            reconnect_delay_ms: DEFAULT_TCP_RECONNECT_DELAY_MS,
        }
    }
}

impl TcpClientConfig {
    /// Validates `remote_addr` syntax (`SocketAddr` or non-empty `host:port`).
    pub fn validate(&self) -> CoreResult<()> {
        validate_host_port_or_socket_addr(&self.remote_addr, "TcpClient remote_addr")
    }
}

/// Configuration for [`TcpServerTransport`] matching `TransportConfigDto::TcpServer`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TcpServerConfig {
    pub listen_addr: String,
    #[serde(default = "default_true")]
    pub tcp_nodelay: bool,
}

impl Default for TcpServerConfig {
    fn default() -> Self {
        Self {
            listen_addr: "127.0.0.1:0".to_string(),
            tcp_nodelay: true,
        }
    }
}

impl TcpServerConfig {
    /// Validates `listen_addr` as a parseable [`SocketAddr`].
    pub fn validate(&self) -> CoreResult<SocketAddr> {
        self.listen_addr.parse::<SocketAddr>().map_err(|e| {
            CoreError::InvalidConfig(format!(
                "invalid TcpServer listen_addr `{}`: {e}",
                self.listen_addr
            ))
        })
    }
}

fn validate_host_port_or_socket_addr(raw: &str, label: &str) -> CoreResult<()> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(CoreError::InvalidConfig(format!("{label} must not be empty")));
    }
    if trimmed.parse::<SocketAddr>().is_ok() {
        return Ok(());
    }
    if let Some((host, port_str)) = trimmed.rsplit_once(':') {
        if !host.is_empty() && port_str.parse::<u16>().is_ok_and(|p| p > 0) {
            return Ok(());
        }
    }
    Err(CoreError::InvalidConfig(format!(
        "invalid {label} `{raw}`: expected socket address or host:port"
    )))
}

/// Connected bidirectional async TCP stream pair.
pub struct ConnectedTcpStream {
    pub reader: Box<dyn AsyncRead + Unpin + Send>,
    pub writer: Box<dyn AsyncWrite + Unpin + Send>,
    pub peer_addr: Option<SocketAddr>,
}

/// Abstraction for outbound TCP connection establishment.
pub trait TcpStreamConnector: Send + Sync + 'static {
    fn connect<'a>(
        &'a self,
        remote_addr: &'a str,
        tcp_nodelay: bool,
    ) -> Pin<Box<dyn Future<Output = CoreResult<ConnectedTcpStream>> + Send + 'a>>;
}

/// Abstraction for an active TCP listener accepting incoming client connections.
pub trait TcpListenerHandle: Send + 'static {
    fn local_addr(&self) -> CoreResult<SocketAddr>;
    fn accept<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = CoreResult<ConnectedTcpStream>> + Send + 'a>>;
}

/// Abstraction for binding TCP servers.
pub trait TcpStreamAcceptor: Send + Sync + 'static {
    fn bind<'a>(
        &'a self,
        listen_addr: SocketAddr,
        tcp_nodelay: bool,
    ) -> BoxFuture<'a, CoreResult<Box<dyn TcpListenerHandle>>>;
}

/// Production OS TCP client connector using `tokio::net::TcpStream` + `TCP_NODELAY`.
#[derive(Debug, Default)]
pub struct OsTcpConnector;

impl TcpStreamConnector for OsTcpConnector {
    fn connect<'a>(
        &'a self,
        remote_addr: &'a str,
        tcp_nodelay: bool,
    ) -> Pin<Box<dyn Future<Output = CoreResult<ConnectedTcpStream>> + Send + 'a>> {
        let addr = remote_addr.to_string();
        Box::pin(async move {
            let stream = tokio::net::TcpStream::connect(&addr).await?;
            stream.set_nodelay(tcp_nodelay)?;
            let peer_addr = stream.peer_addr().ok();
            let (reader, writer) = tokio::io::split(stream);
            Ok(ConnectedTcpStream {
                reader: Box::new(reader),
                writer: Box::new(writer),
                peer_addr,
            })
        })
    }
}

/// Production OS TCP server acceptor using `socket2` + `tokio::net::TcpListener` + `TCP_NODELAY`.
#[derive(Debug, Default)]
pub struct OsTcpAcceptor;

struct OsTcpListenerHandle {
    listener: tokio::net::TcpListener,
    tcp_nodelay: bool,
}

impl TcpListenerHandle for OsTcpListenerHandle {
    fn local_addr(&self) -> CoreResult<SocketAddr> {
        Ok(self.listener.local_addr()?)
    }

    fn accept<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = CoreResult<ConnectedTcpStream>> + Send + 'a>> {
        let nodelay = self.tcp_nodelay;
        Box::pin(async move {
            let (stream, peer_addr) = self.listener.accept().await?;
            stream.set_nodelay(nodelay)?;
            let (reader, writer) = tokio::io::split(stream);
            Ok(ConnectedTcpStream {
                reader: Box::new(reader),
                writer: Box::new(writer),
                peer_addr: Some(peer_addr),
            })
        })
    }
}

impl TcpStreamAcceptor for OsTcpAcceptor {
    fn bind<'a>(
        &'a self,
        listen_addr: SocketAddr,
        tcp_nodelay: bool,
    ) -> Pin<Box<dyn Future<Output = CoreResult<Box<dyn TcpListenerHandle>>> + Send + 'a>> {
        Box::pin(async move {
            let domain = Domain::for_address(listen_addr);
            let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;
            socket.set_reuse_address(true)?;
            socket.set_nonblocking(true)?;
            socket.bind(&SockAddr::from(listen_addr))?;
            socket.listen(128)?;
            let std_listener: std::net::TcpListener = socket.into();
            let listener = tokio::net::TcpListener::from_std(std_listener)?;
            Ok(Box::new(OsTcpListenerHandle {
                listener,
                tcp_nodelay,
            }) as Box<dyn TcpListenerHandle>)
        })
    }
}

struct VirtualListenerEntry {
    tx: mpsc::UnboundedSender<ConnectedTcpStream>,
    tcp_nodelay: bool,
}

struct DuplexTcpNetworkInner {
    next_ephemeral_port: AtomicU16,
    next_client_port: AtomicU16,
    accepting: AtomicBool,
    listeners: Mutex<HashMap<SocketAddr, VirtualListenerEntry>>,
    active_killers: Mutex<Vec<oneshot::Sender<()>>>,
    nodelay_log: Mutex<Vec<(String, bool)>>,
}

/// Deterministic virtual TCP network backed by `tokio::io::duplex` implementing both
/// [`TcpStreamAcceptor`] and [`TcpStreamConnector`].
#[derive(Clone)]
pub struct DuplexTcpNetwork {
    inner: Arc<DuplexTcpNetworkInner>,
}

impl Default for DuplexTcpNetwork {
    fn default() -> Self {
        Self::new()
    }
}

impl DuplexTcpNetwork {
    /// Creates an online [`DuplexTcpNetwork`].
    pub fn new() -> Self {
        Self {
            inner: Arc::new(DuplexTcpNetworkInner {
                next_ephemeral_port: AtomicU16::new(40_000),
                next_client_port: AtomicU16::new(50_000),
                accepting: AtomicBool::new(true),
                listeners: Mutex::new(HashMap::new()),
                active_killers: Mutex::new(Vec::new()),
                nodelay_log: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Returns a [`TcpStreamConnector`] handle backed by this network.
    pub fn connector(&self) -> Arc<dyn TcpStreamConnector> {
        Arc::new(self.clone())
    }

    /// Returns a [`TcpStreamAcceptor`] handle backed by this network.
    pub fn acceptor(&self) -> Arc<dyn TcpStreamAcceptor> {
        Arc::new(self.clone())
    }

    /// Controls whether new `connect()` calls succeed or return `ConnectionRefused`.
    pub fn set_accepting(&self, accepting: bool) {
        self.inner.accepting.store(accepting, Ordering::Release);
    }

    /// Forcefully drops all active duplex TCP connections (simulating network drop).
    pub fn disconnect_all(&self) {
        let killers = std::mem::take(&mut *self.inner.active_killers.lock());
        for tx in killers {
            let _ = tx.send(());
        }
    }

    /// Returns `(role, tcp_nodelay)` pairs recorded during `bind` and `connect`.
    pub fn nodelay_log(&self) -> Vec<(String, bool)> {
        self.inner.nodelay_log.lock().clone()
    }

    /// Connects a standalone virtual TCP test client directly to `remote_addr`.
    pub async fn connect_raw(&self, remote_addr: SocketAddr) -> CoreResult<ConnectedTcpStream> {
        let addr_str = remote_addr.to_string();
        <Self as TcpStreamConnector>::connect(self, &addr_str, true).await
    }

    /// Binds a standalone virtual TCP test listener on `listen_addr`.
    pub async fn bind_raw(&self, listen_addr: SocketAddr) -> CoreResult<Box<dyn TcpListenerHandle>> {
        <Self as TcpStreamAcceptor>::bind(self, listen_addr, true).await
    }

    fn make_killable_pair(&self) -> (ConnectedTcpStream, ConnectedTcpStream, SocketAddr) {
        let (a_stream, b_stream) = tokio::io::duplex(64 * 1024);
        let (a_r, a_w) = tokio::io::split(a_stream);
        let (b_r, b_w) = tokio::io::split(b_stream);

        let (ka_r_tx, ka_r_rx) = oneshot::channel();
        let (ka_w_tx, ka_w_rx) = oneshot::channel();
        let (kb_r_tx, kb_r_rx) = oneshot::channel();
        let (kb_w_tx, kb_w_rx) = oneshot::channel();
        {
            let mut killers = self.inner.active_killers.lock();
            killers.push(ka_r_tx);
            killers.push(ka_w_tx);
            killers.push(kb_r_tx);
            killers.push(kb_w_tx);
        }

        let client_port = self.inner.next_client_port.fetch_add(1, Ordering::Relaxed);
        let client_addr: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), client_port);

        let a = ConnectedTcpStream {
            reader: Box::new(KillableReader::new(a_r, ka_r_rx)),
            writer: Box::new(KillableWriter::new(a_w, ka_w_rx)),
            peer_addr: None,
        };
        let b = ConnectedTcpStream {
            reader: Box::new(KillableReader::new(b_r, kb_r_rx)),
            writer: Box::new(KillableWriter::new(b_w, kb_w_rx)),
            peer_addr: Some(client_addr),
        };
        (a, b, client_addr)
    }
}

struct DuplexTcpListenerHandle {
    inner: Arc<DuplexTcpNetworkInner>,
    local_addr: SocketAddr,
    rx: mpsc::UnboundedReceiver<ConnectedTcpStream>,
}

impl Drop for DuplexTcpListenerHandle {
    fn drop(&mut self) {
        self.inner.listeners.lock().remove(&self.local_addr);
    }
}

impl TcpListenerHandle for DuplexTcpListenerHandle {
    fn local_addr(&self) -> CoreResult<SocketAddr> {
        Ok(self.local_addr)
    }

    fn accept<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = CoreResult<ConnectedTcpStream>> + Send + 'a>> {
        Box::pin(async move {
            self.rx.recv().await.ok_or_else(|| {
                CoreError::Io(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "virtual TCP listener closed",
                ))
            })
        })
    }
}

impl TcpStreamAcceptor for DuplexTcpNetwork {
    fn bind<'a>(
        &'a self,
        listen_addr: SocketAddr,
        tcp_nodelay: bool,
    ) -> Pin<Box<dyn Future<Output = CoreResult<Box<dyn TcpListenerHandle>>> + Send + 'a>> {
        let net = self.clone();
        Box::pin(async move {
            let mut listeners = net.inner.listeners.lock();
            let mut actual_addr = listen_addr;
            if actual_addr.port() == 0 {
                loop {
                    let port = net.inner.next_ephemeral_port.fetch_add(1, Ordering::Relaxed);
                    let candidate = SocketAddr::new(actual_addr.ip(), port);
                    if !listeners.contains_key(&candidate) {
                        actual_addr = candidate;
                        break;
                    }
                }
            } else if listeners.contains_key(&actual_addr) {
                return Err(CoreError::Io(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!("virtual TCP address {actual_addr} already in use"),
                )));
            }

            let (tx, rx) = mpsc::unbounded_channel();
            listeners.insert(actual_addr, VirtualListenerEntry { tx, tcp_nodelay });
            net.inner
                .nodelay_log
                .lock()
                .push((format!("bind:{actual_addr}"), tcp_nodelay));

            Ok(Box::new(DuplexTcpListenerHandle {
                inner: Arc::clone(&net.inner),
                local_addr: actual_addr,
                rx,
            }) as Box<dyn TcpListenerHandle>)
        })
    }
}

impl TcpStreamConnector for DuplexTcpNetwork {
    fn connect<'a>(
        &'a self,
        remote_addr: &'a str,
        tcp_nodelay: bool,
    ) -> Pin<Box<dyn Future<Output = CoreResult<ConnectedTcpStream>> + Send + 'a>> {
        let target_str = remote_addr.to_string();
        let net = self.clone();
        Box::pin(async move {
            if !net.inner.accepting.load(Ordering::Acquire) {
                return Err(CoreError::Io(io::Error::new(
                    io::ErrorKind::ConnectionRefused,
                    "virtual TCP network offline",
                )));
            }
            let target: SocketAddr = target_str.parse().map_err(|e| {
                CoreError::InvalidConfig(format!("invalid virtual TCP address `{target_str}`: {e}"))
            })?;

            let (server_tx, listener_nodelay) = {
                let listeners = net.inner.listeners.lock();
                if let Some(entry) = listeners.get(&target) {
                    (entry.tx.clone(), entry.tcp_nodelay)
                } else {
                    let wildcard_ip = match target {
                        SocketAddr::V4(_) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                        SocketAddr::V6(_) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
                    };
                    let wildcard = SocketAddr::new(wildcard_ip, target.port());
                    let entry = listeners.get(&wildcard).ok_or_else(|| {
                        CoreError::Io(io::Error::new(
                            io::ErrorKind::ConnectionRefused,
                            format!("no virtual TCP listener bound on {target}"),
                        ))
                    })?;
                    (entry.tx.clone(), entry.tcp_nodelay)
                }
            };

            let (mut client_side, server_side, _) = net.make_killable_pair();
            client_side.peer_addr = Some(target);
            net.inner
                .nodelay_log
                .lock()
                .push((format!("connect:{target}"), tcp_nodelay));
            let _ = listener_nodelay;

            server_tx.send(server_side).map_err(|_| {
                CoreError::Io(io::Error::new(
                    io::ErrorKind::ConnectionRefused,
                    "virtual TCP listener dropped",
                ))
            })?;
            Ok(client_side)
        })
    }
}

/// Asynchronous TCP client [`Transport`] with `TCP_NODELAY`, slab RX, TX draining, and auto-reconnect.
pub struct TcpClientTransport {
    config: TcpClientConfig,
    connector: Arc<dyn TcpStreamConnector>,
    state: Arc<StateTracker>,
}

impl std::fmt::Debug for TcpClientTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TcpClientTransport")
            .field("config", &self.config)
            .field("state", &self.state())
            .finish()
    }
}

impl TcpClientTransport {
    /// Creates a [`TcpClientTransport`] backed by the production OS TCP connector.
    pub fn new(config: TcpClientConfig) -> CoreResult<Self> {
        Self::with_connector(config, Arc::new(OsTcpConnector))
    }

    /// Creates a [`TcpClientTransport`] backed by a custom [`TcpStreamConnector`].
    pub fn with_connector(
        config: TcpClientConfig,
        connector: Arc<dyn TcpStreamConnector>,
    ) -> CoreResult<Self> {
        config.validate()?;
        Ok(Self {
            config,
            connector,
            state: Arc::new(StateTracker::new()),
        })
    }

    /// Returns the validated [`TcpClientConfig`].
    pub fn config(&self) -> &TcpClientConfig {
        &self.config
    }

    /// Returns the transport's live internal state.
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

impl Transport for TcpClientTransport {
    fn capabilities(&self) -> TransportCaps {
        TransportCaps {
            kind: TransportKind::TcpClient,
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
        let connector = Arc::clone(&self.connector);
        let state = Arc::clone(&self.state);
        state.set(TransportState::Connecting);

        Ok(tokio::spawn(async move {
            let mut emitter = RxPacketEmitter::new(
                rx_sink,
                BufferPoolConfig::default(),
                RxBackpressurePolicy::DropNewest,
            );
            let mut tx_consumer = TxStreamConsumer::new(tx_rx);
            let mut rx_buf = vec![0u8; DEFAULT_TCP_READ_CHUNK_BYTES];
            let reconnect_delay = Duration::from_millis(config.reconnect_delay_ms.max(1));

            loop {
                if *shutdown_rx.borrow() {
                    state.set(TransportState::Stopped);
                    return Ok(());
                }

                let connected = tokio::select! {
                    biased;
                    res = shutdown_rx.changed() => {
                        if res.is_err() || *shutdown_rx.borrow() {
                            state.set(TransportState::Stopped);
                            return Ok(());
                        }
                        continue;
                    }
                    res = connector.connect(&config.remote_addr, config.tcp_nodelay) => res,
                };

                let mut stream = match connected {
                    Ok(s) => {
                        state.set(TransportState::Running);
                        s
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

                let source_tag: Arc<str> = stream
                    .peer_addr
                    .map(|a| Arc::from(a.to_string()))
                    .unwrap_or_else(|| Arc::from(config.remote_addr.as_str()));

                let session_ended_by_shutdown = loop {
                    tokio::select! {
                        biased;
                        res = shutdown_rx.changed() => {
                            if res.is_err() || *shutdown_rx.borrow() {
                                break Ok(true);
                            }
                        }
                        read_res = stream.reader.read(&mut rx_buf) => {
                            match read_res {
                                Ok(0) => {
                                    break Err("TCP peer closed connection".to_string());
                                }
                                Ok(n) => {
                                    if let Err(CoreError::ChannelClosed) = emitter.emit_slice(
                                        &rx_buf[..n],
                                        host_monotonic_ns(),
                                        Some(Arc::clone(&source_tag)),
                                    ) {
                                        break Ok(true);
                                    }
                                }
                                Err(err) => {
                                    break Err(format!("TCP read error: {err}"));
                                }
                            }
                        }
                        maybe_frame = tx_consumer.recv_next() => {
                            match maybe_frame {
                                Some(frame) => {
                                    if let Err(err) = stream.writer.write_all(&frame.payload).await {
                                        break Err(format!("TCP write error: {err}"));
                                    }
                                    if let Err(err) = stream.writer.flush().await {
                                        break Err(format!("TCP flush error: {err}"));
                                    }
                                }
                                None => break Ok(true),
                            }
                        }
                    }
                };

                match session_ended_by_shutdown {
                    Ok(true) => {
                        state.set(TransportState::Stopped);
                        return Ok(());
                    }
                    Ok(false) => unreachable!(),
                    Err(reason) => {
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

/// Static [`TransportFactoryFn`](crate::plugin::TransportFactoryFn) creating a [`TcpClientTransport`] from JSON.
pub fn create_tcp_client_transport(json_config: &str) -> CoreResult<Box<dyn Transport>> {
    let cfg: TcpClientConfig = serde_json::from_str(json_config).map_err(|e| {
        CoreError::InvalidConfig(format!("invalid TcpClient transport JSON: {e}"))
    })?;
    Ok(Box::new(TcpClientTransport::new(cfg)?))
}

/// Asynchronous multi-client TCP server [`Transport`] stamping each packet with peer `source_tag`.
pub struct TcpServerTransport {
    config: TcpServerConfig,
    acceptor: Arc<dyn TcpStreamAcceptor>,
    bound_addr: Arc<Mutex<Option<SocketAddr>>>,
    connected_clients: Arc<AtomicUsize>,
    state: Arc<StateTracker>,
}

impl std::fmt::Debug for TcpServerTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TcpServerTransport")
            .field("config", &self.config)
            .field("bound_addr", &self.bound_addr())
            .field("connected_clients", &self.connected_client_count())
            .field("state", &self.state())
            .finish()
    }
}

impl TcpServerTransport {
    /// Creates a [`TcpServerTransport`] backed by the production OS TCP acceptor.
    pub fn new(config: TcpServerConfig) -> CoreResult<Self> {
        Self::with_acceptor(config, Arc::new(OsTcpAcceptor))
    }

    /// Creates a [`TcpServerTransport`] backed by a custom [`TcpStreamAcceptor`].
    pub fn with_acceptor(
        config: TcpServerConfig,
        acceptor: Arc<dyn TcpStreamAcceptor>,
    ) -> CoreResult<Self> {
        let _ = config.validate()?;
        Ok(Self {
            config,
            acceptor,
            bound_addr: Arc::new(Mutex::new(None)),
            connected_clients: Arc::new(AtomicUsize::new(0)),
            state: Arc::new(StateTracker::new()),
        })
    }

    /// Returns the validated [`TcpServerConfig`].
    pub fn config(&self) -> &TcpServerConfig {
        &self.config
    }

    /// Returns the actual bound listener address once started.
    pub fn bound_addr(&self) -> Option<SocketAddr> {
        *self.bound_addr.lock()
    }

    /// Returns the current number of active connected TCP clients.
    pub fn connected_client_count(&self) -> usize {
        self.connected_clients.load(Ordering::Acquire)
    }

    /// Returns the transport's live internal state.
    pub fn state(&self) -> TransportState {
        self.state.get()
    }

    /// Subscribes to live [`TransportState`] updates.
    pub fn subscribe_state(&self) -> watch::Receiver<TransportState> {
        self.state.subscribe_watch()
    }
}

impl Transport for TcpServerTransport {
    fn capabilities(&self) -> TransportCaps {
        TransportCaps {
            kind: TransportKind::TcpServer,
            supports_tx: true,
            supports_seek: false,
            supports_raw_record: true,
            supports_auto_reconnect: false,
        }
    }

    fn start(
        &mut self,
        rx_sink: PacketSender,
        tx_rx: OutboundReceiver,
        mut shutdown_rx: ShutdownReceiver,
    ) -> CoreResult<JoinHandle<CoreResult<()>>> {
        let listen_addr = self.config.validate()?;
        let tcp_nodelay = self.config.tcp_nodelay;
        let acceptor = Arc::clone(&self.acceptor);
        let bound_addr_slot = Arc::clone(&self.bound_addr);
        let connected_clients = Arc::clone(&self.connected_clients);
        let state = Arc::clone(&self.state);
        state.set(TransportState::Connecting);

        Ok(tokio::spawn(async move {
            let mut listener = match acceptor.bind(listen_addr, tcp_nodelay).await {
                Ok(l) => l,
                Err(err) => {
                    state.set(TransportState::Error);
                    return Err(err);
                }
            };
            let local_addr = listener.local_addr()?;
            *bound_addr_slot.lock() = Some(local_addr);
            connected_clients.store(0, Ordering::Release);
            state.set(TransportState::Running);

            let emitter = Arc::new(Mutex::new(RxPacketEmitter::new(
                rx_sink,
                BufferPoolConfig::default(),
                RxBackpressurePolicy::DropNewest,
            )));
            let clients: Arc<Mutex<HashMap<u64, mpsc::Sender<Bytes>>>> =
                Arc::new(Mutex::new(HashMap::new()));
            let next_client_id = AtomicU64::new(1);
            let mut client_tasks: Vec<JoinHandle<()>> = Vec::new();
            let mut tx_consumer = TxStreamConsumer::new(tx_rx);

            loop {
                tokio::select! {
                    biased;
                    res = shutdown_rx.changed() => {
                        if res.is_err() || *shutdown_rx.borrow() {
                            break;
                        }
                    }
                    accept_res = listener.accept() => {
                        match accept_res {
                            Ok(client) => {
                                let client_id = next_client_id.fetch_add(1, Ordering::Relaxed);
                                let peer_tag: Arc<str> = client
                                    .peer_addr
                                    .map(|a| Arc::from(a.to_string()))
                                    .unwrap_or_else(|| Arc::from(format!("client-{client_id}")));
                                let (client_tx, client_rx) = mpsc::channel::<Bytes>(256);
                                clients.lock().insert(client_id, client_tx);
                                connected_clients.fetch_add(1, Ordering::AcqRel);

                                let task = spawn_tcp_server_client_task(
                                    client_id,
                                    client,
                                    peer_tag,
                                    Arc::clone(&emitter),
                                    Arc::clone(&clients),
                                    Arc::clone(&connected_clients),
                                    client_rx,
                                    shutdown_rx.clone(),
                                );
                                client_tasks.retain(|h| !h.is_finished());
                                client_tasks.push(task);
                            }
                            Err(err) => {
                                cleanup_server_clients(&clients, &connected_clients, &mut client_tasks).await;
                                state.set(TransportState::Error);
                                return Err(err);
                            }
                        }
                    }
                    maybe_frame = tx_consumer.recv_next() => {
                        match maybe_frame {
                            Some(frame) => {
                                let targets: Vec<(u64, mpsc::Sender<Bytes>)> = clients
                                    .lock()
                                    .iter()
                                    .map(|(&id, tx)| (id, tx.clone()))
                                    .collect();
                                for (id, tx) in targets {
                                    if tx.send(frame.payload.clone()).await.is_err()
                                        && clients.lock().remove(&id).is_some()
                                    {
                                        connected_clients.fetch_sub(1, Ordering::AcqRel);
                                    }
                                }
                            }
                            None => break,
                        }
                    }
                }
            }

            cleanup_server_clients(&clients, &connected_clients, &mut client_tasks).await;
            state.set(TransportState::Stopped);
            Ok(())
        }))
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_tcp_server_client_task(
    client_id: u64,
    mut stream: ConnectedTcpStream,
    peer_tag: Arc<str>,
    emitter: Arc<Mutex<RxPacketEmitter>>,
    clients: Arc<Mutex<HashMap<u64, mpsc::Sender<Bytes>>>>,
    connected_clients: Arc<AtomicUsize>,
    mut client_rx: mpsc::Receiver<Bytes>,
    mut shutdown_rx: ShutdownReceiver,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut rx_buf = vec![0u8; DEFAULT_TCP_READ_CHUNK_BYTES];
        loop {
            tokio::select! {
                biased;
                res = shutdown_rx.changed() => {
                    if res.is_err() || *shutdown_rx.borrow() {
                        break;
                    }
                }
                read_res = stream.reader.read(&mut rx_buf) => {
                    match read_res {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let ts = host_monotonic_ns();
                            let emit_res = emitter.lock().emit_slice(
                                &rx_buf[..n],
                                ts,
                                Some(Arc::clone(&peer_tag)),
                            );
                            if let Err(CoreError::ChannelClosed) = emit_res {
                                break;
                            }
                        }
                    }
                }
                maybe_tx = client_rx.recv() => {
                    match maybe_tx {
                        Some(payload) => {
                            if stream.writer.write_all(&payload).await.is_err()
                                || stream.writer.flush().await.is_err()
                            {
                                break;
                            }
                        }
                        None => break,
                    }
                }
            }
        }

        if clients.lock().remove(&client_id).is_some() {
            connected_clients.fetch_sub(1, Ordering::AcqRel);
        }
    })
}

async fn cleanup_server_clients(
    clients: &Arc<Mutex<HashMap<u64, mpsc::Sender<Bytes>>>>,
    connected_clients: &Arc<AtomicUsize>,
    client_tasks: &mut Vec<JoinHandle<()>>,
) {
    clients.lock().clear();
    for handle in client_tasks.drain(..) {
        let _ = handle.await;
    }
    connected_clients.store(0, Ordering::Release);
}

/// Static [`TransportFactoryFn`](crate::plugin::TransportFactoryFn) creating a [`TcpServerTransport`] from JSON.
pub fn create_tcp_server_transport(json_config: &str) -> CoreResult<Box<dyn Transport>> {
    let cfg: TcpServerConfig = serde_json::from_str(json_config).map_err(|e| {
        CoreError::InvalidConfig(format!("invalid TcpServer transport JSON: {e}"))
    })?;
    Ok(Box::new(TcpServerTransport::new(cfg)?))
}
