//! Validated bidirectional conversions between `tauri-specta` DTOs and `ddscope-core` types.

use std::path::PathBuf;

use ddscope_core::clock::{ClockMode, ClockSyncConfig};
use ddscope_core::protocol::{
    ChecksumKind, ChecksumScope, DownlinkPayload, FireWaterConfig, FixedFieldSpec,
    FixedSchemaBinaryConfig, JustFloatConfig, SimPresetConfig, TransactionResult,
};
use ddscope_core::scope::{
    EdgeSlope, ScopeControllerConfig, TriggerCondition, TriggerSweepMode,
};
use ddscope_core::transport::{
    RawFileReplayConfig, SerialConfig, SerialFlowControl, SerialParity, SerialPortInfo,
    SerialStopBits, SimConfig, SimWireFormat, TcpClientConfig, TcpServerConfig, TransportKind,
    TransportRuntimeConfig, TxPriority, UdpConfig,
};
use ddscope_core::types::{
    ChannelId, ChannelKind, Endianness, EventSchemaKind, LinearScale, ScalarType, ScalarValue,
    ScopeId, SourceId, SyncGroupId,
};
use ddscope_core::{
    AnalogToDigitalMode, CalculusMode, CoreWorkspace, PowerOutputFlags, RegisteredChannelInfo,
    ScopeConfig, ScopeLaneKind, ScopeSummary, ScopeTrackBinding, SourceConfig, SourceProtocolConfig,
    SourceRuntimeState, SourceSummary, SourceTelemetrySnapshot, SourceTransportConfig, StateLabelSpec,
    StateRangeRule, StateValueEntry, SyncGroupUpdate, ToAnalogMode, ToStateMachineMode,
    VirtualNodeConfig, VirtualNodeSummary, VirtualOperatorConfig, WindowMetricKind,
    WorkspaceTelemetrySnapshot,
};

use super::dto::{
    AnalogToDigitalModeDto, ApiErrorDto, CalculusModeDto, ChannelModalityDto, ChannelNodeDto,
    ChannelTreeSnapshotDto, ClockConfigDto, CreateSourceRequestDto, DownlinkPayloadDto,
    DownlinkReceiptDto, EdgeSlopeDto, FixedFieldSpecDto, PowerOutputFlagsDto, ProtocolConfigDto,
    ScopeLaneKindDto, ScopeViewportConfigDto, ScopeViewportSummaryDto, SendDownlinkRequestDto,
    SerialFlowControlDto, SerialParityDto, SerialPortInfoDto, SerialStopBitsDto,
    SourceChannelGroupDto, SourceRuntimeStateDto, SourceSummaryDto, SourceTelemetryDto,
    StateLabelEntryDto, StateLabelSpecDto, StateRangeRuleDto, StateValueEntryDto,
    SyncGroupUpdateDto, ToAnalogModeDto, ToStateMachineModeDto, TransportConfigDto,
    TriggerConditionDto, TriggerSweepModeDto, TxPriorityDto, VirtualNodeConfigDto,
    VirtualNodeSummaryDto, VirtualOperatorDto, WindowMetricKindDto, WorkspaceTelemetryEvent,
};
use ddscope_core::workspace::DownlinkRequest;

/// Converts OS serial port metadata into the frontend DTO.
pub fn serial_port_to_dto(info: SerialPortInfo) -> SerialPortInfoDto {
    SerialPortInfoDto {
        port_name: info.port_name,
        description: info.description,
        vid: info.vid,
        pid: info.pid,
    }
}

/// Converts a [`TransportKind`] discriminant to its canonical lowercase wire tag.
pub fn transport_kind_to_str(kind: TransportKind) -> &'static str {
    match kind {
        TransportKind::Sim => "sim",
        TransportKind::Serial => "serial",
        TransportKind::Udp => "udp",
        TransportKind::TcpClient => "tcp_client",
        TransportKind::TcpServer => "tcp_server",
        TransportKind::RawFileReplay => "raw_file_replay",
        TransportKind::UsbBulk => "usb_bulk",
        TransportKind::DebugProbeRtt => "debug_probe_rtt",
        TransportKind::CanFd => "can_fd",
    }
}

/// Converts a core [`SourceRuntimeState`] to [`SourceRuntimeStateDto`].
pub fn source_state_to_dto(state: SourceRuntimeState) -> SourceRuntimeStateDto {
    match state {
        SourceRuntimeState::Stopped => SourceRuntimeStateDto::Stopped,
        SourceRuntimeState::Connecting => SourceRuntimeStateDto::Connecting,
        SourceRuntimeState::Running => SourceRuntimeStateDto::Running,
        SourceRuntimeState::Reconnecting => SourceRuntimeStateDto::Reconnecting,
        SourceRuntimeState::Completed => SourceRuntimeStateDto::Completed,
        SourceRuntimeState::Error => SourceRuntimeStateDto::Error,
    }
}

/// Converts a core [`SourceSummary`] into [`SourceSummaryDto`].
pub fn source_summary_to_dto(summary: SourceSummary) -> SourceSummaryDto {
    SourceSummaryDto {
        source_id: summary.source_id.as_str().to_owned(),
        display_name: summary.display_name,
        transport_kind: transport_kind_to_str(summary.transport_kind).to_string(),
        state: source_state_to_dto(summary.state),
        raw_recording_path: summary
            .raw_recording_path
            .map(|p| p.to_string_lossy().into_owned()),
    }
}

/// Parses a scalar type name (`bool`, `u8`..`f64`) into [`ScalarType`].
pub fn parse_scalar_type(raw: &str) -> Result<ScalarType, ApiErrorDto> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "bool" => Ok(ScalarType::Bool),
        "u8" => Ok(ScalarType::U8),
        "u16" => Ok(ScalarType::U16),
        "u32" => Ok(ScalarType::U32),
        "u64" => Ok(ScalarType::U64),
        "i8" => Ok(ScalarType::I8),
        "i16" => Ok(ScalarType::I16),
        "i32" => Ok(ScalarType::I32),
        "i64" => Ok(ScalarType::I64),
        "f32" => Ok(ScalarType::F32),
        "f64" => Ok(ScalarType::F64),
        other => Err(ApiErrorDto::invalid_config(format!(
            "unsupported scalar_type `{other}`"
        ))),
    }
}

