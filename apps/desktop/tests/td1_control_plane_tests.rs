use std::sync::Arc;
use std::time::Duration;

use ddscope_core::CoreWorkspace;
use ddscope_desktop_lib::commands::convert;
use ddscope_desktop_lib::commands::{
    ClockConfigDto, CreateSourceRequestDto, DesktopBridgeState, DownlinkPayloadDto, EdgeSlopeDto,
    ProtocolConfigDto, ScopeLaneKindDto, ScopeTrackBindingDto, ScopeViewportConfigDto,
    SendDownlinkRequestDto, SyncGroupUpdateDto, TransportConfigDto, TriggerConditionDto,
    TriggerSweepModeDto, TxPriorityDto,
};

fn make_sim_create_request(source_id: &str) -> CreateSourceRequestDto {
    CreateSourceRequestDto {
        source_id: source_id.to_string(),
        display_name: Some("Simulated Source".to_string()),
        transport: TransportConfigDto::Sim {
            sample_rate_hz: 1_000.0,
            analog_channels: 2,
            enable_glitch: false,
            glitch_period_samples: 0,
        },
        clock: ClockConfigDto::Tier3HostArrival,
        protocol: ProtocolConfigDto::SimPreset,
        auto_start: false,
    }
}

#[tokio::test]
async fn stub_backend_preserves_pre_m4_contract() {
    let state = DesktopBridgeState::new_stub();
    assert!(!state.is_live());

    let engine = state.engine_info();
    assert!(!engine.bridge_ready);
    assert_eq!(engine.ddsf_version, 1);

    let tree = state
        .get_channel_tree()
        .expect("stub get_channel_tree returns empty tree cleanly");
    assert!(tree.sources.is_empty());

    let cfgs = state
        .get_source_configs()
        .expect("stub get_source_configs returns empty list cleanly");
    assert!(cfgs.is_empty());

    let create_err = state
        .create_source(make_sim_create_request("sim0"))
        .expect_err("stub create_source must return NOT_IMPLEMENTED");
    assert_eq!(create_err.code, "NOT_IMPLEMENTED");

    let start_err = state
        .start_source("sim0".to_string())
        .expect_err("stub start_source must return NOT_IMPLEMENTED");
    assert_eq!(start_err.code, "NOT_IMPLEMENTED");

    let stop_err = state
        .stop_source("sim0".to_string())
        .await
        .expect_err("stub stop_source must return NOT_IMPLEMENTED");
    assert_eq!(stop_err.code, "NOT_IMPLEMENTED");

    let remove_err = state
        .remove_source("sim0".to_string())
        .await
        .expect_err("stub remove_source must return NOT_IMPLEMENTED");
    assert_eq!(remove_err.code, "NOT_IMPLEMENTED");

    let scope_err = state
        .upsert_scope_viewport(ScopeViewportConfigDto {
            scope_id: 1,
            title: Some("Scope 1".to_string()),
            canvas_width_px: 800,
            time_span_ns: 20_000_000.0,
            pre_trigger_ratio: 0.2,
            pan_offset_ns: 0.0,
            sweep_mode: TriggerSweepModeDto::Auto,
            auto_timeout_ms: 50,
            trigger: None,
            tracks: vec![],
            sync_group_id: None,
            paused: false,
        })
        .expect_err("stub upsert_scope_viewport must return NOT_IMPLEMENTED");
    assert_eq!(scope_err.code, "NOT_IMPLEMENTED");

    let downlink_err = state
        .send_downlink(SendDownlinkRequestDto {
            source_id: "sim0".to_string(),
            payload: DownlinkPayloadDto::AsciiText {
                text: "PING".to_string(),
                append_newline: true,
            },
            priority: TxPriorityDto::High,
            coalesce_key: None,
        })
        .expect_err("stub send_downlink must return NOT_IMPLEMENTED");
    assert_eq!(downlink_err.code, "NOT_IMPLEMENTED");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_workspace_control_plane_roundtrip() {
    let ws = Arc::new(CoreWorkspace::new(tokio::runtime::Handle::current()));
    let state = DesktopBridgeState::with_workspace(Arc::clone(&ws));
    assert!(state.is_live());

    let engine = state.engine_info();
    assert!(engine.bridge_ready);
    assert_eq!(engine.ddsf_version, 1);

    let ports = state
        .list_serial_ports()
        .expect("serial port enumeration should succeed in live backend");
    let _ = ports;

    // 1. Create & start a simulated source
    let sim_summary = state
        .create_source(make_sim_create_request("sim0"))
        .expect("create sim0");
    assert_eq!(sim_summary.source_id, "sim0");
    assert_eq!(sim_summary.transport_kind, "sim");

    let started = state.start_source("sim0".to_string()).expect("start sim0");
    assert_eq!(started.source_id, "sim0");

    tokio::time::sleep(Duration::from_millis(40)).await;

    // 2. Inspect channel tree snapshot
    let tree = state.get_channel_tree().expect("get_channel_tree");
    let sim_group = tree
        .sources
        .iter()
        .find(|g| g.source.source_id == "sim0")
        .expect("sim0 source group present");
    assert!(
        !sim_group.channels.is_empty(),
        "sim0 should register declared channels"
    );
    let ch_id = sim_group.channels[0].channel_id;

    // 3. Configure scope viewport, re-arm trigger, and propagate sync group update
    let scope_summary = state
        .upsert_scope_viewport(ScopeViewportConfigDto {
            scope_id: 1,
            title: Some("Primary Analog".to_string()),
            canvas_width_px: 1024,
            time_span_ns: 20_000_000.0,
            pre_trigger_ratio: 0.25,
            pan_offset_ns: 0.0,
            sweep_mode: TriggerSweepModeDto::Auto,
            auto_timeout_ms: 50,
            trigger: Some(TriggerConditionDto::Edge {
                channel_id: ch_id,
                level: 0.0,
                hysteresis: 0.05,
                slope: EdgeSlopeDto::Rising,
            }),
            tracks: vec![ScopeTrackBindingDto {
                channel_id: ch_id,
                lane: ScopeLaneKindDto::Analog,
                visible: true,
                y_scale: Some(1.0),
                y_offset: Some(0.0),
                color_rgba: Some(0x38_BD_F8_FF),
            }],
            sync_group_id: Some(7),
            paused: false,
        })
        .expect("upsert_scope_viewport");
    assert_eq!(scope_summary.scope_id, 1);
    assert_eq!(scope_summary.active_track_count, 1);
    assert_eq!(scope_summary.sync_group_id, Some(7));

    state.rearm_scope(1).expect("rearm_scope");
    state
        .update_sync_group(SyncGroupUpdateDto {
            group_id: 7,
            origin_scope_id: 1,
            time_span_ns: 10_000_000.0,
            center_offset_ns: 0.0,
            cursor_a_ratio: Some(0.25),
            cursor_b_ratio: Some(0.75),
        })
        .expect("update_sync_group");

    // 4. Downlink dispatch on sim0 + error propagation + coalesced KeyValue TX on UDP
    let sim_receipt = state
        .send_downlink(SendDownlinkRequestDto {
            source_id: "sim0".to_string(),
            payload: DownlinkPayloadDto::AsciiText {
                text: "PING".to_string(),
                append_newline: true,
            },
            priority: TxPriorityDto::High,
            coalesce_key: None,
        })
        .expect("sim0 downlink dispatch succeeds");
    assert!(sim_receipt.accepted);
    assert_eq!(sim_receipt.result_kind, "sent_fire_and_forget");

    let nan_err = state
        .send_downlink(SendDownlinkRequestDto {
            source_id: "sim0".to_string(),
            payload: DownlinkPayloadDto::KeyValue {
                key: "kp".to_string(),
                value: f64::NAN,
            },
            priority: TxPriorityDto::Normal,
            coalesce_key: None,
        })
        .expect_err("non-finite KeyValue downlink must fail validation");
    assert_eq!(nan_err.code, "INVALID_CONFIG");

    let missing_err = state
        .send_downlink(SendDownlinkRequestDto {
            source_id: "nonexistent_source".to_string(),
            payload: DownlinkPayloadDto::AsciiText {
                text: "PING".to_string(),
                append_newline: false,
            },
            priority: TxPriorityDto::High,
            coalesce_key: None,
        })
        .expect_err("unknown source downlink must fail");
    assert!(!missing_err.code.is_empty());

    state
        .create_source(CreateSourceRequestDto {
            source_id: "udp0".to_string(),
            display_name: Some("UDP Loopback TX".to_string()),
            transport: TransportConfigDto::Udp {
                bind_addr: "127.0.0.1:0".to_string(),
                multicast_group: None,
                recv_buffer_bytes: 65_536,
                remote_tx_addr: Some("127.0.0.1:19999".to_string()),
            },
            clock: ClockConfigDto::Tier3HostArrival,
            protocol: ProtocolConfigDto::FireWater {
                channel_names: vec!["ch0".to_string()],
            },
            auto_start: true,
        })
        .expect("create udp0 bidirectional source");

    let receipt = state
        .send_downlink(SendDownlinkRequestDto {
            source_id: "udp0".to_string(),
            payload: DownlinkPayloadDto::KeyValue {
                key: "kp".to_string(),
                value: 1.5,
            },
            priority: TxPriorityDto::High,
            coalesce_key: Some("gain:kp".to_string()),
        })
        .expect("udp0 downlink should succeed");
    assert!(receipt.accepted);
    assert_eq!(receipt.result_kind, "sent_fire_and_forget");

    // 4b. Query retained source configurations for config export
    let exported_cfgs = state.get_source_configs().expect("get_source_configs");
    assert_eq!(exported_cfgs.len(), 2);
    assert_eq!(exported_cfgs[0].source_id, "sim0");
    assert_eq!(exported_cfgs[1].source_id, "udp0");
    assert_eq!(
        exported_cfgs[1].display_name.as_deref(),
        Some("UDP Loopback TX")
    );

    // 5. Telemetry event conversion
    let snap = ws.telemetry_snapshot();
    let event = convert::telemetry_snapshot_to_event(&snap, 1234.5);
    assert_eq!(event.timestamp_ms, 1234.5);
    assert_eq!(event.sources.len(), 2);

    // 6. Clean teardown
    state
        .remove_scope_viewport(1)
        .expect("remove_scope_viewport");
    state
        .stop_source("sim0".to_string())
        .await
        .expect("stop sim0");
    state
        .remove_source("sim0".to_string())
        .await
        .expect("remove sim0");
    state
        .stop_source("udp0".to_string())
        .await
        .expect("stop udp0");
    state
        .remove_source("udp0".to_string())
        .await
        .expect("remove udp0");
    ws.shutdown().await.expect("shutdown workspace");
}

#[test]
fn portable_default_project_config_saves_next_to_exe_and_round_trips() {
    use ddscope_desktop_lib::commands::state::{
        clear_default_project_config_at_path, load_default_project_config_from_path,
        portable_default_project_config_path, save_default_project_config_to_path,
        DEFAULT_PROJECT_CONFIG_FILENAME,
    };

    let exe_cfg_path = portable_default_project_config_path().expect("resolve exe dir path");
    let exe_dir = std::env::current_exe()
        .expect("current_exe")
        .parent()
        .expect("exe parent")
        .to_path_buf();
    assert_eq!(exe_cfg_path, exe_dir.join(DEFAULT_PROJECT_CONFIG_FILENAME));

    let temp_dir = std::env::temp_dir().join(format!(
        "ddscope_portable_cfg_test_{}",
        std::process::id()
    ));
    let test_file = temp_dir.join(DEFAULT_PROJECT_CONFIG_FILENAME);
    let _ = clear_default_project_config_at_path(&test_file);

    assert_eq!(
        load_default_project_config_from_path(&test_file).expect("load missing"),
        None
    );

    let err = save_default_project_config_to_path(&test_file, "{not-valid-json")
        .expect_err("invalid JSON must be rejected");
    assert_eq!(err.code, "INVALID_CONFIG");

    let payload = r#"{"schema":"ddscope.project.v1","sources":[],"board":{}}"#;
    let saved_path =
        save_default_project_config_to_path(&test_file, payload).expect("save valid JSON");
    assert_eq!(saved_path, test_file.to_string_lossy());
    assert_eq!(
        load_default_project_config_from_path(&test_file).expect("load saved"),
        Some(payload.to_string())
    );

    assert!(clear_default_project_config_at_path(&test_file).expect("clear saved"));
    assert!(!clear_default_project_config_at_path(&test_file).expect("clear missing"));
    let _ = std::fs::remove_dir_all(&temp_dir);
}
