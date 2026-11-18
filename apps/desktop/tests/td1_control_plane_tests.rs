use std::sync::Arc;
use std::time::Duration;

use ddscope_core::CoreWorkspace;
use ddscope_desktop_lib::commands::convert;
use ddscope_desktop_lib::commands::{
    AnalogToDigitalModeDto, ClockConfigDto, CreateSourceRequestDto, DesktopBridgeState,
    DownlinkPayloadDto, EdgeSlopeDto, JustTypedChannelDto, JustTypedKindDto, PowerOutputFlagsDto,
    ProtocolConfigDto, ScopeLaneKindDto, ScopeTrackBindingDto, ScopeViewportConfigDto,
    SendDownlinkRequestDto, SyncGroupUpdateDto, TransportConfigDto, TriggerConditionDto,
    TriggerSweepModeDto, TxPriorityDto, VirtualNodeConfigDto, VirtualOperatorDto,
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

#[test]
fn downlink_ascii_append_newline_false_preserves_exact_raw_bytes() {
    use ddscope_core::protocol::DownlinkPayload;

    let (_, no_nl_req) = convert::downlink_dto_to_core(SendDownlinkRequestDto {
        source_id: "udp0".to_string(),
        payload: DownlinkPayloadDto::AsciiText {
            text: "123".to_string(),
            append_newline: false,
        },
        priority: TxPriorityDto::Normal,
        coalesce_key: None,
    })
    .expect("append_newline=false conversion should succeed");
    assert_eq!(no_nl_req.payload, DownlinkPayload::RawBytes(b"123".to_vec()));

    let (_, nl_req) = convert::downlink_dto_to_core(SendDownlinkRequestDto {
        source_id: "udp0".to_string(),
        payload: DownlinkPayloadDto::AsciiText {
            text: "123".to_string(),
            append_newline: true,
        },
        priority: TxPriorityDto::Normal,
        coalesce_key: None,
    })
    .expect("append_newline=true conversion should succeed");
    assert_eq!(
        nl_req.payload,
        DownlinkPayload::AsciiLine("123\n".to_string())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_source_and_run_boundaries_roundtrip() {
    use ddscope_core::types::SourceId;
    use std::collections::HashMap;

    let stub = DesktopBridgeState::new_stub();
    assert_eq!(
        stub.get_run_boundaries().expect("stub run boundaries"),
        Vec::<f64>::new()
    );
    let stub_update_err = stub
        .update_source("sim0".to_string(), make_sim_create_request("sim0"))
        .await
        .expect_err("stub update_source must return NOT_IMPLEMENTED");
    assert_eq!(stub_update_err.code, "NOT_IMPLEMENTED");

    let ws = Arc::new(CoreWorkspace::new(tokio::runtime::Handle::current()));
    let state = DesktopBridgeState::with_workspace(Arc::clone(&ws));

    state
        .create_source(make_sim_create_request("sim0"))
        .expect("create sim0");
    let before_tree = state.get_channel_tree().expect("initial channel tree");
    let before_sim = before_tree
        .sources
        .iter()
        .find(|g| g.source.source_id == "sim0")
        .expect("sim0 group present");
    let before_ids: HashMap<String, u32> = before_sim
        .channels
        .iter()
        .map(|c| (c.local_path.clone(), c.channel_id))
        .collect();

    // Start Run #1, wait for samples to arrive, then stop and restart Run #2 -> divider recorded.
    state.start_source("sim0".to_string()).expect("start run 1");
    let sim_core_id = SourceId::new("sim0");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let total: u64 = ws
            .hub()
            .channels_for_source(&sim_core_id)
            .iter()
            .map(|c| c.total_samples())
            .sum();
        if total > 0 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for sim0 samples"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    state
        .stop_source("sim0".to_string())
        .await
        .expect("stop run 1");
    assert!(
        state
            .get_run_boundaries()
            .expect("boundaries before run 2")
            .is_empty(),
        "no divider before Run #2 begins"
    );

    state.start_source("sim0".to_string()).expect("start run 2");
    let boundaries = state.get_run_boundaries().expect("boundaries after run 2");
    assert_eq!(boundaries.len(), 1);
    assert!(boundaries[0] > 0.0);

    // Update sim0 display_name + expand analog_channels 2 -> 3 while preserving channel IDs.
    let updated_sim = CreateSourceRequestDto {
        source_id: "sim0".to_string(),
        display_name: Some("Updated Sim0".to_string()),
        transport: TransportConfigDto::Sim {
            sample_rate_hz: 2_000.0,
            analog_channels: 3,
            enable_glitch: false,
            glitch_period_samples: 0,
        },
        clock: ClockConfigDto::Tier3HostArrival,
        protocol: ProtocolConfigDto::SimPreset,
        auto_start: true,
    };
    let updated_summary = state
        .update_source("sim0".to_string(), updated_sim)
        .await
        .expect("update sim0");
    assert_eq!(updated_summary.display_name, "Updated Sim0");

    let cfgs = state.get_source_configs().expect("source configs after edit");
    assert_eq!(cfgs.len(), 1);
    assert_eq!(cfgs[0].display_name.as_deref(), Some("Updated Sim0"));
    match &cfgs[0].transport {
        TransportConfigDto::Sim {
            analog_channels,
            sample_rate_hz,
            ..
        } => {
            assert_eq!(*analog_channels, 3);
            assert_eq!(*sample_rate_hz, 2_000.0);
        }
        other => panic!("expected Sim transport after update, got {other:?}"),
    }

    let after_tree = state.get_channel_tree().expect("channel tree after edit");
    let after_sim = after_tree
        .sources
        .iter()
        .find(|g| g.source.source_id == "sim0")
        .expect("sim0 still present");
    for ch in &after_sim.channels {
        if let Some(&orig_id) = before_ids.get(&ch.local_path) {
            assert_eq!(ch.channel_id, orig_id);
        }
    }

    // Create UDP rx-only source, then update with remote_tx_addr and verify downlink succeeds.
    state
        .create_source(CreateSourceRequestDto {
            source_id: "udp_edit".to_string(),
            display_name: Some("UDP Editable".to_string()),
            transport: TransportConfigDto::Udp {
                bind_addr: "127.0.0.1:0".to_string(),
                multicast_group: None,
                recv_buffer_bytes: 65_536,
                remote_tx_addr: None,
            },
            clock: ClockConfigDto::Tier3HostArrival,
            protocol: ProtocolConfigDto::FireWater {
                channel_names: vec!["ch0".to_string()],
            },
            auto_start: true,
        })
        .expect("create udp_edit");

    state
        .update_source(
            "udp_edit".to_string(),
            CreateSourceRequestDto {
                source_id: "udp_edit".to_string(),
                display_name: Some("UDP With TX".to_string()),
                transport: TransportConfigDto::Udp {
                    bind_addr: "127.0.0.1:0".to_string(),
                    multicast_group: None,
                    recv_buffer_bytes: 65_536,
                    remote_tx_addr: Some("127.0.0.1:19002".to_string()),
                },
                clock: ClockConfigDto::Tier3HostArrival,
                protocol: ProtocolConfigDto::FireWater {
                    channel_names: vec!["ch0".to_string()],
                },
                auto_start: true,
            },
        )
        .await
        .expect("update udp_edit with remote_tx_addr");

    let tx_receipt = state
        .send_downlink(SendDownlinkRequestDto {
            source_id: "udp_edit".to_string(),
            payload: DownlinkPayloadDto::AsciiText {
                text: "CMD".to_string(),
                append_newline: true,
            },
            priority: TxPriorityDto::High,
            coalesce_key: None,
        })
        .expect("UDP downlink after setting remote_tx_addr should succeed");
    assert!(tx_receipt.accepted);

    state.stop_source("sim0".to_string()).await.ok();
    state.stop_source("udp_edit".to_string()).await.ok();
    ws.shutdown().await.expect("shutdown workspace");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn channel_counter_clock_mode_control_plane_roundtrip() {
    let ws = Arc::new(CoreWorkspace::new(tokio::runtime::Handle::current()));
    let state = DesktopBridgeState::with_workspace(Arc::clone(&ws));

    // Invalid empty or nonexistent channel must return INVALID_CONFIG
    let bad_err = state
        .create_source(CreateSourceRequestDto {
            source_id: "cc_bad".to_string(),
            display_name: None,
            transport: TransportConfigDto::Udp {
                bind_addr: "127.0.0.1:0".to_string(),
                multicast_group: None,
                recv_buffer_bytes: 65_536,
                remote_tx_addr: None,
            },
            clock: ClockConfigDto::ChannelCounter {
                channel: "missing_cnt".to_string(),
                ns_per_count: 1_000.0,
                rollover_modulus: Some(65_536.0),
                drift_compensation: true,
            },
            protocol: ProtocolConfigDto::FireWater {
                channel_names: vec!["va".to_string(), "tick_cnt".to_string()],
            },
            auto_start: false,
        })
        .expect_err("nonexistent sync channel must fail");
    assert_eq!(bad_err.code, "INVALID_CONFIG");

    // Valid ChannelCounter create + update + get_source_configs export roundtrip
    let created = state
        .create_source(CreateSourceRequestDto {
            source_id: "cc_udp".to_string(),
            display_name: Some("Channel Sync UDP".to_string()),
            transport: TransportConfigDto::Udp {
                bind_addr: "127.0.0.1:0".to_string(),
                multicast_group: None,
                recv_buffer_bytes: 65_536,
                remote_tx_addr: None,
            },
            clock: ClockConfigDto::ChannelCounter {
                channel: "tick_cnt".to_string(),
                ns_per_count: 1_000.0,
                rollover_modulus: None,
                drift_compensation: true,
            },
            protocol: ProtocolConfigDto::FireWater {
                channel_names: vec!["va".to_string(), "tick_cnt".to_string()],
            },
            auto_start: false,
        })
        .expect("create cc_udp with ChannelCounter");
    assert_eq!(created.source_id, "cc_udp");

    state
        .update_source(
            "cc_udp".to_string(),
            CreateSourceRequestDto {
                source_id: "cc_udp".to_string(),
                display_name: Some("Channel Sync Edited".to_string()),
                transport: TransportConfigDto::Udp {
                    bind_addr: "127.0.0.1:0".to_string(),
                    multicast_group: None,
                    recv_buffer_bytes: 65_536,
                    remote_tx_addr: Some("127.0.0.1:19010".to_string()),
                },
                clock: ClockConfigDto::ChannelCounter {
                    channel: "tick_cnt".to_string(),
                    ns_per_count: 250.0,
                    rollover_modulus: Some(10_000.0),
                    drift_compensation: false,
                },
                protocol: ProtocolConfigDto::FireWater {
                    channel_names: vec!["va".to_string(), "tick_cnt".to_string()],
                },
                auto_start: false,
            },
        )
        .await
        .expect("update cc_udp ChannelCounter parameters");

    let cfgs = state.get_source_configs().expect("get_source_configs");
    assert_eq!(cfgs.len(), 1);
    assert_eq!(
        cfgs[0].clock,
        ClockConfigDto::ChannelCounter {
            channel: "tick_cnt".to_string(),
            ns_per_count: 250.0,
            rollover_modulus: Some(10_000.0),
            drift_compensation: false,
        }
    );

    ws.shutdown().await.expect("shutdown workspace");
}

#[tokio::test]
async fn virtual_channels_and_single_phase_power_control_plane_roundtrip() {
    let ws = Arc::new(CoreWorkspace::new(tokio::runtime::Handle::current()));
    let state = DesktopBridgeState::with_workspace(Arc::clone(&ws));

    state
        .create_source(make_sim_create_request("sim0"))
        .expect("create sim0");
    state.start_source("sim0".to_string()).expect("start sim0");

    let a2d_summary = state
        .upsert_virtual_channel(VirtualNodeConfigDto {
            node_id: "v_cmp".to_string(),
            name: "Zero Cross".to_string(),
            enabled: true,
            operator: VirtualOperatorDto::AnalogToDigital {
                input_channel: "sim0/ch0".to_string(),
                output_path: "zc_cmp".to_string(),
                mode: AnalogToDigitalModeDto::Schmitt {
                    threshold: 0.0,
                    hysteresis: 0.2,
                    invert: false,
                },
            },
        })
        .expect("upsert a2d virtual channel");
    assert_eq!(a2d_summary.output_channels.len(), 1);

    let pwr_summary = state
        .upsert_virtual_channel(VirtualNodeConfigDto {
            node_id: "v_pwr".to_string(),
            name: "Single Phase Power".to_string(),
            enabled: true,
            operator: VirtualOperatorDto::SinglePhasePower {
                voltage_channel: "sim0/ch0".to_string(),
                current_channel: "sim0/ch1".to_string(),
                output_prefix: "grid".to_string(),
                voltage_scale: 1.0,
                voltage_offset: 0.0,
                current_scale: 1.0,
                current_offset: 0.0,
                nominal_freq_hz: 50.0,
                zero_cross_hysteresis_v: 0.1,
                min_freq_hz: 5.0,
                fallback_window_ms: 100.0,
                outputs: PowerOutputFlagsDto {
                    freq: true,
                    u_rms: true,
                    i_rms: true,
                    p: true,
                    q1: true,
                    q_tot: true,
                    s: true,
                    pf: true,
                    phi_deg: true,
                    u_pk: true,
                    i_pk: true,
                    energy_wh: true,
                },
            },
        })
        .expect("upsert single_phase_power virtual node");
    assert_eq!(pwr_summary.output_channels.len(), 12);

    let list = state.list_virtual_channels().expect("list virtual channels");
    assert_eq!(list.len(), 2);

    // Channel tree exposes the derived "virtual" group, while get_source_configs excludes it.
    let tree = state.get_channel_tree().expect("get_channel_tree");
    let virt_grp = tree
        .sources
        .iter()
        .find(|g| g.source.source_id == "virtual")
        .expect("virtual source group must appear in channel tree");
    assert_eq!(virt_grp.channels.len(), 13);
    assert!(virt_grp.channels.iter().any(|c| c.local_path == "grid.u_rms"));
    assert!(virt_grp.channels.iter().any(|c| c.local_path == "grid.p"));

    let exported_sources = state.get_source_configs().expect("get_source_configs");
    assert_eq!(exported_sources.len(), 1);
    assert_eq!(exported_sources[0].source_id, "sim0");

    // Wait briefly for live sim0 acquisition to drive samples into virtual/zc_cmp
    let virt_id = ddscope_core::types::SourceId::new("virtual");
    let zc_entry = ws.find_channel(&virt_id, "zc_cmp").expect("zc_cmp entry");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while zc_entry.total_samples() == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for live samples on virtual/zc_cmp"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert!(state
        .remove_virtual_channel("v_cmp".to_string())
        .expect("remove v_cmp"));
    assert_eq!(state.list_virtual_channels().unwrap().len(), 1);
    assert_eq!(state.clear_virtual_channels().expect("clear remaining"), 1);
    assert!(state.list_virtual_channels().unwrap().is_empty());

    ws.shutdown().await.expect("shutdown workspace");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn just_typed_control_plane_declares_per_channel_types_and_roundtrips_config() {
    let ws = Arc::new(CoreWorkspace::new(tokio::runtime::Handle::current()));
    let state = DesktopBridgeState::with_workspace(Arc::clone(&ws));

    let req = CreateSourceRequestDto {
        source_id: "jt_mcu".to_string(),
        display_name: Some("MCU JustTyped".to_string()),
        transport: TransportConfigDto::Udp {
            bind_addr: "127.0.0.1:0".to_string(),
            multicast_group: None,
            recv_buffer_bytes: 1_048_576,
            remote_tx_addr: None,
        },
        clock: ClockConfigDto::Tier2SoftwarePll {
            nominal_sample_rate_hz: Some(20_000.0),
        },
        protocol: ProtocolConfigDto::JustTyped {
            channels: vec![
                JustTypedChannelDto {
                    name: "va".to_string(),
                    data_type: JustTypedKindDto::Float,
                },
                JustTypedChannelDto {
                    name: "err_i32".to_string(),
                    data_type: JustTypedKindDto::Int,
                },
                JustTypedChannelDto {
                    name: "flags_u32".to_string(),
                    data_type: JustTypedKindDto::Uint,
                },
            ],
        },
        auto_start: false,
    };

    state.create_source(req.clone()).expect("create just_typed source");

    let tree = state.get_channel_tree().expect("get_channel_tree");
    let grp = tree
        .sources
        .iter()
        .find(|g| g.source.source_id == "jt_mcu")
        .expect("jt_mcu source group must exist");
    assert_eq!(grp.channels.len(), 3);
    let scalar_of = |name: &str| {
        grp.channels
            .iter()
            .find(|c| c.local_path == name)
            .and_then(|c| c.scalar_type.as_deref())
    };
    assert_eq!(scalar_of("va"), Some("f32"));
    assert_eq!(scalar_of("err_i32"), Some("i32"));
    assert_eq!(scalar_of("flags_u32"), Some("u32"));

    let cfgs = state.get_source_configs().expect("get_source_configs");
    assert_eq!(cfgs.len(), 1);
    assert_eq!(cfgs[0].protocol, req.protocol);

    // Duplicate/empty channel names must be rejected clearly
    let mut bad_dup = req.clone();
    bad_dup.source_id = "jt_dup".to_string();
    bad_dup.protocol = ProtocolConfigDto::JustTyped {
        channels: vec![
            JustTypedChannelDto {
                name: "dup".to_string(),
                data_type: JustTypedKindDto::Float,
            },
            JustTypedChannelDto {
                name: "dup".to_string(),
                data_type: JustTypedKindDto::Int,
            },
        ],
    };
    assert!(state.create_source(bad_dup).is_err());

    ws.shutdown().await.expect("shutdown workspace");
}

#[tokio::test]
async fn external_plugins_control_plane_lists_catalog_loads_bundle_and_roundtrips_source_config() {
    let ws = Arc::new(CoreWorkspace::new(tokio::runtime::Handle::current()));
    let plugins_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("plugins");
    ws.reload_external_plugins_from(&[plugins_dir]);
    let state = DesktopBridgeState::with_workspace(Arc::clone(&ws));

    let catalog = state.list_external_plugins().expect("list_external_plugins");
    assert!(catalog
        .protocols
        .iter()
        .any(|p| p.id == "scaled_int16_frame" && p.available));
    assert!(catalog
        .widgets
        .iter()
        .any(|w| w.id == "bar_level_monitor" && w.available));

    let bundle = state
        .load_widget_plugin_bundle("bar_level_monitor".to_string())
        .expect("load widget plugin bundle");
    assert!(bundle.entry_js.contains("mount("));
    assert!(bundle.style_css.is_some());

    // Traversal must be rejected at IPC control plane too
    assert!(state
        .load_widget_plugin_bundle("../evil".to_string())
        .is_err());

    let req = CreateSourceRequestDto {
        source_id: "ext_proto_src".to_string(),
        display_name: Some("Scaled Int16 Plugin".to_string()),
        transport: TransportConfigDto::Udp {
            bind_addr: "127.0.0.1:0".to_string(),
            multicast_group: None,
            recv_buffer_bytes: 1_048_576,
            remote_tx_addr: None,
        },
        clock: ClockConfigDto::Tier2SoftwarePll {
            nominal_sample_rate_hz: Some(20_000.0),
        },
        protocol: ProtocolConfigDto::Plugin {
            plugin_id: "scaled_int16_frame".to_string(),
            config_json: r#"{"current_scale":0.05,"voltage_scale":0.25}"#.to_string(),
        },
        auto_start: false,
    };

    state.create_source(req.clone()).expect("create plugin source");
    let tree = state.get_channel_tree().expect("get_channel_tree");
    let grp = tree
        .sources
        .iter()
        .find(|g| g.source.source_id == "ext_proto_src")
        .expect("ext_proto_src must exist");
    let paths: Vec<&str> = grp.channels.iter().map(|c| c.local_path.as_str()).collect();
    assert!(paths.contains(&"phase_u"));
    assert!(paths.contains(&"phase_v"));
    assert!(paths.contains(&"bus_v"));

    let cfgs = state.get_source_configs().expect("get_source_configs");
    assert_eq!(cfgs.len(), 1);
    assert_eq!(cfgs[0].protocol, req.protocol);

    ws.shutdown().await.expect("shutdown workspace");
}
