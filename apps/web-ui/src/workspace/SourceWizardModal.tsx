import { useEffect, useMemo, useRef, useState, type FormEvent } from "react";
import type {
  ClockConfigDto_Deserialize,
  CreateSourceRequestDto_Deserialize,
  CreateSourceRequestDto_Serialize,
  ProtocolConfigDto_Deserialize,
  SerialFlowControlDto,
  SerialParityDto,
  SerialPortInfoDto_Serialize,
  SerialStopBitsDto,
  TransportConfigDto_Deserialize,
} from "../bridge/bindings.ts";
import {
  bridgeClient,
  formatUdpRemoteTxAddr,
  parseUdpRemoteTxAddr,
} from "../bridge/index.ts";
import { formatFrequencyHz } from "../scope-engine/time-format.ts";
import { synthesizeFallbackSourceConfig } from "./config-io.ts";
import { useWorkspaceStore } from "./store.ts";
import { useI18n } from "./use-i18n.ts";

type WizardTransportKind =
  | "sim"
  | "serial"
  | "udp"
  | "tcp_client"
  | "tcp_server"
  | "raw_file_replay";

type WizardClockMode =
  | "tier1_hardware_counter"
  | "tier2_software_pll"
  | "tier3_host_arrival"
  | "channel_counter";

type RolloverPreset =
  | "auto"
  | "256"
  | "65536"
  | "16777216"
  | "4294967296"
  | "custom";

function rolloverModulusToPreset(mod: number | null | undefined): RolloverPreset {
  if (mod == null) return "auto";
  switch (mod) {
    case 256:
      return "256";
    case 65536:
      return "65536";
    case 16777216:
      return "16777216";
    case 4294967296:
      return "4294967296";
    default:
      return "custom";
  }
}

type WizardProtocolCodec =
  | "sim_preset"
  | "just_float"
  | "fire_water"
  | "fixed_schema_binary";

