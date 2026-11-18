//! End-to-end tests verifying external `plugin.json` descriptor discovery,
//! declarative JSON protocol plugins, C-ABI dynamic library (`.dll`/`.so`) plugins,
//! external widget bundle loading, and path-traversal safety.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ddscope_core::clock::ClockMode;
use ddscope_core::protocol::compute_checksum;
use ddscope_core::protocol::ChecksumKind;
use ddscope_core::transport::TcpClientConfig;
use ddscope_core::types::{ChannelKind, ChannelPath};
use ddscope_core::{CoreWorkspace, SourceConfig, SourceProtocolConfig, SourceTransportConfig};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

fn repo_plugins_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("plugins")
        .canonicalize()
        .expect("repo plugins directory must exist")
}

async fn wait_for_samples(ws: &Arc<CoreWorkspace>, path: &str, min_samples: u64) {
    let ch_path = ChannelPath(path.to_owned());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(entry) = ws.hub().get_by_path(&ch_path) {
            if entry.total_samples() >= min_samples {
                return;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for >= {min_samples} samples on `{path}`"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn external_plugin_catalog_discovers_protocols_and_widgets_with_traversal_guard() {
    let ws = CoreWorkspace::new(tokio::runtime::Handle::current());
    let catalog = ws.reload_external_plugins_from(&[repo_plugins_dir()]);

    let scaled = catalog
        .protocols
        .iter()
        .find(|p| p.manifest.id == "scaled_int16_frame")
        .expect("scaled_int16_frame plugin.json must be discovered");
    assert!(scaled.available, "scaled_int16_frame error: {:?}", scaled.error);
    assert_eq!(scaled.manifest.backend.mode_tag(), "declarative");

    let xor = catalog
        .protocols
        .iter()
        .find(|p| p.manifest.id == "custom_xor_frame")
        .expect("custom_xor_frame plugin.json must be discovered");
    assert!(
        xor.available,
        "custom_xor_frame cdylib error (run `cargo build -p ddscope-plugin-xor-demo` first if missing): {:?}",
        xor.error
    );
    assert_eq!(xor.manifest.backend.mode_tag(), "cdylib");

    let widget = catalog
        .widgets
        .iter()
        .find(|w| w.manifest.id == "bar_level_monitor")
        .expect("bar_level_monitor widget plugin must be discovered");
    assert!(widget.available, "widget error: {:?}", widget.error);

    let bundle = ws
        .load_widget_plugin_bundle("bar_level_monitor")
        .expect("load_widget_plugin_bundle must succeed");
    assert!(bundle.entry_js.contains("export function mount"));
    assert!(bundle
        .style_css
        .as_deref()
        .unwrap_or("")
        .contains(".ddscope-ext-bar-level"));

    // Path traversal / unknown IDs must be rejected cleanly
    assert!(ws.load_widget_plugin_bundle("../evil").is_err());
    assert!(ws.load_widget_plugin_bundle("nonexistent_widget").is_err());

    ws.shutdown().await.expect("workspace shutdown");
}

#[tokio::test]
async fn declarative_scaled_int16_frame_plugin_ingests_calibrated_samples_via_workspace() {
    let ws = Arc::new(CoreWorkspace::new(tokio::runtime::Handle::current()));
    ws.reload_external_plugins_from(&[repo_plugins_dir()]);

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind tcp");
    let addr = listener.local_addr().unwrap();

    let server_task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept tcp");
        // Send junk + 2 valid AA 55 frames + 1 bad-CRC frame + 1 valid frame
        let mut wire = Vec::new();
        wire.extend_from_slice(&[0x00, 0x11, 0xFF]);
        let build_frame = |u: i16, v: i16, bus: i16, corrupt: bool| -> Vec<u8> {
            let mut payload = Vec::with_capacity(6);
            payload.extend_from_slice(&u.to_le_bytes());
            payload.extend_from_slice(&v.to_le_bytes());
            payload.extend_from_slice(&bus.to_le_bytes());
            let mut crc = compute_checksum(ChecksumKind::Crc16Ccitt, &payload) as u16;
            if corrupt {
                crc ^= 0x00FF;
            }
            let mut out = vec![0xAA, 0x55];
            out.extend_from_slice(&payload);
            out.extend_from_slice(&crc.to_le_bytes());
            out
        };
        wire.extend_from_slice(&build_frame(150, -250, 480, false));
        wire.extend_from_slice(&build_frame(999, 999, 999, true));
        wire.extend_from_slice(&build_frame(300, -100, 500, false));

        // Split across arbitrary boundary to verify framing carry
        socket.write_all(&wire[..7]).await.unwrap();
        socket.flush().await.unwrap();
        tokio::time::sleep(Duration::from_millis(15)).await;
        socket.write_all(&wire[7..]).await.unwrap();
        socket.flush().await.unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
    });

    let cfg = SourceConfig::new(
        "decl_src",
        SourceTransportConfig::TcpClient(TcpClientConfig {
            remote_addr: addr.to_string(),
            auto_reconnect: false,
            tcp_nodelay: true,
            reconnect_delay_ms: 100,
        }),
        ClockMode::Tier3HostArrival,
        SourceProtocolConfig::Custom {
            codec_id: "scaled_int16_frame".to_string(),
            json_config: r#"{"current_scale":0.02,"voltage_scale":0.5}"#.to_string(),
        },
    )
    .with_auto_start(true);

    ws.create_source(cfg).expect("create declarative plugin source");
    wait_for_samples(&ws, "decl_src/bus_v", 2).await;

    let entry_u = ws
        .hub()
        .get_by_path(&ChannelPath("decl_src/phase_u".to_owned()))
        .unwrap();
    let entry_v = ws
        .hub()
        .get_by_path(&ChannelPath("decl_src/phase_v".to_owned()))
        .unwrap();
    let entry_bus = ws
        .hub()
        .get_by_path(&ChannelPath("decl_src/bus_v".to_owned()))
        .unwrap();

    // Native i16 wire samples calibrated via `current_scale=0.02` & `voltage_scale=0.5`.
    assert_eq!(entry_u.total_samples(), 2);
    let (_, val_u) = entry_u.latest_value().unwrap();
    let (_, val_v) = entry_v.latest_value().unwrap();
    let (_, val_bus) = entry_bus.latest_value().unwrap();
    assert!((val_u - 6.0).abs() < 1e-6, "got {val_u}");
    assert!((val_v - (-2.0)).abs() < 1e-6, "got {val_v}");
    assert!((val_bus - 250.0).abs() < 1e-6, "got {val_bus}");

    match entry_u.descriptor().kind.clone() {
        ChannelKind::Scalar { calibration, .. } => {
            assert!((calibration.apply(300.0) - 6.0).abs() < 1e-6);
        }
        other => panic!("unexpected channel kind: {other:?}"),
    }
    match entry_bus.descriptor().kind.clone() {
        ddscope_core::types::ChannelKind::Scalar { calibration, .. } => {
            assert!((calibration.apply(500.0) - 250.0).abs() < 1e-6);
        }
        other => panic!("unexpected channel kind: {other:?}"),
    }

    server_task.await.unwrap();
    ws.shutdown().await.expect("workspace shutdown");
}