/// Returns the canonical lowercase identifier for a [`ScalarType`].
pub fn scalar_type_to_str(st: ScalarType) -> &'static str {
    match st {
        ScalarType::Bool => "bool",
        ScalarType::U8 => "u8",
        ScalarType::U16 => "u16",
        ScalarType::U32 => "u32",
        ScalarType::U64 => "u64",
        ScalarType::I8 => "i8",
        ScalarType::I16 => "i16",
        ScalarType::I32 => "i32",
        ScalarType::I64 => "i64",
        ScalarType::F32 => "f32",
        ScalarType::F64 => "f64",
    }
}

/// Converts [`CreateSourceRequestDto`] into a validated core [`SourceConfig`].
pub fn create_source_dto_to_core(req: CreateSourceRequestDto) -> Result<SourceConfig, ApiErrorDto> {
    let source_id_str = req.source_id.trim();
    if source_id_str.is_empty() {
        return Err(ApiErrorDto::invalid_config(
            "source_id must not be empty",
        ));
    }

    let wire_format = match &req.protocol {
        ProtocolConfigDto::SimPreset => SimWireFormat::SimPreset,
        ProtocolConfigDto::JustFloat { .. } => SimWireFormat::JustFloat,
        ProtocolConfigDto::FireWater { .. } => SimWireFormat::FireWaterCsv,
        ProtocolConfigDto::FixedSchemaBinary { .. } => SimWireFormat::SimPreset,
    };

    let sim_analog_channels = match &req.transport {
        TransportConfigDto::Sim { analog_channels, .. } => *analog_channels,
        _ => 4,
    };

    let transport = match req.transport {
        TransportConfigDto::Sim {
            sample_rate_hz,
            analog_channels,
            enable_glitch,
            glitch_period_samples,
        } => {
            let cfg = SimConfig {
                sample_rate_hz,
                analog_channels,
                enable_glitch,
                glitch_period_samples,
                wire_format,
                ..SimConfig::default()
            };
            cfg.validate().map_err(ApiErrorDto::from)?;
            SourceTransportConfig::Sim(cfg)
        }
        TransportConfigDto::Serial {
            port_name,
            baud_rate,
            data_bits,
            parity,
            stop_bits,
            flow_control,
            auto_reconnect,
        } => {
            let cfg = SerialConfig {
                port_name,
                baud_rate,
                data_bits,
                parity: match parity {
                    SerialParityDto::None => SerialParity::None,
                    SerialParityDto::Odd => SerialParity::Odd,
                    SerialParityDto::Even => SerialParity::Even,
                },
                stop_bits: match stop_bits {
                    SerialStopBitsDto::One => SerialStopBits::One,
                    SerialStopBitsDto::Two => SerialStopBits::Two,
                },
                flow_control: match flow_control {
                    SerialFlowControlDto::None => SerialFlowControl::None,
                    SerialFlowControlDto::HardwareRtsCts => SerialFlowControl::HardwareRtsCts,
                    SerialFlowControlDto::SoftwareXonXoff => SerialFlowControl::SoftwareXonXoff,
                },
                auto_reconnect,
                ..SerialConfig::default()
            };
            cfg.validate().map_err(ApiErrorDto::from)?;
            SourceTransportConfig::Serial(cfg)
        }
        TransportConfigDto::Udp {
            bind_addr,
            multicast_group,
            recv_buffer_bytes,
            remote_tx_addr,
        } => {
            let cfg = UdpConfig {
                bind_addr,
                multicast_group,
                recv_buffer_bytes,
                remote_tx_addr,
                ..UdpConfig::default()
            };
            cfg.validate().map_err(ApiErrorDto::from)?;
            SourceTransportConfig::Udp(cfg)
        }
        TransportConfigDto::TcpClient {
            remote_addr,
            auto_reconnect,
            tcp_nodelay,
        } => {
            let cfg = TcpClientConfig {
                remote_addr,
                auto_reconnect,
                tcp_nodelay,
                ..TcpClientConfig::default()
            };
            cfg.validate().map_err(ApiErrorDto::from)?;
            SourceTransportConfig::TcpClient(cfg)
        }
        TransportConfigDto::TcpServer {
            listen_addr,
            tcp_nodelay,
        } => {
            let cfg = TcpServerConfig {
                listen_addr,
                tcp_nodelay,
            };
            cfg.validate().map_err(ApiErrorDto::from)?;
            SourceTransportConfig::TcpServer(cfg)
        }
        TransportConfigDto::RawFileReplay {
            file_path,
            full_speed_offline,
            playback_speed,
            loop_playback,
            nominal_byte_rate,
        } => {
            let mut cfg = if full_speed_offline {
                RawFileReplayConfig::offline(PathBuf::from(file_path))
            } else {
                RawFileReplayConfig::paced(PathBuf::from(file_path), nominal_byte_rate)
            };
            cfg.playback_speed = playback_speed;
            cfg.loop_playback = loop_playback;
            cfg.validate().map_err(ApiErrorDto::from)?;
            SourceTransportConfig::RawFileReplay(cfg)
        }
    };

    let clock_mode = match req.clock {
        ClockConfigDto::Tier1HardwareCounter {
            tick_hz,
            counter_bits,
        } => {
            if !tick_hz.is_finite() || tick_hz <= 0.0 {
                return Err(ApiErrorDto::invalid_config(
                    "tier1 tick_hz must be finite and > 0",
                ));
            }
            ClockMode::Tier1HardwareCounter {
                tick_hz,
                counter_bits,
            }
        }
        ClockConfigDto::Tier2SoftwarePll {
            nominal_sample_rate_hz,
        } => ClockMode::Tier2SoftwarePll {
            nominal_sample_rate_hz,
        },
        ClockConfigDto::Tier3HostArrival => ClockMode::Tier3HostArrival,
        ClockConfigDto::ChannelCounter {
            channel,
            ns_per_count,
            rollover_modulus,
            drift_compensation,
        } => {
            let trimmed_ch = channel.trim().to_owned();
            if trimmed_ch.is_empty() {
                return Err(ApiErrorDto::invalid_config(
                    "channel_counter sync channel must not be empty",
                ));
            }
            if !ns_per_count.is_finite() || ns_per_count <= 0.0 {
                return Err(ApiErrorDto::invalid_config(
                    "channel_counter ns_per_count must be finite and > 0",
                ));
            }
            if let Some(m) = rollover_modulus {
                if !m.is_finite() || m <= 0.0 {
                    return Err(ApiErrorDto::invalid_config(
                        "channel_counter rollover_modulus must be finite and > 0",
                    ));
                }
            }
            ClockMode::ChannelCounter {
                channel: trimmed_ch,
                ns_per_count,
                rollover_modulus,
                drift_compensation,
            }
        }
    };

    let protocol = match req.protocol {
        ProtocolConfigDto::SimPreset => SourceProtocolConfig::SimPreset(SimPresetConfig {
            source_id: source_id_str.to_owned(),
            analog_channels: sim_analog_channels,
            ..SimPresetConfig::default()
        }),
        ProtocolConfigDto::JustFloat { channel_names } => {
            SourceProtocolConfig::JustFloat(JustFloatConfig::new(source_id_str, channel_names))
        }
        ProtocolConfigDto::FireWater { channel_names } => SourceProtocolConfig::FireWater(
            FireWaterConfig::from_channel_names(source_id_str, channel_names),
        ),
        ProtocolConfigDto::FixedSchemaBinary {
            header_magic,
            fields,
            has_hw_tick_first_col,
        } => {
            let mut core_fields = Vec::with_capacity(fields.len());
            for f in fields {
                let st = parse_scalar_type(&f.scalar_type)?;
                if !f.scale.is_finite() || !f.offset.is_finite() {
                    return Err(ApiErrorDto::invalid_config(format!(
                        "field `{}` scale/offset must be finite",
                        f.name
                    )));
                }
                let mut spec =
                    FixedFieldSpec::scalar(f.name, st).with_scale(LinearScale::new(f.scale, f.offset));
                if let Some(unit) = f.unit {
                    spec = spec.with_unit(unit);
                }
                spec.is_state_enum = f.is_state_enum;
                core_fields.push(spec);
            }
            SourceProtocolConfig::FixedSchemaBinary(FixedSchemaBinaryConfig {
                source_id: source_id_str.to_owned(),
                header_magic,
                tail_magic: None,
                checksum: ChecksumKind::default(),
                checksum_scope: ChecksumScope::default(),
                checksum_endian: Endianness::default(),
                has_hw_tick_first_col,
                fields: core_fields,
                max_carry_bytes: 64 * 1024,
            })
        }
    };

    Ok(SourceConfig {
        source_id: SourceId::new(source_id_str),
        display_name: req.display_name,
        transport,
        clock_mode,
        clock_config: ClockSyncConfig::default(),
        protocol,
        runtime: TransportRuntimeConfig::default(),
        auto_start: req.auto_start,
    })
}

