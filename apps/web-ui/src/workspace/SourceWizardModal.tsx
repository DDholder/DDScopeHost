import { useEffect, useState, type FormEvent } from "react";
import type {
  ClockConfigDto_Deserialize,
  ProtocolConfigDto_Deserialize,
  SerialFlowControlDto,
  SerialParityDto,
  SerialPortInfoDto_Serialize,
  SerialStopBitsDto,
  TransportConfigDto_Deserialize,
} from "../bridge/bindings.ts";
import { bridgeClient } from "../bridge/control-client.ts";
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
  | "tier3_host_arrival";

type WizardProtocolCodec =
  | "sim_preset"
  | "just_float"
  | "fire_water"
  | "fixed_schema_binary";

export function SourceWizardModal() {
  const { t } = useI18n();
  const open = useWorkspaceStore((s) => s.sourceWizardOpen);
  const setOpen = useWorkspaceStore((s) => s.setSourceWizardOpen);
  const createSource = useWorkspaceStore((s) => s.createSource);
  const channelGroups = useWorkspaceStore((s) => s.channelGroups);

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
  const [channelNamesText, setChannelNamesText] = useState("va, vb, vc, iq");

  const [submitting, setSubmitting] = useState(false);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    const existingIds = new Set(channelGroups.map((g) => g.source.source_id));
    let idx = channelGroups.length;
    while (existingIds.has(`src${idx}`)) {
      idx += 1;
    }
    setSourceId(`src${idx}`);
    setDisplayName(t("wizard.defaultName", { index: idx }));
    setErrorMsg(null);
    bridgeClient
      .listSerialPorts()
      .then((ports) => {
        setSerialPorts(ports);
        if (ports.length > 0) {
          setPortName(ports[0].port_name);
        }
      })
      .catch(() => {});
  }, [open, channelGroups, t]);

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

    let transport: TransportConfigDto_Deserialize;
    switch (transportKind) {
      case "sim":
        transport = {
          kind: "sim",
          sample_rate_hz: simRateHz,
          analog_channels: simChannels,
          enable_glitch: simGlitch,
          glitch_period_samples: 500_000,
        };
        break;
      case "serial":
        transport = {
          kind: "serial",
          port_name: portName,
          baud_rate: baudRate,
          data_bits: 8,
          parity,
          stop_bits: stopBits,
          flow_control: flowControl,
          auto_reconnect: true,
        };
        break;
      case "udp":
        transport = {
          kind: "udp",
          bind_addr: udpBindAddr,
          multicast_group: null,
          recv_buffer_bytes: 4 * 1024 * 1024,
          remote_tx_addr: null,
        };
        break;
      case "tcp_client":
        transport = {
          kind: "tcp_client",
          remote_addr: tcpAddr,
          auto_reconnect: true,
          tcp_nodelay: true,
        };
        break;
      case "tcp_server":
        transport = {
          kind: "tcp_server",
          listen_addr: tcpAddr,
          tcp_nodelay: true,
        };
        break;
      case "raw_file_replay":
        transport = {
          kind: "raw_file_replay",
          file_path: replayPath,
          full_speed_offline: replayFullSpeed,
          playback_speed: replaySpeed,
          loop_playback: replayLoop,
          nominal_byte_rate: 4_000_000,
        };
        break;
    }

    let clock: ClockConfigDto_Deserialize;
    switch (clockMode) {
      case "tier1_hardware_counter":
        clock = {
          mode: "tier1_hardware_counter",
          tick_hz: hwTickHz,
          counter_bits: 32,
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
        protocol = {
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

    setSubmitting(true);
    setErrorMsg(null);
    try {
      await createSource({
        source_id: trimmedId,
        display_name: displayName.trim() || null,
        transport,
        clock,
        protocol,
        auto_start: autoStart,
      });
      setOpen(false);
    } catch (err: unknown) {
      setErrorMsg(err instanceof Error ? err.message : String(err));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div className="modal-backdrop" role="dialog" aria-modal="true" aria-label={t("wizard.ariaLabel")}>
      <div className="modal-panel">
        <div className="modal-header">
          <h2>{t("wizard.title")}</h2>
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
              >
                <option value="tier2_software_pll">{t("wizard.clock.tier2")}</option>
                <option value="tier1_hardware_counter">{t("wizard.clock.tier1")}</option>
                <option value="tier3_host_arrival">{t("wizard.clock.tier3")}</option>
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
            <label className="wizard-field">
              <span>{t("wizard.udpBind")}</span>
              <input
                type="text"
                value={udpBindAddr}
                onChange={(e) => setUdpBindAddr(e.target.value)}
              />
            </label>
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
          </div>

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
            <span>{t("wizard.autoStart")}</span>
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
              {submitting ? t("wizard.creating") : t("wizard.create")}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
