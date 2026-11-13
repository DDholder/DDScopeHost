//! Opaque-box E2E integration tests for Milestone TB3 Part 1:
//! `SerialTransport`, `UdpTransport`, `TcpClientTransport`, and `TcpServerTransport`.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use ddscope_core::transport::{
    configure_os_udp_socket, create_serial_transport, create_tcp_client_transport,
    create_tcp_server_transport, create_udp_transport, list_serial_ports, parse_usb_vid_pid,
    DuplexTcpNetwork, OsTcpAcceptor, OsUdpBackend, OutboundFrame, SerialConfig, SerialFlowControl,
    SerialParity, SerialStopBits, SerialTransport, TcpClientConfig, TcpClientTransport,
    TcpServerConfig, TcpServerTransport, TcpStreamAcceptor, Transport, TransportKind,
    TransportRegistry, TransportRuntimeConfig, TransportScheduler, TransportState, TxPriority,
    UdpConfig, UdpSocketBackend, UdpTransport, VirtualSerialPair, VirtualUdpHub,
    MAX_SERIAL_BAUD_RATE,
};
use ddscope_core::types::CoreError;

async fn wait_for_state<F>(mut get_state: F, target: TransportState, timeout: Duration)
where
    F: FnMut() -> TransportState,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if get_state() == target {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for state {target:?}, last state was {:?}",
            get_state()
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[test]
fn t1_serial_config_validation_vid_pid_parsing_and_enumeration() {
    let valid_12m = SerialConfig {
        port_name: "COM7".to_string(),
        baud_rate: MAX_SERIAL_BAUD_RATE,
        data_bits: 8,
        parity: SerialParity::Even,
        stop_bits: SerialStopBits::Two,
        flow_control: SerialFlowControl::HardwareRtsCts,
        dtr_on_open: true,
        auto_reconnect: true,
        reconnect_delay_ms: 100,
    };
    assert!(valid_12m.validate().is_ok());

    for bits in 5..=8 {
        let cfg = SerialConfig {
            port_name: "/dev/ttyACM0".to_string(),
            baud_rate: 921_600,
            data_bits: bits,
            flow_control: SerialFlowControl::SoftwareXonXoff,
            ..Default::default()
        };
        assert!(cfg.validate().is_ok());
    }

    let empty_port = SerialConfig {
        port_name: "   ".to_string(),
        ..Default::default()
    };
    assert!(matches!(empty_port.validate(), Err(CoreError::InvalidConfig(_))));

    let over_baud = SerialConfig {
        port_name: "COM1".to_string(),
        baud_rate: MAX_SERIAL_BAUD_RATE + 1,
        ..Default::default()
    };
    assert!(matches!(over_baud.validate(), Err(CoreError::InvalidConfig(_))));

    let zero_baud = SerialConfig {
        port_name: "COM1".to_string(),
        baud_rate: 0,
        ..Default::default()
    };
    assert!(matches!(zero_baud.validate(), Err(CoreError::InvalidConfig(_))));

    let bad_bits = SerialConfig {
        port_name: "COM1".to_string(),
        data_bits: 9,
        ..Default::default()
    };
    assert!(matches!(bad_bits.validate(), Err(CoreError::InvalidConfig(_))));

    assert_eq!(
        parse_usb_vid_pid(r"USB\VID_0483&PID_5740\307237703431"),
        (Some(0x0483), Some(0x5740))
    );
    assert_eq!(
        parse_usb_vid_pid(r"FTDIBUS\vid_0403+pid_6001+A50285BI\0000"),
        (Some(0x0403), Some(0x6001))
    );
    assert_eq!(parse_usb_vid_pid("ACPI\\PNP0501\\1"), (None, None));

    // OS enumeration must succeed cleanly even when no physical adapters are attached.
    let ports = list_serial_ports().expect("list_serial_ports must not error");
    for port in &ports {
        assert!(!port.port_name.is_empty());
    }
}

#[tokio::test]
async fn t2_serial_transport_full_duplex_and_hotplug_auto_reconnect() {
    let virtual_port = VirtualSerialPair::new("COM_VIRT_1");
    let mut peer1 = virtual_port.take_peer().expect("initial peer must exist");

    let cfg = SerialConfig {
        port_name: "COM_VIRT_1".to_string(),
        baud_rate: 12_000_000,
        data_bits: 8,
        parity: SerialParity::None,
        stop_bits: SerialStopBits::One,
        flow_control: SerialFlowControl::HardwareRtsCts,
        dtr_on_open: true,
        auto_reconnect: true,
        reconnect_delay_ms: 20,
    };

    let transport = SerialTransport::with_provider(cfg.clone(), virtual_port.provider())
        .expect("serial transport should construct");
    let caps = transport.capabilities();
    assert_eq!(caps.kind, TransportKind::Serial);
    assert!(caps.supports_tx);
    assert!(!caps.supports_seek);
    assert!(caps.supports_raw_record);
    assert!(caps.supports_auto_reconnect);

    let mut state_events = transport.subscribe_state_events();
    let state_watch = transport.subscribe_state();
    assert_eq!(*state_watch.borrow(), TransportState::Stopped);

    let mut scheduler =
        TransportScheduler::new(Box::new(transport), TransportRuntimeConfig::default());
    let rx = scheduler.start().expect("scheduler start must succeed");

    // Wait until session #1 reaches Running.
    wait_for_state(
        || *state_watch.borrow(),
        TransportState::Running,
        Duration::from_secs(2),
    )
    .await;
    assert_eq!(virtual_port.open_count(), 1);
    assert_eq!(virtual_port.last_config(), Some(cfg));

    // Verify RX path via RxPacketEmitter + slab pool.
    peer1.send_rx(b"SERIAL_RX_1\n").await.unwrap();
    let pkt1 = tokio::task::spawn_blocking(move || {
        let p = rx.recv_timeout(Duration::from_secs(2)).expect("RX packet expected");
        (p, rx)
    })
    .await
    .unwrap();
    let (pkt1, rx) = pkt1;
    assert_eq!(&pkt1.bytes[..], b"SERIAL_RX_1\n");
    assert_eq!(pkt1.source_tag.as_deref(), Some("COM_VIRT_1"));

    // Verify TX path through TransportScheduler -> SerialTransport -> VirtualSerialPeer.
    scheduler
        .submit_tx(OutboundFrame::fire_and_forget(
            Bytes::from_static(b"SERIAL_TX_1\n"),
            TxPriority::High,
        ))
        .unwrap();
    let mut tx_buf = [0u8; 64];
    let n = tokio::time::timeout(Duration::from_secs(2), peer1.recv_tx(&mut tx_buf))
        .await
        .expect("TX read timeout")
        .expect("TX read should succeed");
    assert_eq!(&tx_buf[..n], b"SERIAL_TX_1\n");

    // Simulate hot-plug USB unplug -> transport transitions Running -> Reconnecting.
    virtual_port.unplug();
    wait_for_state(
        || *state_watch.borrow(),
        TransportState::Reconnecting,
        Duration::from_secs(2),
    )
    .await;

    // Wait across multiple retry ticks while still unplugged.
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(*state_watch.borrow(), TransportState::Reconnecting);
    assert_eq!(virtual_port.open_count(), 1);

    // Simulate cable replug -> transport transitions Reconnecting -> Running.
    let mut peer2 = virtual_port.replug();
    wait_for_state(
        || *state_watch.borrow(),
        TransportState::Running,
        Duration::from_secs(2),
    )
    .await;
    assert_eq!(virtual_port.open_count(), 2);

    // Full-duplex traffic resumes seamlessly on replugged port.
    peer2.send_rx(b"SERIAL_RX_2\n").await.unwrap();
    let pkt2 = tokio::task::spawn_blocking(move || rx.recv_timeout(Duration::from_secs(2)).unwrap())
        .await
        .unwrap();
    assert_eq!(&pkt2.bytes[..], b"SERIAL_RX_2\n");
    assert_eq!(pkt2.source_tag.as_deref(), Some("COM_VIRT_1"));

    scheduler
        .submit_tx(OutboundFrame::fire_and_forget(
            Bytes::from_static(b"SERIAL_TX_2\n"),
            TxPriority::Normal,
        ))
        .unwrap();
    let n2 = tokio::time::timeout(Duration::from_secs(2), peer2.recv_tx(&mut tx_buf))
        .await
        .expect("TX2 read timeout")
        .expect("TX2 read should succeed");
    assert_eq!(&tx_buf[..n2], b"SERIAL_TX_2\n");

    scheduler.stop().await.expect("scheduler stop must succeed");
    assert_eq!(*state_watch.borrow(), TransportState::Stopped);

    let mut observed = Vec::new();
    while let Ok(ev) = state_events.try_recv() {
        observed.push(ev);
    }
    assert_eq!(
        observed,
        vec![
            TransportState::Connecting,
            TransportState::Running,
            TransportState::Reconnecting,
            TransportState::Running,
            TransportState::Stopped,
        ]
    );
}

#[tokio::test]
async fn t3_serial_transport_no_reconnect_errors_on_unplug() {
    let virtual_port = VirtualSerialPair::new("COM_NO_RECONNECT");
    let _peer = virtual_port.take_peer().unwrap();

    let cfg = SerialConfig {
        port_name: "COM_NO_RECONNECT".to_string(),
        baud_rate: 115_200,
        data_bits: 8,
        auto_reconnect: false,
        ..Default::default()
    };
    let transport = SerialTransport::with_provider(cfg, virtual_port.provider()).unwrap();
    let state_watch = transport.subscribe_state();

    let mut scheduler =
        TransportScheduler::new(Box::new(transport), TransportRuntimeConfig::default());
    let _rx = scheduler.start().unwrap();
    wait_for_state(
        || *state_watch.borrow(),
        TransportState::Running,
        Duration::from_secs(2),
    )
    .await;

    virtual_port.unplug();
    wait_for_state(
        || *state_watch.borrow(),
        TransportState::Error,
        Duration::from_secs(2),
    )
    .await;

    let stop_res = scheduler.stop().await;
    assert!(matches!(stop_res, Err(CoreError::Transport(_))));
}

#[tokio::test]
async fn t4_real_os_socket2_options_and_bounded_loopback_probe() {
    let udp_cfg = UdpConfig {
        bind_addr: "127.0.0.1:0".to_string(),
        multicast_group: None,
        recv_buffer_bytes: 2 * 1024 * 1024,
        remote_tx_addr: None,
        broadcast: true,
        max_datagram_bytes: 65_535,
    };
    let validated = udp_cfg.validate().unwrap();
    let sock2 = configure_os_udp_socket(&validated).expect("socket2 UDP setup must succeed");
    assert!(sock2.broadcast().unwrap());
    let rcvbuf = sock2.recv_buffer_size().unwrap();
    assert!(rcvbuf >= 65_536, "OS recv buffer should be configured, got {rcvbuf}");
    let local_udp = sock2.local_addr().unwrap().as_socket().unwrap();
    assert_ne!(local_udp.port(), 0);
    drop(sock2);

    // Real OsUdpBackend + OsTcpAcceptor bind & bounded 100ms probe (never hangs in AppContainer).
    let os_udp = OsUdpBackend;
    let bound_udp = os_udp.bind(&validated).await.expect("OsUdpBackend bind must succeed");
    let udp_addr = bound_udp.local_addr().unwrap();
    assert_ne!(udp_addr.port(), 0);
    let _ = bound_udp.send_to(b"PROBE", udp_addr).await;
    let mut probe_buf = [0u8; 32];
    let _ = tokio::time::timeout(
        Duration::from_millis(100),
        bound_udp.recv_from(&mut probe_buf),
    )
    .await;

    let os_tcp = OsTcpAcceptor;
    let mut listener = os_tcp
        .bind("127.0.0.1:0".parse().unwrap(), true)
        .await
        .expect("OsTcpAcceptor bind must succeed");
    let tcp_addr = listener.local_addr().unwrap();
    assert_ne!(tcp_addr.port(), 0);
    let _ = tokio::time::timeout(
        Duration::from_millis(100),
        tokio::net::TcpStream::connect(tcp_addr),
    )
    .await;
    let _ = tokio::time::timeout(Duration::from_millis(50), listener.accept()).await;
}

#[tokio::test]
async fn t5_udp_transport_unicast_tx_rx_and_peer_source_tags() {
    let hub = VirtualUdpHub::new();
    let peer_a = hub
        .bind_peer("127.0.0.1:0".parse().unwrap(), false, None)
        .unwrap();
    let peer_b = hub
        .bind_peer("127.0.0.1:0".parse().unwrap(), false, None)
        .unwrap();

    let cfg = UdpConfig {
        bind_addr: "127.0.0.1:0".to_string(),
        multicast_group: None,
        recv_buffer_bytes: 4 * 1024 * 1024,
        remote_tx_addr: Some(peer_a.local_addr().to_string()),
        broadcast: false,
        max_datagram_bytes: 65_535,
    };
    let transport = UdpTransport::with_backend(cfg, hub.backend()).unwrap();
    let caps = transport.capabilities();
    assert_eq!(caps.kind, TransportKind::Udp);
    assert!(caps.supports_tx);
    assert!(!caps.supports_seek);
    assert!(caps.supports_raw_record);
    assert!(!caps.supports_auto_reconnect);

    let state_watch = transport.subscribe_state();
    let mut scheduler =
        TransportScheduler::new(Box::new(transport), TransportRuntimeConfig::default());
    let rx = scheduler.start().unwrap();
    wait_for_state(
        || *state_watch.borrow(),
        TransportState::Running,
        Duration::from_secs(2),
    )
    .await;

    let bindings = hub.binding_history();
    let server_binding = bindings.last().unwrap();
    assert_eq!(server_binding.recv_buffer_bytes, 4 * 1024 * 1024);
    let target_addr = server_binding.local_addr;

    peer_a.send_to(b"FROM_A", target_addr).await.unwrap();
    peer_b.send_to(b"FROM_B", target_addr).await.unwrap();

    let (pkt_a, pkt_b, rx) = tokio::task::spawn_blocking(move || {
        let a = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let b = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        (a, b, rx)
    })
    .await
    .unwrap();
    drop(rx);

    assert_eq!(&pkt_a.bytes[..], b"FROM_A");
    assert_eq!(pkt_a.source_tag.as_deref(), Some(peer_a.local_addr().to_string().as_str()));
    assert_eq!(&pkt_b.bytes[..], b"FROM_B");
    assert_eq!(pkt_b.source_tag.as_deref(), Some(peer_b.local_addr().to_string().as_str()));

    scheduler
        .submit_tx(OutboundFrame::fire_and_forget(
            Bytes::from_static(b"TO_A_CMD"),
            TxPriority::High,
        ))
        .unwrap();
    let (reply, from) = tokio::time::timeout(Duration::from_secs(2), peer_a.recv())
        .await
        .expect("peer_a recv timeout")
        .expect("peer_a must receive packet");
    assert_eq!(&reply[..], b"TO_A_CMD");
    assert_eq!(from, target_addr);

    scheduler.stop().await.unwrap();
}

#[tokio::test]
async fn t6_udp_transport_multicast_v4_v6_and_broadcast() {
    // Validation guards for multicast family and non-multicast IP.
    let bad_mcast = UdpConfig {
        bind_addr: "0.0.0.0:45000".to_string(),
        multicast_group: Some("192.168.1.1".to_string()),
        ..Default::default()
    };
    assert!(matches!(bad_mcast.validate(), Err(CoreError::InvalidConfig(_))));

    let family_mismatch = UdpConfig {
        bind_addr: "0.0.0.0:45000".to_string(),
        multicast_group: Some("ff12::1".to_string()),
        ..Default::default()
    };
    assert!(matches!(
        family_mismatch.validate(),
        Err(CoreError::InvalidConfig(_))
    ));

    let hub = VirtualUdpHub::new();

    // IPv4 Multicast
    let mcast_v4: IpAddr = "239.255.10.1".parse().unwrap();
    let v4_cfg = UdpConfig {
        bind_addr: "0.0.0.0:45100".to_string(),
        multicast_group: Some(mcast_v4.to_string()),
        recv_buffer_bytes: 1024 * 1024,
        remote_tx_addr: None,
        broadcast: false,
        max_datagram_bytes: 4096,
    };
    let v4_transport = UdpTransport::with_backend(v4_cfg, hub.backend()).unwrap();
    let v4_state = v4_transport.subscribe_state();
    let mut v4_sched =
        TransportScheduler::new(Box::new(v4_transport), TransportRuntimeConfig::default());
    let v4_rx = v4_sched.start().unwrap();
    wait_for_state(|| *v4_state.borrow(), TransportState::Running, Duration::from_secs(2)).await;

    let sender_v4 = hub
        .bind_peer("127.0.0.1:0".parse().unwrap(), false, None)
        .unwrap();
    sender_v4
        .send_to(b"MCAST_V4_DATA", SocketAddr::new(mcast_v4, 45100))
        .await
        .unwrap();
    let pkt_v4 = tokio::task::spawn_blocking(move || v4_rx.recv_timeout(Duration::from_secs(2)).unwrap())
        .await
        .unwrap();
    assert_eq!(&pkt_v4.bytes[..], b"MCAST_V4_DATA");
    v4_sched.stop().await.unwrap();

    // IPv6 Multicast
    let mcast_v6: IpAddr = "ff12::42".parse().unwrap();
    let v6_cfg = UdpConfig {
        bind_addr: "[::]:45101".to_string(),
        multicast_group: Some(mcast_v6.to_string()),
        recv_buffer_bytes: 1024 * 1024,
        remote_tx_addr: None,
        broadcast: false,
        max_datagram_bytes: 4096,
    };
    let v6_transport = UdpTransport::with_backend(v6_cfg, hub.backend()).unwrap();
    let v6_state = v6_transport.subscribe_state();
    let mut v6_sched =
        TransportScheduler::new(Box::new(v6_transport), TransportRuntimeConfig::default());
    let v6_rx = v6_sched.start().unwrap();
    wait_for_state(|| *v6_state.borrow(), TransportState::Running, Duration::from_secs(2)).await;

    let sender_v6 = hub
        .bind_peer("[::1]:0".parse().unwrap(), false, None)
        .unwrap();
    sender_v6
        .send_to(b"MCAST_V6_DATA", SocketAddr::new(mcast_v6, 45101))
        .await
        .unwrap();
    let pkt_v6 = tokio::task::spawn_blocking(move || v6_rx.recv_timeout(Duration::from_secs(2)).unwrap())
        .await
        .unwrap();
    assert_eq!(&pkt_v6.bytes[..], b"MCAST_V6_DATA");
    v6_sched.stop().await.unwrap();

    // Broadcast RX + TX
    let bcast_peer = hub
        .bind_peer("127.0.0.1:45102".parse().unwrap(), true, None)
        .unwrap();
    let bcast_cfg = UdpConfig {
        bind_addr: "0.0.0.0:45102".to_string(),
        multicast_group: None,
        recv_buffer_bytes: 1024 * 1024,
        remote_tx_addr: Some("255.255.255.255:45102".to_string()),
        broadcast: true,
        max_datagram_bytes: 4096,
    };
    let bcast_transport = UdpTransport::with_backend(bcast_cfg, hub.backend()).unwrap();
    let bcast_state = bcast_transport.subscribe_state();
    let mut bcast_sched =
        TransportScheduler::new(Box::new(bcast_transport), TransportRuntimeConfig::default());
    let bcast_rx = bcast_sched.start().unwrap();
    wait_for_state(
        || *bcast_state.borrow(),
        TransportState::Running,
        Duration::from_secs(2),
    )
    .await;

    bcast_peer
        .send_to(
            b"BCAST_PING",
            SocketAddr::new(IpAddr::V4(Ipv4Addr::BROADCAST), 45102),
        )
        .await
        .unwrap();
    let pkt_bcast =
        tokio::task::spawn_blocking(move || bcast_rx.recv_timeout(Duration::from_secs(2)).unwrap())
            .await
            .unwrap();
    assert_eq!(&pkt_bcast.bytes[..], b"BCAST_PING");

    bcast_sched
        .submit_tx(OutboundFrame::fire_and_forget(
            Bytes::from_static(b"BCAST_OUT"),
            TxPriority::Normal,
        ))
        .unwrap();
    let (out_data, _) = tokio::time::timeout(Duration::from_secs(2), bcast_peer.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&out_data[..], b"BCAST_OUT");
    bcast_sched.stop().await.unwrap();
}

#[tokio::test]
async fn t7_tcp_client_full_duplex_nodelay_and_auto_reconnect() {
    let network = DuplexTcpNetwork::new();
    let mut server_listener = network
        .bind_raw("127.0.0.1:46000".parse().unwrap())
        .await
        .unwrap();

    let cfg = TcpClientConfig {
        remote_addr: "127.0.0.1:46000".to_string(),
        auto_reconnect: true,
        tcp_nodelay: true,
        reconnect_delay_ms: 20,
    };
    let transport = TcpClientTransport::with_connector(cfg, network.connector()).unwrap();
    let caps = transport.capabilities();
    assert_eq!(caps.kind, TransportKind::TcpClient);
    assert!(caps.supports_tx);
    assert!(!caps.supports_seek);
    assert!(caps.supports_raw_record);
    assert!(caps.supports_auto_reconnect);

    let mut state_events = transport.subscribe_state_events();
    let state_watch = transport.subscribe_state();

    let mut scheduler =
        TransportScheduler::new(Box::new(transport), TransportRuntimeConfig::default());
    let rx = scheduler.start().unwrap();

    let mut conn1 = tokio::time::timeout(Duration::from_secs(2), server_listener.accept())
        .await
        .unwrap()
        .unwrap();
    wait_for_state(
        || *state_watch.borrow(),
        TransportState::Running,
        Duration::from_secs(2),
    )
    .await;

    assert!(network
        .nodelay_log()
        .iter()
        .any(|(role, nodelay)| role == "connect:127.0.0.1:46000" && *nodelay));

    conn1.writer.write_all(b"TCP_CLIENT_RX_1").await.unwrap();
    conn1.writer.flush().await.unwrap();
    let (pkt1, rx) = tokio::task::spawn_blocking(move || {
        (rx.recv_timeout(Duration::from_secs(2)).unwrap(), rx)
    })
    .await
    .unwrap();
    assert_eq!(&pkt1.bytes[..], b"TCP_CLIENT_RX_1");
    assert_eq!(pkt1.source_tag.as_deref(), Some("127.0.0.1:46000"));

    scheduler
        .submit_tx(OutboundFrame::fire_and_forget(
            Bytes::from_static(b"TCP_CLIENT_TX_1"),
            TxPriority::High,
        ))
        .unwrap();
    let mut buf = [0u8; 64];
    let n = tokio::time::timeout(Duration::from_secs(2), conn1.reader.read(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&buf[..n], b"TCP_CLIENT_TX_1");

    // Simulate network drop while listener refuses, verify Reconnecting, then allow reconnect.
    network.set_accepting(false);
    network.disconnect_all();
    wait_for_state(
        || *state_watch.borrow(),
        TransportState::Reconnecting,
        Duration::from_secs(2),
    )
    .await;

    network.set_accepting(true);
    let mut conn2 = tokio::time::timeout(Duration::from_secs(2), server_listener.accept())
        .await
        .unwrap()
        .unwrap();
    wait_for_state(
        || *state_watch.borrow(),
        TransportState::Running,
        Duration::from_secs(2),
    )
    .await;

    conn2.writer.write_all(b"TCP_CLIENT_RX_2").await.unwrap();
    conn2.writer.flush().await.unwrap();
    let pkt2 =
        tokio::task::spawn_blocking(move || rx.recv_timeout(Duration::from_secs(2)).unwrap())
            .await
            .unwrap();
    assert_eq!(&pkt2.bytes[..], b"TCP_CLIENT_RX_2");

    scheduler.stop().await.unwrap();
    assert_eq!(*state_watch.borrow(), TransportState::Stopped);

    let mut observed = Vec::new();
    while let Ok(ev) = state_events.try_recv() {
        observed.push(ev);
    }
    assert_eq!(
        observed,
        vec![
            TransportState::Connecting,
            TransportState::Running,
            TransportState::Reconnecting,
            TransportState::Running,
            TransportState::Stopped,
        ]
    );
}

#[tokio::test]
async fn t8_tcp_server_multi_client_source_tag_tx_broadcast_and_cleanup() {
    let network = DuplexTcpNetwork::new();
    let cfg = TcpServerConfig {
        listen_addr: "127.0.0.1:0".to_string(),
        tcp_nodelay: true,
    };
    let transport = TcpServerTransport::with_acceptor(cfg, network.acceptor()).unwrap();
    let caps = transport.capabilities();
    assert_eq!(caps.kind, TransportKind::TcpServer);
    assert!(caps.supports_tx);
    assert!(!caps.supports_seek);
    assert!(caps.supports_raw_record);
    assert!(!caps.supports_auto_reconnect);

    let state_watch = transport.subscribe_state();
    // Keep lightweight access via bound_addr / client count before boxing: wait via network/state.
    let mut scheduler =
        TransportScheduler::new(Box::new(transport), TransportRuntimeConfig::default());
    let rx = scheduler.start().unwrap();
    wait_for_state(
        || *state_watch.borrow(),
        TransportState::Running,
        Duration::from_secs(2),
    )
    .await;

    let server_addr = network
        .nodelay_log()
        .into_iter()
        .find_map(|(role, nodelay)| {
            assert!(nodelay);
            role.strip_prefix("bind:")
                .and_then(|s| s.parse::<SocketAddr>().ok())
        })
        .expect("server must have bound an ephemeral address");

    let mut c1 = network.connect_raw(server_addr).await.unwrap();
    let mut c2 = network.connect_raw(server_addr).await.unwrap();
    let mut c3 = network.connect_raw(server_addr).await.unwrap();

    c1.writer.write_all(b"MSG_FROM_C1").await.unwrap();
    c2.writer.write_all(b"MSG_FROM_C2").await.unwrap();
    c3.writer.write_all(b"MSG_FROM_C3").await.unwrap();
    c1.writer.flush().await.unwrap();
    c2.writer.flush().await.unwrap();
    c3.writer.flush().await.unwrap();

    let (pkts, rx) = tokio::task::spawn_blocking(move || {
        let mut out = Vec::new();
        for _ in 0..3 {
            out.push(rx.recv_timeout(Duration::from_secs(2)).unwrap());
        }
        (out, rx)
    })
    .await
    .unwrap();

    let mut tags = std::collections::HashSet::new();
    let mut payloads = std::collections::HashSet::new();
    for pkt in &pkts {
        let tag = pkt
            .source_tag
            .as_ref()
            .expect("TcpServer packets must carry client peer source_tag")
            .to_string();
        tags.insert(tag);
        payloads.insert(pkt.bytes.to_vec());
    }
    assert_eq!(tags.len(), 3, "each client must have a distinct peer source_tag");
    assert!(payloads.contains(b"MSG_FROM_C1".as_slice()));
    assert!(payloads.contains(b"MSG_FROM_C2".as_slice()));
    assert!(payloads.contains(b"MSG_FROM_C3".as_slice()));

    // Broadcast outbound frame to all 3 connected clients.
    scheduler
        .submit_tx(OutboundFrame::fire_and_forget(
            Bytes::from_static(b"SERVER_BCAST_1"),
            TxPriority::High,
        ))
        .unwrap();

    for (idx, client) in [&mut c1, &mut c2, &mut c3].into_iter().enumerate() {
        let mut buf = [0u8; 64];
        let n = tokio::time::timeout(Duration::from_secs(2), client.reader.read(&mut buf))
            .await
            .unwrap_or_else(|_| panic!("client {idx} TX timeout"))
            .unwrap();
        assert_eq!(&buf[..n], b"SERVER_BCAST_1");
    }

    // Disconnect client 2 cleanly and verify remaining clients still operate.
    drop(c2);
    tokio::time::sleep(Duration::from_millis(30)).await;

    c1.writer.write_all(b"C1_AFTER_C2_DROP").await.unwrap();
    c1.writer.flush().await.unwrap();
    let pkt_after =
        tokio::task::spawn_blocking(move || rx.recv_timeout(Duration::from_secs(2)).unwrap())
            .await
            .unwrap();
    assert_eq!(&pkt_after.bytes[..], b"C1_AFTER_C2_DROP");

    scheduler
        .submit_tx(OutboundFrame::fire_and_forget(
            Bytes::from_static(b"SERVER_BCAST_2"),
            TxPriority::Normal,
        ))
        .unwrap();
    for client in [&mut c1, &mut c3] {
        let mut buf = [0u8; 64];
        let n = tokio::time::timeout(Duration::from_secs(2), client.reader.read(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&buf[..n], b"SERVER_BCAST_2");
    }

    scheduler.stop().await.unwrap();
    assert_eq!(*state_watch.borrow(), TransportState::Stopped);
}

#[test]
fn t9_transport_registry_factories_and_dto_json_compatibility() {
    let mut registry = TransportRegistry::new();
    registry.register(TransportKind::Serial, create_serial_transport);
    registry.register(TransportKind::Udp, create_udp_transport);
    registry.register(TransportKind::TcpClient, create_tcp_client_transport);
    registry.register(TransportKind::TcpServer, create_tcp_server_transport);

    let serial_json = r#"{
        "kind": "serial",
        "port_name": "COM4",
        "baud_rate": 2000000,
        "data_bits": 8,
        "parity": "none",
        "stop_bits": "one",
        "flow_control": "hardware_rts_cts",
        "auto_reconnect": true
    }"#;
    let serial_t = registry.create(TransportKind::Serial, serial_json).unwrap();
    assert_eq!(serial_t.capabilities().kind, TransportKind::Serial);
    assert!(serial_t.capabilities().supports_auto_reconnect);

    let udp_json = r#"{
        "kind": "udp",
        "bind_addr": "0.0.0.0:9001",
        "multicast_group": "239.1.2.3",
        "recv_buffer_bytes": 4194304,
        "remote_tx_addr": "127.0.0.1:9002"
    }"#;
    let udp_t = registry.create(TransportKind::Udp, udp_json).unwrap();
    assert_eq!(udp_t.capabilities().kind, TransportKind::Udp);
    assert!(udp_t.capabilities().supports_tx);

    let tcp_client_json = r#"{
        "kind": "tcp_client",
        "remote_addr": "127.0.0.1:7000",
        "auto_reconnect": false,
        "tcp_nodelay": true
    }"#;
    let tcp_client_t = registry
        .create(TransportKind::TcpClient, tcp_client_json)
        .unwrap();
    assert_eq!(tcp_client_t.capabilities().kind, TransportKind::TcpClient);
    assert!(!tcp_client_t.capabilities().supports_auto_reconnect);

    let tcp_server_json = r#"{
        "kind": "tcp_server",
        "listen_addr": "127.0.0.1:7001",
        "tcp_nodelay": true
    }"#;
    let tcp_server_t = registry
        .create(TransportKind::TcpServer, tcp_server_json)
        .unwrap();
    assert_eq!(tcp_server_t.capabilities().kind, TransportKind::TcpServer);
    assert!(tcp_server_t.capabilities().supports_tx);

    assert!(matches!(
        registry.create(TransportKind::Serial, "{}"),
        Err(CoreError::InvalidConfig(_))
    ));
    assert!(matches!(
        registry.create(TransportKind::Udp, r#"{"bind_addr":"bad"}"#),
        Err(CoreError::InvalidConfig(_))
    ));
}