/// Converts a core [`SourceConfig`] back into a control-plane [`CreateSourceRequestDto`].
pub fn source_config_core_to_dto(cfg: &SourceConfig) -> Result<CreateSourceRequestDto, ApiErrorDto> {
    let transport = match &cfg.transport {
        SourceTransportConfig::Sim(sim) => TransportConfigDto::Sim {
            sample_rate_hz: sim.sample_rate_hz,
            analog_channels: sim.analog_channels,
            enable_glitch: sim.enable_glitch,
            glitch_period_samples: sim.glitch_period_samples,
        },
        SourceTransportConfig::Serial(ser) => TransportConfigDto::Serial {
            port_name: ser.port_name.clone(),
            baud_rate: ser.baud_rate,
            data_bits: ser.data_bits,
            parity: match ser.parity {
                SerialParity::None => SerialParityDto::None,
                SerialParity::Odd => SerialParityDto::Odd,
                SerialParity::Even => SerialParityDto::Even,
            },
            stop_bits: match ser.stop_bits {
                SerialStopBits::One => SerialStopBitsDto::One,
                SerialStopBits::Two => SerialStopBitsDto::Two,
            },
            flow_control: match ser.flow_control {
                SerialFlowControl::None => SerialFlowControlDto::None,
                SerialFlowControl::HardwareRtsCts => SerialFlowControlDto::HardwareRtsCts,
                SerialFlowControl::SoftwareXonXoff => SerialFlowControlDto::SoftwareXonXoff,
            },
            auto_reconnect: ser.auto_reconnect,
        },
        SourceTransportConfig::Udp(udp) => TransportConfigDto::Udp {
            bind_addr: udp.bind_addr.clone(),
            multicast_group: udp.multicast_group.clone(),
            recv_buffer_bytes: udp.recv_buffer_bytes,
            remote_tx_addr: udp.remote_tx_addr.clone(),
        },
        SourceTransportConfig::TcpClient(tcp) => TransportConfigDto::TcpClient {
            remote_addr: tcp.remote_addr.clone(),
            auto_reconnect: tcp.auto_reconnect,
            tcp_nodelay: tcp.tcp_nodelay,
        },
        SourceTransportConfig::TcpServer(srv) => TransportConfigDto::TcpServer {
            listen_addr: srv.listen_addr.clone(),
            tcp_nodelay: srv.tcp_nodelay,
        },
        SourceTransportConfig::RawFileReplay(rep) => TransportConfigDto::RawFileReplay {
            file_path: rep.file_path.to_string_lossy().into_owned(),
            full_speed_offline: rep.full_speed_offline,
            playback_speed: rep.playback_speed,
            loop_playback: rep.loop_playback,
            nominal_byte_rate: rep.nominal_byte_rate,
        },
        SourceTransportConfig::Custom { transport_kind, .. } => {
            return Err(ApiErrorDto::invalid_config(format!(
                "custom transport `{}` is not exportable as CreateSourceRequestDto",
                transport_kind_to_str(*transport_kind)
            )));
        }
    };

    let clock = match &cfg.clock_mode {
        ClockMode::Tier1HardwareCounter {
            tick_hz,
            counter_bits,
        } => ClockConfigDto::Tier1HardwareCounter {
            tick_hz: *tick_hz,
            counter_bits: *counter_bits,
        },
        ClockMode::Tier2SoftwarePll {
            nominal_sample_rate_hz,
        } => ClockConfigDto::Tier2SoftwarePll {
            nominal_sample_rate_hz: *nominal_sample_rate_hz,
        },
        ClockMode::Tier3HostArrival => ClockConfigDto::Tier3HostArrival,
        ClockMode::ChannelCounter {
            channel,
            ns_per_count,
            rollover_modulus,
            drift_compensation,
        } => ClockConfigDto::ChannelCounter {
            channel: channel.clone(),
            ns_per_count: *ns_per_count,
            rollover_modulus: *rollover_modulus,
            drift_compensation: *drift_compensation,
        },
    };

    let protocol = match &cfg.protocol {
        SourceProtocolConfig::SimPreset(_) => ProtocolConfigDto::SimPreset,
        SourceProtocolConfig::JustFloat(jf) => ProtocolConfigDto::JustFloat {
            channel_names: jf.channel_names.clone(),
        },
        SourceProtocolConfig::FireWater(fw) => ProtocolConfigDto::FireWater {
            channel_names: if !fw.channel_names.is_empty() {
                fw.channel_names.clone()
            } else {
                fw.columns.iter().map(|c| c.name.clone()).collect()
            },
        },
        SourceProtocolConfig::FixedSchemaBinary(fb) => ProtocolConfigDto::FixedSchemaBinary {
            header_magic: fb.header_magic.clone(),
            fields: fb
                .fields
                .iter()
                .map(|f| FixedFieldSpecDto {
                    name: f.name.clone(),
                    scalar_type: scalar_type_to_str(f.scalar_type).to_string(),
                    scale: f.scale.scale,
                    offset: f.scale.offset,
                    unit: f.unit.clone(),
                    is_state_enum: f.is_state_enum,
                })
                .collect(),
            has_hw_tick_first_col: fb.has_hw_tick_first_col,
        },
        SourceProtocolConfig::Custom { codec_id, .. } => {
            return Err(ApiErrorDto::invalid_config(format!(
                "custom protocol `{codec_id}` is not exportable as CreateSourceRequestDto"
            )));
        }
    };

    Ok(CreateSourceRequestDto {
        source_id: cfg.source_id.as_str().to_owned(),
        display_name: cfg.display_name.clone(),
        transport,
        clock,
        protocol,
        auto_start: cfg.auto_start,
    })
}