export function SourceWizardModal() {
  const { t } = useI18n();
  const open = useWorkspaceStore((s) => s.sourceWizardOpen);
  const editingSourceId = useWorkspaceStore((s) => s.editingSourceId);
  const setOpen = useWorkspaceStore((s) => s.setSourceWizardOpen);
  const createSource = useWorkspaceStore((s) => s.createSource);
  const updateSource = useWorkspaceStore((s) => s.updateSource);
  const channelGroups = useWorkspaceStore((s) => s.channelGroups);
  const sourceConfigsById = useWorkspaceStore((s) => s.sourceConfigsById);
  const isEditMode = editingSourceId !== null;

  const [sourceId, setSourceId] = useState("sim1");
  const [displayName, setDisplayName] = useState("Auxiliary Test Source");
  const [transportKind, setTransportKind] = useState<WizardTransportKind>("sim");
  const [clockMode, setClockMode] = useState<WizardClockMode>("tier2_software_pll");
  const [protocolCodec, setProtocolCodec] = useState<WizardProtocolCodec>("sim_preset");
  const [autoStart, setAutoStart] = useState(true);

  // Sim fields
  const [simRateHz, setSimRateHz] = useState(1_000_000);
  const [simChannels, setSimChannels] = useState(4);
  const [simGlitch, setSimGlitch] = useState(true);

  // Serial fields
  const [serialPorts, setSerialPorts] = useState<SerialPortInfoDto_Serialize[]>([]);
  const [portName, setPortName] = useState("COM3");
  const [baudRate, setBaudRate] = useState(2_000_000);
  const [parity, setParity] = useState<SerialParityDto>("none");
  const [stopBits, setStopBits] = useState<SerialStopBitsDto>("one");
  const [flowControl, setFlowControl] = useState<SerialFlowControlDto>("none");

  // Network fields
  const [udpBindAddr, setUdpBindAddr] = useState("0.0.0.0:9001");
  const [udpRemoteIp, setUdpRemoteIp] = useState("");
  const [udpRemotePort, setUdpRemotePort] = useState(9002);
  const [tcpAddr, setTcpAddr] = useState("127.0.0.1:9100");

  // Replay fields
  const [replayPath, setReplayPath] = useState("captures/motor_run.bin");
  const [replayFullSpeed, setReplayFullSpeed] = useState(false);
  const [replaySpeed, setReplaySpeed] = useState(1.0);
  const [replayLoop, setReplayLoop] = useState(true);

  // Clock & Protocol fields
  const [pllAutoEstimate, setPllAutoEstimate] = useState(false);
  const [nominalRateHz, setNominalRateHz] = useState(500_000);
  const [hwTickHz, setHwTickHz] = useState(1_000_000);
  const [syncChannel, setSyncChannel] = useState("iq");
  const [nsPerCount, setNsPerCount] = useState(1_000);
  const [rolloverPreset, setRolloverPreset] = useState<RolloverPreset>("auto");
  const [customRolloverModulus, setCustomRolloverModulus] = useState(10_000);
  const [syncDriftComp, setSyncDriftComp] = useState(true);
  const [channelNamesText, setChannelNamesText] = useState("va, vb, vc, iq");

  const [submitting, setSubmitting] = useState(false);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);

  const prevOpenRef = useRef(false);
  const prevEditingIdRef = useRef<string | null>(null);
  const baseConfigRef = useRef<CreateSourceRequestDto_Serialize | null>(null);

  useEffect(() => {
    if (!open) {
      prevOpenRef.current = false;
      prevEditingIdRef.current = null;
      baseConfigRef.current = null;
      return;
    }

    const sessionChanged =
      !prevOpenRef.current || prevEditingIdRef.current !== editingSourceId;
    if (!sessionChanged) {
      return;
    }
    prevOpenRef.current = true;
    prevEditingIdRef.current = editingSourceId;
    setErrorMsg(null);

    if (editingSourceId !== null) {
      const grp = channelGroups.find((g) => g.source.source_id === editingSourceId);
      const existingCfg =
        sourceConfigsById[editingSourceId] ??
        (grp ? synthesizeFallbackSourceConfig(grp) : null);
      baseConfigRef.current = existingCfg;

      if (existingCfg) {
        setSourceId(existingCfg.source_id);
        setDisplayName(existingCfg.display_name ?? "");
        setAutoStart(existingCfg.auto_start);

        setTransportKind(existingCfg.transport.kind);
        switch (existingCfg.transport.kind) {
          case "sim":
            setSimRateHz(existingCfg.transport.sample_rate_hz ?? 1_000_000);
            setSimChannels(existingCfg.transport.analog_channels);
            setSimGlitch(existingCfg.transport.enable_glitch);
            break;
          case "serial":
            setPortName(existingCfg.transport.port_name);
            setBaudRate(existingCfg.transport.baud_rate);
            setParity(existingCfg.transport.parity);
            setStopBits(existingCfg.transport.stop_bits);
            setFlowControl(existingCfg.transport.flow_control);
            break;
          case "udp": {
            setUdpBindAddr(existingCfg.transport.bind_addr);
            const parsed = parseUdpRemoteTxAddr(existingCfg.transport.remote_tx_addr, 9002);
            setUdpRemoteIp(parsed.ip);
            setUdpRemotePort(parsed.port);
            break;
          }
          case "tcp_client":
            setTcpAddr(existingCfg.transport.remote_addr);
            break;
          case "tcp_server":
            setTcpAddr(existingCfg.transport.listen_addr);
            break;
          case "raw_file_replay":
            setReplayPath(existingCfg.transport.file_path);
            setReplayFullSpeed(existingCfg.transport.full_speed_offline);
            setReplaySpeed(existingCfg.transport.playback_speed ?? 1.0);
            setReplayLoop(existingCfg.transport.loop_playback);
            break;
        }

        setClockMode(existingCfg.clock.mode);
        switch (existingCfg.clock.mode) {
          case "tier1_hardware_counter":
            setHwTickHz(existingCfg.clock.tick_hz ?? 1_000_000);
            break;
          case "tier2_software_pll":
            if (existingCfg.clock.nominal_sample_rate_hz == null) {
              setPllAutoEstimate(true);
            } else {
              setPllAutoEstimate(false);
              setNominalRateHz(existingCfg.clock.nominal_sample_rate_hz);
            }
            break;
          case "tier3_host_arrival":
            break;
          case "channel_counter": {
            const rawCh = existingCfg.clock.channel.trim();
            const slash = rawCh.indexOf("/");
            setSyncChannel(slash >= 0 ? rawCh.slice(slash + 1).trim() : rawCh);
            setNsPerCount(existingCfg.clock.ns_per_count ?? 1_000);
            const preset = rolloverModulusToPreset(existingCfg.clock.rollover_modulus);
            setRolloverPreset(preset);
            if (preset === "custom" && existingCfg.clock.rollover_modulus != null) {
              setCustomRolloverModulus(existingCfg.clock.rollover_modulus);
            }
            setSyncDriftComp(existingCfg.clock.drift_compensation ?? true);
            break;
          }
        }

        setProtocolCodec(existingCfg.protocol.codec);
        if (
          existingCfg.protocol.codec === "just_float" ||
          existingCfg.protocol.codec === "fire_water"
        ) {
          setChannelNamesText(existingCfg.protocol.channel_names.join(", "));
        }
      } else {
        setSourceId(editingSourceId);
        setDisplayName("");
      }
    } else {
      baseConfigRef.current = null;
      const existingIds = new Set(channelGroups.map((g) => g.source.source_id));
      let idx = channelGroups.length;
      while (existingIds.has(`src${idx}`)) {
        idx += 1;
      }
      setSourceId(`src${idx}`);
      setDisplayName(t("wizard.defaultName", { index: idx }));
      setTransportKind("sim");
      setClockMode("tier2_software_pll");
      setProtocolCodec("sim_preset");
      setAutoStart(true);
      setSimRateHz(1_000_000);
      setSimChannels(4);
      setSimGlitch(true);
      setUdpBindAddr("0.0.0.0:9001");
      setUdpRemoteIp("");
      setUdpRemotePort(9002);
      setTcpAddr("127.0.0.1:9100");
      setPllAutoEstimate(false);
      setNominalRateHz(500_000);
      setHwTickHz(1_000_000);
      setSyncChannel("iq");
      setNsPerCount(1_000);
      setRolloverPreset("auto");
      setCustomRolloverModulus(10_000);
      setSyncDriftComp(true);
      setChannelNamesText("va, vb, vc, iq");
    }

    bridgeClient
      .listSerialPorts()
      .then((ports) => {
        setSerialPorts(ports);
        if (editingSourceId === null && ports.length > 0) {
          setPortName(ports[0].port_name);
        }
      })
      .catch(() => {});
  }, [open, editingSourceId, channelGroups, sourceConfigsById, t]);

  const availableSyncChannels = useMemo<string[]>(() => {
    if (protocolCodec === "just_float" || protocolCodec === "fire_water") {
      const parsed = channelNamesText
        .split(",")
        .map((s) => s.trim())
        .filter((s) => s.length > 0);
      return parsed.length > 0 ? parsed : ["ch0", "ch1"];
    }
    if (protocolCodec === "fixed_schema_binary") {
      const baseProtocol = baseConfigRef.current?.protocol;
      if (baseProtocol?.codec === "fixed_schema_binary") {
        return baseProtocol.fields
          .filter((f) => !f.is_state_enum && f.scalar_type !== "bool")
          .map((f) => f.name);
      }
      return ["v_out", "i_out"];
    }
    const existingGroup =
      editingSourceId !== null
        ? channelGroups.find((g) => g.source.source_id === editingSourceId)
        : undefined;
    if (existingGroup) {
      const scalarPaths = existingGroup.channels
        .filter((c) => c.modality === "scalar")
        .map((c) => c.local_path);
      if (scalarPaths.length === Math.max(1, Math.min(16, simChannels))) {
        return scalarPaths;
      }
    }
    const count = Math.max(1, Math.min(16, simChannels));
    if (sourceId.trim() === "sim0" && count === 4) {
      return ["ia", "ib", "ic", "v_bus"];
    }
    return Array.from({ length: count }, (_, i) => `ch${i}`);
  }, [
    protocolCodec,
    channelNamesText,
    editingSourceId,
    channelGroups,
    simChannels,
    sourceId,
  ]);

  useEffect(() => {
    if (
      availableSyncChannels.length > 0 &&
      !availableSyncChannels.includes(syncChannel)
    ) {
      setSyncChannel(availableSyncChannels[availableSyncChannels.length - 1]);
    }
  }, [availableSyncChannels, syncChannel]);

  if (!open) return null;

  const handleTransportChange = (nextKind: WizardTransportKind) => {
    setTransportKind(nextKind);
    if (nextKind === "sim") {
      setPllAutoEstimate(false);
      setNominalRateHz(simRateHz);
    } else {
      if (protocolCodec === "sim_preset") {
        setProtocolCodec("just_float");
      }
      setPllAutoEstimate(true);
    }
  };

  const handleSubmit = async (ev: FormEvent) => {
    ev.preventDefault();
    const trimmedId = sourceId.trim();
    if (!trimmedId) {
      setErrorMsg(t("wizard.errSourceId"));
      return;
    }

    const baseCfg = baseConfigRef.current;
    const baseTransport = baseCfg?.transport;
    const baseClock = baseCfg?.clock;
    const baseProtocol = baseCfg?.protocol;

    let udpRemoteTxAddr: string | null = null;
    if (transportKind === "udp" && udpRemoteIp.trim().length > 0) {
      try {
        udpRemoteTxAddr = formatUdpRemoteTxAddr(udpRemoteIp, udpRemotePort);
      } catch {
        setErrorMsg(t("wizard.errUdpPort"));
        return;
      }
    }

    let transport: TransportConfigDto_Deserialize;
    switch (transportKind) {
      case "sim":
        transport = {
          kind: "sim",
          sample_rate_hz: simRateHz,
          analog_channels: simChannels,
          enable_glitch: simGlitch,
          glitch_period_samples:
            baseTransport?.kind === "sim" ? baseTransport.glitch_period_samples : 500_000,
        };
        break;
      case "serial":
        transport = {
          kind: "serial",
          port_name: portName,
          baud_rate: baudRate,
          data_bits: baseTransport?.kind === "serial" ? baseTransport.data_bits : 8,
          parity,
          stop_bits: stopBits,
          flow_control: flowControl,
          auto_reconnect: baseTransport?.kind === "serial" ? baseTransport.auto_reconnect : true,
        };
        break;
      case "udp":
        transport = {
          kind: "udp",
          bind_addr: udpBindAddr,
          multicast_group: baseTransport?.kind === "udp" ? baseTransport.multicast_group : null,
          recv_buffer_bytes:
            baseTransport?.kind === "udp" ? baseTransport.recv_buffer_bytes : 4 * 1024 * 1024,
          remote_tx_addr: udpRemoteTxAddr,
        };
        break;
      case "tcp_client":
        transport = {
          kind: "tcp_client",
          remote_addr: tcpAddr,
          auto_reconnect: baseTransport?.kind === "tcp_client" ? baseTransport.auto_reconnect : true,
          tcp_nodelay: baseTransport?.kind === "tcp_client" ? baseTransport.tcp_nodelay : true,
        };
        break;
      case "tcp_server":
        transport = {
          kind: "tcp_server",
          listen_addr: tcpAddr,
          tcp_nodelay: baseTransport?.kind === "tcp_server" ? baseTransport.tcp_nodelay : true,
        };
        break;
      case "raw_file_replay":
        transport = {
          kind: "raw_file_replay",
          file_path: replayPath,
          full_speed_offline: replayFullSpeed,
          playback_speed: replaySpeed,
          loop_playback: replayLoop,
          nominal_byte_rate:
            baseTransport?.kind === "raw_file_replay"
              ? baseTransport.nominal_byte_rate
              : 4_000_000,
        };
        break;
    }

    let clock: ClockConfigDto_Deserialize;
    switch (clockMode) {
      case "tier1_hardware_counter":
        clock = {
          mode: "tier1_hardware_counter",
          tick_hz: hwTickHz,
          counter_bits: baseClock?.mode === "tier1_hardware_counter" ? baseClock.counter_bits : 32,
        };
        break;
      case "tier2_software_pll":
        clock = {
          mode: "tier2_software_pll",
          nominal_sample_rate_hz: pllAutoEstimate ? null : nominalRateHz,
        };
        break;
      case "tier3_host_arrival":
        clock = { mode: "tier3_host_arrival" };
        break;
      case "channel_counter": {
        const targetCh = (syncChannel || availableSyncChannels[0] || "").trim();
        if (!targetCh) {
          setErrorMsg(t("wizard.errSyncChannel"));
          return;
        }
        if (!Number.isFinite(nsPerCount) || nsPerCount <= 0) {
          setErrorMsg(t("wizard.errNsPerCount"));
          return;
        }
        let rolloverModulus: number | null = null;
        if (rolloverPreset === "custom") {
          if (!Number.isFinite(customRolloverModulus) || customRolloverModulus <= 0) {
            setErrorMsg(t("wizard.errRolloverModulus"));
            return;
          }
          rolloverModulus = customRolloverModulus;
        } else if (rolloverPreset !== "auto") {
          rolloverModulus = Number(rolloverPreset);
        }
        clock = {
          mode: "channel_counter",
          channel: targetCh,
          ns_per_count: nsPerCount,
          rollover_modulus: rolloverModulus,
          drift_compensation: syncDriftComp,
        };
        break;
      }
    }

    const parsedNames = channelNamesText
      .split(",")
      .map((s) => s.trim())
      .filter((s) => s.length > 0);

    let protocol: ProtocolConfigDto_Deserialize;
    switch (protocolCodec) {
      case "sim_preset":
        protocol = { codec: "sim_preset" };
        break;
      case "just_float":
        protocol = {
          codec: "just_float",
          channel_names: parsedNames.length > 0 ? parsedNames : ["ch0", "ch1"],
        };
        break;
      case "fire_water":
        protocol = {
          codec: "fire_water",
          channel_names: parsedNames.length > 0 ? parsedNames : ["ch0", "ch1"],
        };
        break;
      case "fixed_schema_binary":
        protocol =
          baseProtocol?.codec === "fixed_schema_binary"
            ? baseProtocol
            : {
                codec: "fixed_schema_binary",
                header_magic: [0xaa, 0x55],
                has_hw_tick_first_col: true,
                fields: [
                  { name: "v_out", scalar_type: "f32", scale: 1, offset: 0, unit: "V", is_state_enum: false },
                  { name: "i_out", scalar_type: "f32", scale: 1, offset: 0, unit: "A", is_state_enum: false },
                  { name: "en_flag", scalar_type: "bool", scale: 1, offset: 0, unit: null, is_state_enum: false },
                  { name: "fsm", scalar_type: "u8", scale: 1, offset: 0, unit: null, is_state_enum: true },
                ],
              };
        break;
    }

    const payload: CreateSourceRequestDto_Deserialize = {
      source_id: trimmedId,
      display_name: displayName.trim() || null,
      transport,
      clock,
      protocol,
      auto_start: autoStart,
    };

    setSubmitting(true);
    setErrorMsg(null);
    try {
      if (isEditMode && editingSourceId !== null) {
        await updateSource(editingSourceId, payload);
      } else {
        await createSource(payload);
      }
      setOpen(false);
    } catch (err: unknown) {
      setErrorMsg(err instanceof Error ? err.message : String(err));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div
      className="modal-backdrop"
      role="dialog"
      aria-modal="true"
      aria-label={isEditMode ? t("wizard.editAriaLabel") : t("wizard.ariaLabel")}
    >
      <div className="modal-panel">
        <div className="modal-header">
          <h2>{isEditMode ? t("wizard.editTitle", { sourceId: editingSourceId }) : t("wizard.title")}</h2>
          <button type="button" className="icon-btn" onClick={() => setOpen(false)}>
            ×
          </button>
        </div>
        <form onSubmit={handleSubmit} className="wizard-form">
          <div className="wizard-grid">
            <label className="wizard-field">
              <span>{t("wizard.sourceId")}</span>
              <input
                type="text"
                value={sourceId}
                onChange={(e) => setSourceId(e.target.value)}
                required
                data-testid="wizard-source-id"
              />
            </label>
            <label className="wizard-field">
              <span>{t("wizard.displayName")}</span>
              <input
                type="text"
                value={displayName}
                onChange={(e) => setDisplayName(e.target.value)}
              />
            </label>
          </div>

          <div className="wizard-grid">
            <label className="wizard-field">
              <span>{t("wizard.transport")}</span>
              <select
                value={transportKind}
                onChange={(e) => handleTransportChange(e.target.value as WizardTransportKind)}
                data-testid="wizard-transport-kind"
              >
                <option value="sim">{t("wizard.transport.sim")}</option>
                <option value="serial">{t("wizard.transport.serial")}</option>
                <option value="udp">{t("wizard.transport.udp")}</option>
                <option value="tcp_client">{t("wizard.transport.tcpClient")}</option>
                <option value="tcp_server">{t("wizard.transport.tcpServer")}</option>
                <option value="raw_file_replay">{t("wizard.transport.replay")}</option>
              </select>
            </label>
            <label className="wizard-field">
              <span>{t("wizard.clock")}</span>
              <select
                value={clockMode}
                onChange={(e) => setClockMode(e.target.value as WizardClockMode)}
                data-testid="wizard-clock-mode"
              >
                <option value="tier2_software_pll">{t("wizard.clock.tier2")}</option>
                <option value="tier1_hardware_counter">{t("wizard.clock.tier1")}</option>
                <option value="tier3_host_arrival">{t("wizard.clock.tier3")}</option>
                <option value="channel_counter">{t("wizard.clock.channelCounter")}</option>
              </select>
            </label>
          </div>

          {transportKind === "sim" && (
            <div className="wizard-grid">
              <label className="wizard-field">
                <span>{t("wizard.simRate")}</span>
                <input
                  type="number"
                  min={1000}
                  max={10_000_000}
                  step={1000}
                  value={simRateHz}
                  onChange={(e) => setSimRateHz(Number(e.target.value) || 1_000_000)}
                />
              </label>
              <label className="wizard-field">
                <span>{t("wizard.simChannels")}</span>
                <input
                  type="number"
                  min={1}
                  max={8}
                  value={simChannels}
                  onChange={(e) => setSimChannels(Number(e.target.value) || 4)}
                />
              </label>
              <label className="wizard-check">
                <input
                  type="checkbox"
                  checked={simGlitch}
                  onChange={(e) => setSimGlitch(e.target.checked)}
                />
                <span>{t("wizard.simGlitch")}</span>
              </label>
            </div>
          )}

          {transportKind === "serial" && (
            <div className="wizard-grid">
              <label className="wizard-field">
                <span>{t("wizard.serialPort")}</span>
                <select value={portName} onChange={(e) => setPortName(e.target.value)}>
                  {serialPorts.length === 0 && <option value="COM3">COM3</option>}
                  {serialPorts.map((p) => (
                    <option key={p.port_name} value={p.port_name}>
                      {p.port_name}
                    </option>
                  ))}
                </select>
              </label>
              <label className="wizard-field">
                <span>{t("wizard.baudRate")}</span>
                <input
                  type="number"
                  value={baudRate}
                  onChange={(e) => setBaudRate(Number(e.target.value) || 115200)}
                />
              </label>
              <label className="wizard-field">
                <span>{t("wizard.parityFlow")}</span>
                <div className="wizard-inline-pair">
                  <select value={parity} onChange={(e) => setParity(e.target.value as SerialParityDto)}>
                    <option value="none">{t("wizard.parityNone")}</option>
                    <option value="odd">{t("wizard.parityOdd")}</option>
                    <option value="even">{t("wizard.parityEven")}</option>
                  </select>
                  <select
                    value={stopBits}
                    onChange={(e) => setStopBits(e.target.value as SerialStopBitsDto)}
                  >
                    <option value="one">{t("wizard.stop1")}</option>
                    <option value="two">{t("wizard.stop2")}</option>
                  </select>
                  <select
                    value={flowControl}
                    onChange={(e) => setFlowControl(e.target.value as SerialFlowControlDto)}
                  >
                    <option value="none">{t("wizard.flowNone")}</option>
                    <option value="hardware_rts_cts">RTS/CTS</option>
                    <option value="software_xon_xoff">XON/XOFF</option>
                  </select>
                </div>
              </label>
            </div>
          )}

          {transportKind === "udp" && (
            <div className="wizard-grid">
              <label className="wizard-field">
                <span>{t("wizard.udpBind")}</span>
                <input
                  type="text"
                  value={udpBindAddr}
                  onChange={(e) => setUdpBindAddr(e.target.value)}
                  data-testid="wizard-udp-bind"
                />
              </label>
              <label className="wizard-field">
                <span>{t("wizard.udpRemoteIp")}</span>
                <input
                  type="text"
                  value={udpRemoteIp}
                  placeholder={t("wizard.udpRemotePlaceholder")}
                  onChange={(e) => setUdpRemoteIp(e.target.value)}
                  data-testid="wizard-udp-remote-ip"
                />
              </label>
              <label className="wizard-field">
                <span>{t("wizard.udpRemotePort")}</span>
                <input
                  type="number"
                  min={1}
                  max={65535}
                  value={udpRemotePort}
                  onChange={(e) => setUdpRemotePort(Number(e.target.value) || 0)}
                  data-testid="wizard-udp-remote-port"
                />
              </label>
            </div>
          )}

          {(transportKind === "tcp_client" || transportKind === "tcp_server") && (
            <label className="wizard-field">
              <span>
                {transportKind === "tcp_client" ? t("wizard.tcpRemote") : t("wizard.tcpListen")}
              </span>
              <input type="text" value={tcpAddr} onChange={(e) => setTcpAddr(e.target.value)} />
            </label>
          )}

          {transportKind === "raw_file_replay" && (
            <div className="wizard-grid">
              <label className="wizard-field">
                <span>{t("wizard.replayPath")}</span>
                <input
                  type="text"
                  value={replayPath}
                  onChange={(e) => setReplayPath(e.target.value)}
                />
              </label>
              <label className="wizard-field">
                <span>{t("wizard.replaySpeed")}</span>
                <input
                  type="number"
                  min={0.1}
                  max={20}
                  step={0.1}
                  value={replaySpeed}
                  onChange={(e) => setReplaySpeed(Number(e.target.value) || 1)}
                />
              </label>
              <label className="wizard-check">
                <input
                  type="checkbox"
                  checked={replayFullSpeed}
                  onChange={(e) => setReplayFullSpeed(e.target.checked)}
                />
                <span>{t("wizard.replayFullSpeed")}</span>
              </label>
              <label className="wizard-check">
                <input
                  type="checkbox"
                  checked={replayLoop}
                  onChange={(e) => setReplayLoop(e.target.checked)}
                />
                <span>{t("wizard.replayLoop")}</span>
              </label>
            </div>
          )}

          <div className="wizard-grid">
            <label className="wizard-field">
              <span>{t("wizard.codec")}</span>
              <select
                value={protocolCodec}
                onChange={(e) => setProtocolCodec(e.target.value as WizardProtocolCodec)}
              >
                <option value="sim_preset">{t("wizard.codec.sim")}</option>
                <option value="just_float">{t("wizard.codec.justFloat")}</option>
                <option value="fire_water">{t("wizard.codec.fireWater")}</option>
                <option value="fixed_schema_binary">{t("wizard.codec.fixedSchema")}</option>
              </select>
            </label>

            {clockMode === "tier2_software_pll" && (
              <div className="wizard-field">
                <span>{t("wizard.pllRate")}</span>
                <input
                  type="number"
                  min={1}
                  disabled={pllAutoEstimate}
                  value={nominalRateHz}
                  onChange={(e) => setNominalRateHz(Number(e.target.value) || 500_000)}
                  data-testid="wizard-pll-rate"
                />
                <label className="wizard-check">
                  <input
                    type="checkbox"
                    checked={pllAutoEstimate}
                    onChange={(e) => setPllAutoEstimate(e.target.checked)}
                    data-testid="wizard-pll-auto"
                  />
                  <span>{t("wizard.pllAuto")}</span>
                </label>
              </div>
            )}

            {clockMode === "tier1_hardware_counter" && (
              <label className="wizard-field">
                <span>{t("wizard.hwTick")}</span>
                <input
                  type="number"
                  value={hwTickHz}
                  onChange={(e) => setHwTickHz(Number(e.target.value) || 1_000_000)}
                />
              </label>
            )}

            {clockMode === "channel_counter" && (
              <label className="wizard-field">
                <span>{t("wizard.syncChannel")}</span>
                <select
                  value={syncChannel}
                  onChange={(e) => setSyncChannel(e.target.value)}
                  data-testid="wizard-sync-channel"
                >
                  {availableSyncChannels.map((ch) => (
                    <option key={ch} value={ch}>
                      {ch}
                    </option>
                  ))}
                </select>
              </label>
            )}
          </div>

          {clockMode === "channel_counter" && (
            <div className="wizard-grid">
              <label className="wizard-field">
                <span>
                  {t("wizard.nsPerCount")}
                  {Number.isFinite(nsPerCount) && nsPerCount > 0
                    ? ` (≈ ${formatFrequencyHz(1e9 / nsPerCount)})`
                    : ""}
                </span>
                <input
                  type="number"
                  min={0.001}
                  step="any"
                  value={nsPerCount}
                  onChange={(e) => setNsPerCount(Number(e.target.value))}
                  data-testid="wizard-ns-per-count"
                />
              </label>
              <label className="wizard-field">
                <span>{t("wizard.rolloverMode")}</span>
                <select
                  value={rolloverPreset}
                  onChange={(e) => setRolloverPreset(e.target.value as RolloverPreset)}
                  data-testid="wizard-rollover-mode"
                >
                  <option value="auto">{t("wizard.rollover.auto")}</option>
                  <option value="256">{t("wizard.rollover.u8")}</option>
                  <option value="65536">{t("wizard.rollover.u16")}</option>
                  <option value="16777216">{t("wizard.rollover.u24")}</option>
                  <option value="4294967296">{t("wizard.rollover.u32")}</option>
                  <option value="custom">{t("wizard.rollover.custom")}</option>
                </select>
              </label>
              {rolloverPreset === "custom" && (
                <label className="wizard-field">
                  <span>{t("wizard.rolloverModulus")}</span>
                  <input
                    type="number"
                    min={1}
                    step="any"
                    value={customRolloverModulus}
                    onChange={(e) => setCustomRolloverModulus(Number(e.target.value))}
                    data-testid="wizard-rollover-modulus"
                  />
                </label>
              )}
              <label className="wizard-check">
                <input
                  type="checkbox"
                  checked={syncDriftComp}
                  onChange={(e) => setSyncDriftComp(e.target.checked)}
                  data-testid="wizard-sync-drift"
                />
                <span>{t("wizard.syncDriftComp")}</span>
              </label>
            </div>
          )}

          {(protocolCodec === "just_float" || protocolCodec === "fire_water") && (
            <label className="wizard-field">
              <span>{t("wizard.channelNames")}</span>
              <input
                type="text"
                value={channelNamesText}
                onChange={(e) => setChannelNamesText(e.target.value)}
              />
            </label>
          )}

          <label className="wizard-check">
            <input
              type="checkbox"
              checked={autoStart}
              onChange={(e) => setAutoStart(e.target.checked)}
            />
            <span>{isEditMode ? t("wizard.autoStartEdit") : t("wizard.autoStart")}</span>
          </label>

          {errorMsg && <div className="wizard-error">{errorMsg}</div>}

          <div className="modal-actions">
            <button type="button" className="scope-btn" onClick={() => setOpen(false)}>
              {t("wizard.cancel")}
            </button>
            <button
              type="submit"
              className="scope-btn scope-btn-accent"
              disabled={submitting}
              data-testid="wizard-submit-btn"
            >
              {submitting
                ? isEditMode
                  ? t("wizard.saving")
                  : t("wizard.creating")
                : isEditMode
                  ? t("wizard.save")
                  : t("wizard.create")}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