#[tokio::test]
async fn cdylib_xor_demo_plugin_dynamically_loads_and_decodes_fragmented_frames() {
    let ws = Arc::new(CoreWorkspace::new(tokio::runtime::Handle::current()));
    let catalog = ws.reload_external_plugins_from(&[repo_plugins_dir()]);
    let xor_entry = catalog
        .protocols
        .iter()
        .find(|p| p.manifest.id == "custom_xor_frame")
        .unwrap();
    assert!(xor_entry.available, "cdylib plugin must be built: {:?}", xor_entry.error);

    let xor_key: u8 = 0x3C;
    let encode_frame = move |ch0: f32, ch1: f32, seq: u32, corrupt: bool| -> [u8; 15] {
        let mut plain = [0u8; 12];
        plain[0..4].copy_from_slice(&ch0.to_le_bytes());
        plain[4..8].copy_from_slice(&ch1.to_le_bytes());
        plain[8..12].copy_from_slice(&seq.to_le_bytes());
        let mut csum = 0u8;
        for b in plain {
            csum ^= b;
        }
        if corrupt {
            csum ^= 0xFF;
        }
        let mut out = [0u8; 15];
        out[0] = 0xDD;
        out[1] = 0x7E;
        for (dst, src) in out[2..14].iter_mut().zip(plain) {
            *dst = src ^ xor_key;
        }
        out[14] = csum;
        out
    };

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind tcp");
    let addr = listener.local_addr().unwrap();

    let server_task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept tcp");
        let mut wire = Vec::new();
        wire.extend_from_slice(&[0x01, 0x02, 0xDD]); // junk including partial magic
        wire.extend_from_slice(&encode_frame(12.25, -4.5, 1001, false));
        wire.extend_from_slice(&encode_frame(999.0, 999.0, 999, true)); // corrupted
        wire.extend_from_slice(&encode_frame(24.75, 8.125, 1002, false));

        // Send byte-by-byte / mid-frame splits
        socket.write_all(&wire[..10]).await.unwrap();
        socket.flush().await.unwrap();
        tokio::time::sleep(Duration::from_millis(15)).await;
        socket.write_all(&wire[10..]).await.unwrap();
        socket.flush().await.unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
    });

    let cfg = SourceConfig::new(
        "dll_src",
        SourceTransportConfig::TcpClient(TcpClientConfig {
            remote_addr: addr.to_string(),
            auto_reconnect: false,
            tcp_nodelay: true,
            reconnect_delay_ms: 100,
        }),
        ClockMode::Tier3HostArrival,
        SourceProtocolConfig::Custom {
            codec_id: "custom_xor_frame".to_string(),
            json_config: format!(
                r#"{{"xor_key":{xor_key},"ch0_name":"ia_dll","ch1_name":"ib_dll","seq_name":"seq_dll"}}"#
            ),
        },
    )
    .with_auto_start(true);

    ws.create_source(cfg).expect("create cdylib plugin source");
    wait_for_samples(&ws, "dll_src/seq_dll", 2).await;

    // Reloading external plugins while a DLL session is running must keep its Arc<Library> intact!
    let _ = ws.reload_external_plugins_from(&[repo_plugins_dir()]);

    let ia = ws
        .hub()
        .get_by_path(&ChannelPath("dll_src/ia_dll".to_owned()))
        .unwrap();
    let ib = ws
        .hub()
        .get_by_path(&ChannelPath("dll_src/ib_dll".to_owned()))
        .unwrap();
    let seq = ws
        .hub()
        .get_by_path(&ChannelPath("dll_src/seq_dll".to_owned()))
        .unwrap();
    assert_eq!(ia.total_samples(), 2);
    assert!((ia.latest_value().unwrap().1 - 24.75).abs() < 1e-6);
    assert!((ib.latest_value().unwrap().1 - 8.125).abs() < 1e-6);
    assert!((seq.latest_value().unwrap().1 - 1002.0).abs() < 1e-6);

    server_task.await.unwrap();
    ws.shutdown().await.expect("workspace shutdown");
}