fn registered_channel_to_dto(ch: RegisteredChannelInfo) -> ChannelNodeDto {
    let (modality, scalar_type, state_labels) = match ch.kind {
        ChannelKind::Scalar {
            scalar_type: ScalarType::Bool,
            ..
        } => (
            ChannelModalityDto::DigitalBool,
            Some("bool".to_string()),
            Vec::new(),
        ),
        ChannelKind::Scalar { scalar_type, .. } => (
            ChannelModalityDto::Scalar,
            Some(scalar_type_to_str(scalar_type).to_string()),
            Vec::new(),
        ),
        ChannelKind::Frame { element_type, .. } => (
            ChannelModalityDto::Frame,
            Some(scalar_type_to_str(element_type).to_string()),
            Vec::new(),
        ),
        ChannelKind::Event {
            schema: EventSchemaKind::StateMachine { labels },
        } => {
            let entries = labels
                .into_iter()
                .map(|(code, lbl)| StateLabelEntryDto {
                    code,
                    name: lbl.name,
                    color_rgba: lbl.color_rgba,
                })
                .collect();
            (
                ChannelModalityDto::StateMachine,
                Some("i32".to_string()),
                entries,
            )
        }
        ChannelKind::Event { .. } => (ChannelModalityDto::EventLog, None, Vec::new()),
    };

    ChannelNodeDto {
        channel_id: ch.channel_id.0,
        source_id: ch.source_id.as_str().to_owned(),
        local_path: ch.local_path,
        full_path: ch.full_path,
        display_name: ch.display_name,
        unit: ch.unit,
        modality,
        scalar_type,
        state_labels,
    }
}

/// Builds a [`ChannelTreeSnapshotDto`] from live [`CoreWorkspace`] sources, channels, and RAM quota.
pub fn build_channel_tree_dto(ws: &CoreWorkspace) -> ChannelTreeSnapshotDto {
    let mut source_summaries: std::collections::BTreeMap<String, SourceSummaryDto> = ws
        .list_sources()
        .into_iter()
        .map(|s| {
            let dto = source_summary_to_dto(s);
            (dto.source_id.clone(), dto)
        })
        .collect();
    let any_source_running = source_summaries.values().any(|s| {
        matches!(
            s.state,
            SourceRuntimeStateDto::Running
                | SourceRuntimeStateDto::Connecting
                | SourceRuntimeStateDto::Reconnecting
        )
    });

    let core_tree = ws.channel_tree();
    let mut groups = Vec::with_capacity(core_tree.sources.len().max(source_summaries.len()));

    for group in core_tree.sources {
        let src_id_str = group.source_id.as_str().to_owned();
        let summary = source_summaries.remove(&src_id_str).unwrap_or_else(|| {
            if src_id_str == ddscope_core::VIRTUAL_SOURCE_ID {
                SourceSummaryDto {
                    source_id: src_id_str.clone(),
                    display_name: "Virtual Channels".to_string(),
                    transport_kind: "virtual".to_string(),
                    state: if any_source_running {
                        SourceRuntimeStateDto::Running
                    } else {
                        SourceRuntimeStateDto::Stopped
                    },
                    raw_recording_path: None,
                }
            } else {
                SourceSummaryDto {
                    source_id: src_id_str.clone(),
                    display_name: src_id_str,
                    transport_kind: "unknown".to_string(),
                    state: SourceRuntimeStateDto::Stopped,
                    raw_recording_path: None,
                }
            }
        });
        let channels = group
            .channels
            .into_iter()
            .map(registered_channel_to_dto)
            .collect();
        groups.push(SourceChannelGroupDto {
            source: summary,
            channels,
        });
    }

    for (_id, summary) in source_summaries {
        groups.push(SourceChannelGroupDto {
            source: summary,
            channels: Vec::new(),
        });
    }

    let quota = ws.quota_stats();
    ChannelTreeSnapshotDto {
        sources: groups,
        quota_limit_bytes: quota.quota_bytes as f64,
        quota_used_bytes: quota.allocated_bytes as f64,
        pinned_bytes: quota.pinned_bytes as f64,
    }
}

/// Converts [`ScopeViewportConfigDto`] into a validated core [`ScopeConfig`].
pub fn scope_config_dto_to_core(dto: ScopeViewportConfigDto) -> Result<ScopeConfig, ApiErrorDto> {
    if !dto.time_span_ns.is_finite() || dto.time_span_ns <= 0.0 {
        return Err(ApiErrorDto::invalid_config(
            "scope time_span_ns must be finite and > 0",
        ));
    }
    if !dto.pan_offset_ns.is_finite() {
        return Err(ApiErrorDto::invalid_config(
            "scope pan_offset_ns must be finite",
        ));
    }
    if !dto.pre_trigger_ratio.is_finite() {
        return Err(ApiErrorDto::invalid_config(
            "scope pre_trigger_ratio must be finite",
        ));
    }

    let sweep_mode = match dto.sweep_mode {
        TriggerSweepModeDto::Roll => TriggerSweepMode::Roll,
        TriggerSweepModeDto::Auto => TriggerSweepMode::Auto,
        TriggerSweepModeDto::Normal => TriggerSweepMode::Normal,
        TriggerSweepModeDto::Single => TriggerSweepMode::Single,
    };

    let trigger = match dto.trigger {
        Some(TriggerConditionDto::Edge {
            channel_id,
            level,
            hysteresis,
            slope,
        }) => {
            if !level.is_finite() || !hysteresis.is_finite() {
                return Err(ApiErrorDto::invalid_config(
                    "edge trigger level and hysteresis must be finite",
                ));
            }
            Some(TriggerCondition::Edge {
                channel: ChannelId(channel_id),
                level,
                hysteresis: hysteresis.abs(),
                slope: match slope {
                    EdgeSlopeDto::Rising => EdgeSlope::Rising,
                    EdgeSlopeDto::Falling => EdgeSlope::Falling,
                    EdgeSlopeDto::Either => EdgeSlope::Either,
                },
            })
        }
        Some(TriggerConditionDto::StateEquals {
            channel_id,
            state_code,
        }) => Some(TriggerCondition::StateEquals {
            channel: ChannelId(channel_id),
            state_code,
        }),
        None => None,
    };

    let width_px = dto.canvas_width_px.clamp(64, 16_384);
    let scope_id = ScopeId(dto.scope_id);
    let sync_group_id = dto.sync_group_id.map(SyncGroupId);

    let tracks = dto
        .tracks
        .into_iter()
        .filter(|t| t.visible)
        .map(|t| ScopeTrackBinding {
            channel_id: ChannelId(t.channel_id),
            lane_kind: match t.lane {
                ScopeLaneKindDto::Analog => ScopeLaneKind::Analog,
                ScopeLaneKindDto::Digital => ScopeLaneKind::Digital,
                ScopeLaneKindDto::StateRibbon => ScopeLaneKind::StateRibbon,
            },
            color_rgba: t.color_rgba,
            label: None,
        })
        .collect();

    let controller = ScopeControllerConfig {
        scope_id,
        title: dto.title.clone(),
        canvas_width_px: width_px,
        time_span_ns: dto.time_span_ns.max(1.0).round() as u64,
        pre_trigger_ratio: dto.pre_trigger_ratio.clamp(0.0, 1.0),
        pan_offset_ns: dto.pan_offset_ns.round() as i64,
        sweep_mode,
        auto_timeout_ns: u64::from(dto.auto_timeout_ms.max(1)) * 1_000_000,
        trigger,
        sync_group_id,
        paused: dto.paused,
    };

    Ok(ScopeConfig {
        scope_id,
        title: dto.title,
        controller,
        tracks,
        sync_group_id,
        width_px,
        auto_pin_on_latch: true,
    })
}

/// Converts a core [`ScopeSummary`] to [`ScopeViewportSummaryDto`].
pub fn scope_summary_to_dto(summary: ScopeSummary) -> ScopeViewportSummaryDto {
    ScopeViewportSummaryDto {
        scope_id: summary.scope_id.0,
        active_track_count: summary.track_count.min(u32::MAX as usize) as u32,
        sync_group_id: summary.sync_group_id.map(|g| g.0),
    }
}

/// Converts [`SyncGroupUpdateDto`] into core [`SyncGroupUpdate`].
pub fn sync_group_update_dto_to_core(
    dto: SyncGroupUpdateDto,
) -> Result<SyncGroupUpdate, ApiErrorDto> {
    if !dto.time_span_ns.is_finite() || dto.time_span_ns <= 0.0 {
        return Err(ApiErrorDto::invalid_config(
            "sync group time_span_ns must be finite and > 0",
        ));
    }
    if !dto.center_offset_ns.is_finite() {
        return Err(ApiErrorDto::invalid_config(
            "sync group center_offset_ns must be finite",
        ));
    }
    Ok(SyncGroupUpdate {
        group_id: SyncGroupId(dto.group_id),
        origin_scope_id: Some(ScopeId(dto.origin_scope_id)),
        time_span_ns: dto.time_span_ns.max(1.0).round() as u64,
        center_offset_ns: dto.center_offset_ns.round() as i64,
        cursor_a_ratio: dto.cursor_a_ratio,
        cursor_b_ratio: dto.cursor_b_ratio,
    })
}

/// Converts [`SendDownlinkRequestDto`] into target [`SourceId`] and core [`DownlinkRequest`].
pub fn downlink_dto_to_core(
    dto: SendDownlinkRequestDto,
) -> Result<(SourceId, DownlinkRequest), ApiErrorDto> {
    if dto.source_id.trim().is_empty() {
        return Err(ApiErrorDto::invalid_config(
            "downlink source_id must not be empty",
        ));
    }

    let priority = match dto.priority {
        TxPriorityDto::High => TxPriority::High,
        TxPriorityDto::Normal => TxPriority::Normal,
    };

    let mut req = match dto.payload {
        DownlinkPayloadDto::HexBytes { hex } => {
            DownlinkRequest::from_hex(&hex, priority).map_err(ApiErrorDto::from)?
        }
        DownlinkPayloadDto::RawBytes { bytes } => {
            DownlinkRequest::new(DownlinkPayload::RawBytes(bytes), priority)
        }
        DownlinkPayloadDto::AsciiText {
            text,
            append_newline,
        } => {
            if append_newline {
                DownlinkRequest::from_ascii(text, true, priority)
            } else {
                // Use RawBytes so BasicDownlinkEncoder::default()'s ensure_newline=true
                // does not force a trailing '\n' when exact string bytes are requested.
                DownlinkRequest::new(DownlinkPayload::RawBytes(text.into_bytes()), priority)
            }
        }
        DownlinkPayloadDto::KeyValue { key, value } => {
            if !value.is_finite() {
                return Err(ApiErrorDto::invalid_config(format!(
                    "downlink KeyValue `{key}` value must be finite"
                )));
            }
            DownlinkRequest::from_key_value(key, ScalarValue::F64(value), priority)
        }
    };

    if let Some(coalesce_key) = dto.coalesce_key.filter(|k| !k.trim().is_empty()) {
        req = req.with_coalesce_key(coalesce_key);
    }

    Ok((SourceId::new(dto.source_id), req))
}

/// Converts a core [`TxDispatchReceipt`] to [`DownlinkReceiptDto`].
pub fn tx_receipt_to_dto(receipt: ddscope_core::workspace::TxDispatchReceipt) -> DownlinkReceiptDto {
    let result_kind = match receipt.result {
        TransactionResult::SentFireAndForget => "sent_fire_and_forget",
        TransactionResult::AckSuccess { .. } => "ack_success",
        TransactionResult::AckRejected { .. } => "ack_rejected",
        TransactionResult::TimedOut { .. } => "timed_out",
    };

    DownlinkReceiptDto {
        accepted: receipt.accepted,
        coalesced_existing: receipt.coalesced_existing,
        result_kind: result_kind.to_string(),
    }
}

/// Converts a core [`WorkspaceTelemetrySnapshot`] into a [`WorkspaceTelemetryEvent`] payload.
pub fn telemetry_snapshot_to_event(
    snap: &WorkspaceTelemetrySnapshot,
    timestamp_ms: f64,
) -> WorkspaceTelemetryEvent {
    let sources = snap
        .sources
        .iter()
        .map(source_telemetry_to_dto)
        .collect();

    WorkspaceTelemetryEvent {
        timestamp_ms,
        sources,
        quota_used_bytes: snap.quota.allocated_bytes as f64,
        quota_limit_bytes: snap.quota.quota_bytes as f64,
        pinned_bytes: snap.quota.pinned_bytes as f64,
    }
}

fn source_telemetry_to_dto(s: &SourceTelemetrySnapshot) -> SourceTelemetryDto {
    SourceTelemetryDto {
        source_id: s.source_id.as_str().to_owned(),
        state: source_state_to_dto(s.state),
        samples_per_sec: s.samples_per_sec,
        bytes_per_sec: s.bytes_per_sec,
        dropped_packets: s.dropped_packets.min(u32::MAX as u64) as u32,
    }
}

/// Converts a [`VirtualNodeConfigDto`] into a core [`VirtualNodeConfig`].
pub fn virtual_node_dto_to_core(dto: VirtualNodeConfigDto) -> VirtualNodeConfig {
    let operator = match dto.operator {
        VirtualOperatorDto::AnalogToDigital {
            input_channel,
            output_path,
            mode,
        } => VirtualOperatorConfig::AnalogToDigital {
            input_channel,
            output_path,
            mode: match mode {
                AnalogToDigitalModeDto::Schmitt {
                    threshold,
                    hysteresis,
                    invert,
                } => AnalogToDigitalMode::Schmitt {
                    threshold,
                    hysteresis,
                    invert,
                },
                AnalogToDigitalModeDto::Window { low, high, invert } => {
                    AnalogToDigitalMode::Window { low, high, invert }
                }
            },
        },
        VirtualOperatorDto::ToStateMachine { output_path, mode } => {
            VirtualOperatorConfig::ToStateMachine {
                output_path,
                mode: match mode {
                    ToStateMachineModeDto::RangeMap {
                        input_channel,
                        hysteresis,
                        rules,
                        default_state,
                    } => ToStateMachineMode::RangeMap {
                        input_channel,
                        hysteresis,
                        rules: rules
                            .into_iter()
                            .map(|r| StateRangeRule {
                                min: r.min,
                                max: r.max,
                                code: r.code,
                                name: r.name,
                                color_rgba: r.color_rgba,
                            })
                            .collect(),
                        default_state: StateLabelSpec {
                            code: default_state.code,
                            name: default_state.name,
                            color_rgba: default_state.color_rgba,
                        },
                    },
                    ToStateMachineModeDto::BitCombine {
                        bit_channels,
                        threshold,
                        labels,
                    } => ToStateMachineMode::BitCombine {
                        bit_channels,
                        threshold,
                        labels: labels
                            .into_iter()
                            .map(|l| StateLabelSpec {
                                code: l.code,
                                name: l.name,
                                color_rgba: l.color_rgba,
                            })
                            .collect(),
                    },
                },
            }
        }
        VirtualOperatorDto::ToAnalog {
            output_path,
            unit,
            mode,
        } => VirtualOperatorConfig::ToAnalog {
            output_path,
            unit,
            mode: match mode {
                ToAnalogModeDto::LevelMap {
                    input_channel,
                    low_value,
                    high_value,
                    threshold,
                    smoothing_tau_ms,
                } => ToAnalogMode::LevelMap {
                    input_channel,
                    low_value,
                    high_value,
                    threshold,
                    smoothing_tau_ms,
                },
                ToAnalogModeDto::BitWeightedDac {
                    bit_channels,
                    scale,
                    offset,
                    threshold,
                } => ToAnalogMode::BitWeightedDac {
                    bit_channels,
                    scale,
                    offset,
                    threshold,
                },
                ToAnalogModeDto::StateValueMap {
                    input_channel,
                    entries,
                    default_value,
                } => ToAnalogMode::StateValueMap {
                    input_channel,
                    entries: entries
                        .into_iter()
                        .map(|e| StateValueEntry {
                            code: e.code,
                            value: e.value,
                        })
                        .collect(),
                    default_value,
                },
            },
        },
        VirtualOperatorDto::Formula {
            expression,
            output_path,
            unit,
            output_digital,
        } => VirtualOperatorConfig::Formula {
            expression,
            output_path,
            unit,
            output_digital,
        },
        VirtualOperatorDto::WindowStats {
            input_channel,
            output_path,
            unit,
            metric,
            window_ms,
        } => VirtualOperatorConfig::WindowStats {
            input_channel,
            output_path,
            unit,
            metric: match metric {
                WindowMetricKindDto::Rms => WindowMetricKind::Rms,
                WindowMetricKindDto::Mean => WindowMetricKind::Mean,
                WindowMetricKindDto::PeakToPeak => WindowMetricKind::PeakToPeak,
                WindowMetricKindDto::Min => WindowMetricKind::Min,
                WindowMetricKindDto::Max => WindowMetricKind::Max,
                WindowMetricKindDto::StdDev => WindowMetricKind::StdDev,
            },
            window_ms,
        },
        VirtualOperatorDto::Calculus {
            input_channel,
            output_path,
            unit,
            mode,
        } => VirtualOperatorConfig::Calculus {
            input_channel,
            output_path,
            unit,
            mode: match mode {
                CalculusModeDto::Derivative {
                    scale,
                    filter_tau_ms,
                } => CalculusMode::Derivative {
                    scale,
                    filter_tau_ms,
                },
                CalculusModeDto::Integral {
                    scale,
                    dc_offset,
                    initial_value,
                    clamp_min,
                    clamp_max,
                } => CalculusMode::Integral {
                    scale,
                    dc_offset,
                    initial_value,
                    clamp_min,
                    clamp_max,
                },
            },
        },
        VirtualOperatorDto::SinglePhasePower {
            voltage_channel,
            current_channel,
            output_prefix,
            voltage_scale,
            voltage_offset,
            current_scale,
            current_offset,
            nominal_freq_hz,
            zero_cross_hysteresis_v,
            min_freq_hz,
            fallback_window_ms,
            outputs,
        } => VirtualOperatorConfig::SinglePhasePower {
            voltage_channel,
            current_channel,
            output_prefix,
            voltage_scale,
            voltage_offset,
            current_scale,
            current_offset,
            nominal_freq_hz,
            zero_cross_hysteresis_v,
            min_freq_hz,
            fallback_window_ms,
            outputs: PowerOutputFlags {
                freq: outputs.freq,
                u_rms: outputs.u_rms,
                i_rms: outputs.i_rms,
                p: outputs.p,
                q1: outputs.q1,
                q_tot: outputs.q_tot,
                s: outputs.s,
                pf: outputs.pf,
                phi_deg: outputs.phi_deg,
                u_pk: outputs.u_pk,
                i_pk: outputs.i_pk,
                energy_wh: outputs.energy_wh,
            },
        },
    };

    VirtualNodeConfig {
        node_id: dto.node_id,
        name: dto.name,
        enabled: dto.enabled,
        operator,
    }
}

/// Converts a core [`VirtualNodeConfig`] into [`VirtualNodeConfigDto`].
pub fn virtual_node_core_to_dto(cfg: VirtualNodeConfig) -> VirtualNodeConfigDto {
    let operator = match cfg.operator {
        VirtualOperatorConfig::AnalogToDigital {
            input_channel,
            output_path,
            mode,
        } => VirtualOperatorDto::AnalogToDigital {
            input_channel,
            output_path,
            mode: match mode {
                AnalogToDigitalMode::Schmitt {
                    threshold,
                    hysteresis,
                    invert,
                } => AnalogToDigitalModeDto::Schmitt {
                    threshold,
                    hysteresis,
                    invert,
                },
                AnalogToDigitalMode::Window { low, high, invert } => {
                    AnalogToDigitalModeDto::Window { low, high, invert }
                }
            },
        },
        VirtualOperatorConfig::ToStateMachine { output_path, mode } => {
            VirtualOperatorDto::ToStateMachine {
                output_path,
                mode: match mode {
                    ToStateMachineMode::RangeMap {
                        input_channel,
                        hysteresis,
                        rules,
                        default_state,
                    } => ToStateMachineModeDto::RangeMap {
                        input_channel,
                        hysteresis,
                        rules: rules
                            .into_iter()
                            .map(|r| StateRangeRuleDto {
                                min: r.min,
                                max: r.max,
                                code: r.code,
                                name: r.name,
                                color_rgba: r.color_rgba,
                            })
                            .collect(),
                        default_state: StateLabelSpecDto {
                            code: default_state.code,
                            name: default_state.name,
                            color_rgba: default_state.color_rgba,
                        },
                    },
                    ToStateMachineMode::BitCombine {
                        bit_channels,
                        threshold,
                        labels,
                    } => ToStateMachineModeDto::BitCombine {
                        bit_channels,
                        threshold,
                        labels: labels
                            .into_iter()
                            .map(|l| StateLabelSpecDto {
                                code: l.code,
                                name: l.name,
                                color_rgba: l.color_rgba,
                            })
                            .collect(),
                    },
                },
            }
        }
        VirtualOperatorConfig::ToAnalog {
            output_path,
            unit,
            mode,
        } => VirtualOperatorDto::ToAnalog {
            output_path,
            unit,
            mode: match mode {
                ToAnalogMode::LevelMap {
                    input_channel,
                    low_value,
                    high_value,
                    threshold,
                    smoothing_tau_ms,
                } => ToAnalogModeDto::LevelMap {
                    input_channel,
                    low_value,
                    high_value,
                    threshold,
                    smoothing_tau_ms,
                },
                ToAnalogMode::BitWeightedDac {
                    bit_channels,
                    scale,
                    offset,
                    threshold,
                } => ToAnalogModeDto::BitWeightedDac {
                    bit_channels,
                    scale,
                    offset,
                    threshold,
                },
                ToAnalogMode::StateValueMap {
                    input_channel,
                    entries,
                    default_value,
                } => ToAnalogModeDto::StateValueMap {
                    input_channel,
                    entries: entries
                        .into_iter()
                        .map(|e| StateValueEntryDto {
                            code: e.code,
                            value: e.value,
                        })
                        .collect(),
                    default_value,
                },
            },
        },
        VirtualOperatorConfig::Formula {
            expression,
            output_path,
            unit,
            output_digital,
        } => VirtualOperatorDto::Formula {
            expression,
            output_path,
            unit,
            output_digital,
        },
        VirtualOperatorConfig::WindowStats {
            input_channel,
            output_path,
            unit,
            metric,
            window_ms,
        } => VirtualOperatorDto::WindowStats {
            input_channel,
            output_path,
            unit,
            metric: match metric {
                WindowMetricKind::Rms => WindowMetricKindDto::Rms,
                WindowMetricKind::Mean => WindowMetricKindDto::Mean,
                WindowMetricKind::PeakToPeak => WindowMetricKindDto::PeakToPeak,
                WindowMetricKind::Min => WindowMetricKindDto::Min,
                WindowMetricKind::Max => WindowMetricKindDto::Max,
                WindowMetricKind::StdDev => WindowMetricKindDto::StdDev,
            },
            window_ms,
        },
        VirtualOperatorConfig::Calculus {
            input_channel,
            output_path,
            unit,
            mode,
        } => VirtualOperatorDto::Calculus {
            input_channel,
            output_path,
            unit,
            mode: match mode {
                CalculusMode::Derivative {
                    scale,
                    filter_tau_ms,
                } => CalculusModeDto::Derivative {
                    scale,
                    filter_tau_ms,
                },
                CalculusMode::Integral {
                    scale,
                    dc_offset,
                    initial_value,
                    clamp_min,
                    clamp_max,
                } => CalculusModeDto::Integral {
                    scale,
                    dc_offset,
                    initial_value,
                    clamp_min,
                    clamp_max,
                },
            },
        },
        VirtualOperatorConfig::SinglePhasePower {
            voltage_channel,
            current_channel,
            output_prefix,
            voltage_scale,
            voltage_offset,
            current_scale,
            current_offset,
            nominal_freq_hz,
            zero_cross_hysteresis_v,
            min_freq_hz,
            fallback_window_ms,
            outputs,
        } => VirtualOperatorDto::SinglePhasePower {
            voltage_channel,
            current_channel,
            output_prefix,
            voltage_scale,
            voltage_offset,
            current_scale,
            current_offset,
            nominal_freq_hz,
            zero_cross_hysteresis_v,
            min_freq_hz,
            fallback_window_ms,
            outputs: PowerOutputFlagsDto {
                freq: outputs.freq,
                u_rms: outputs.u_rms,
                i_rms: outputs.i_rms,
                p: outputs.p,
                q1: outputs.q1,
                q_tot: outputs.q_tot,
                s: outputs.s,
                pf: outputs.pf,
                phi_deg: outputs.phi_deg,
                u_pk: outputs.u_pk,
                i_pk: outputs.i_pk,
                energy_wh: outputs.energy_wh,
            },
        },
    };

    VirtualNodeConfigDto {
        node_id: cfg.node_id,
        name: cfg.name,
        enabled: cfg.enabled,
        operator,
    }
}

/// Converts a core [`VirtualNodeSummary`] into [`VirtualNodeSummaryDto`] using `ws` channel metadata.
pub fn virtual_summary_to_dto(ws: &CoreWorkspace, summary: VirtualNodeSummary) -> VirtualNodeSummaryDto {
    let mut by_id = std::collections::HashMap::new();
    for grp in ws.channel_tree().sources {
        if grp.source_id.as_str() == ddscope_core::VIRTUAL_SOURCE_ID {
            for ch in grp.channels {
                by_id.insert(ch.channel_id, registered_channel_to_dto(ch));
            }
        }
    }
    let output_channels = summary
        .output_channels
        .into_iter()
        .filter_map(|cid| by_id.remove(&cid))
        .collect();
    VirtualNodeSummaryDto {
        node_id: summary.node_id,
        name: summary.name,
        enabled: summary.enabled,
        output_channels,
    }
}
