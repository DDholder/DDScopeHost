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
  const [nominalRateHz, setNominalRateHz] = useState(500_000);
  const [hwTickHz, setHwTickHz] = useState(1_000_000);
  const [channelNamesText, setChannelNamesText] = useState("va, vb, vc, iq");

  const [submitting, setSubmitting] = useState(false);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    const count = channelGroups.length;
    setSourceId(`src${count}`);
    setDisplayName(`Source #${count}`);
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
  }, [open, channelGroups.length]);

  if (!open) return null;

  const handleSubmit = async (ev: FormEvent) => {
    ev.preventDefault();
    const trimmedId = sourceId.trim();
    if (!trimmedId) {
      setErrorMsg("Source ID is required");
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
          nominal_sample_rate_hz: nominalRateHz,
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
    <div className="modal-backdrop" role="dialog" aria-modal="true" aria-label="Create Data Source">
      <div className="modal-panel">
        <div className="modal-header">
          <h2>Add Multi-Modal Data Source</h2>
          <button type="button" className="icon-btn" onClick={() => setOpen(false)}>
            ×
          </button>
        </div>
        <form onSubmit={handleSubmit} className="wizard-form">
          <div className="wizard-grid">
            <label className="wizard-field">
              <span>Source ID</span>
              <input
                type="text"
                value={sourceId}
                onChange={(e) => setSourceId(e.target.value)}
                required
                data-testid="wizard-source-id"
              />
            </label>
            <label className="wizard-field">
              <span>Display Name</span>
              <input
                type="text"
                value={displayName}
                onChange={(e) => setDisplayName(e.target.value)}
              />
            </label>
          </div>

          <div className="wizard-grid">
            <label className="wizard-field">
              <span>Transport</span>
              <select
                value={transportKind}
                onChange={(e) => setTransportKind(e.target.value as WizardTransportKind)}
                data-testid="wizard-transport-kind"
              >
                <option value="sim">SimSignalTransport (High-Speed Generator)</option>
                <option value="serial">Serial UART / USB-CDC (up to 12 Mbps)</option>
                <option value="udp">UDP Socket (Unicast / Multicast)</option>
                <option value="tcp_client">TCP Client (Auto-Reconnect)</option>
                <option value="tcp_server">TCP Server</option>
                <option value="raw_file_replay">RawFileReplay (.bin / .ddraw)</option>
              </select>
            </label>
            <label className="wizard-field">
              <span>Clock Engine</span>
              <select
                value={clockMode}
                onChange={(e) => setClockMode(e.target.value as WizardClockMode)}
              >
                <option value="tier2_software_pll">Tier 2: 2nd-Order Software PLL</option>
                <option value="tier1_hardware_counter">Tier 1: Hardware Tick Drift Anchor</option>
                <option value="tier3_host_arrival">Tier 3: Host Monotonic Clock</option>
              </select>
            </label>
          </div>

          {transportKind === "sim" && (
            <div className="wizard-grid">
              <label className="wizard-field">
                <span>Sample Rate (Hz)</span>
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
                <span>Analog Channels</span>
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
                <span>Inject Single-Sample Glitch</span>
              </label>
            </div>
          )}

          {transportKind === "serial" && (
            <div className="wizard-grid">
              <label className="wizard-field">
                <span>Serial Port</span>
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
                <span>Baud Rate</span>
                <input
                  type="number"
                  value={baudRate}
                  onChange={(e) => setBaudRate(Number(e.target.value) || 115200)}
                />
              </label>
              <label className="wizard-field">
                <span>Parity / Flow</span>
                <div className="wizard-inline-pair">
                  <select value={parity} onChange={(e) => setParity(e.target.value as SerialParityDto)}>
                    <option value="none">None</option>
                    <option value="odd">Odd</option>
                    <option value="even">Even</option>
                  </select>
                  <select
                    value={stopBits}
                    onChange={(e) => setStopBits(e.target.value as SerialStopBitsDto)}
                  >
                    <option value="one">1 Stop</option>
                    <option value="two">2 Stop</option>
                  </select>
                  <select
                    value={flowControl}
                    onChange={(e) => setFlowControl(e.target.value as SerialFlowControlDto)}
                  >
                    <option value="none">No Flow</option>
                    <option value="hardware_rts_cts">RTS/CTS</option>
                    <option value="software_xon_xoff">XON/XOFF</option>
                  </select>
                </div>
              </label>
            </div>
          )}

          {transportKind === "udp" && (
            <label className="wizard-field">
              <span>UDP Bind Address</span>
              <input
                type="text"
                value={udpBindAddr}
                onChange={(e) => setUdpBindAddr(e.target.value)}
              />
            </label>
          )}

          {(transportKind === "tcp_client" || transportKind === "tcp_server") && (
            <label className="wizard-field">
              <span>{transportKind === "tcp_client" ? "Remote Host:Port" : "Listen Bind:Port"}</span>
              <input type="text" value={tcpAddr} onChange={(e) => setTcpAddr(e.target.value)} />
            </label>
          )}

          {transportKind === "raw_file_replay" && (
            <div className="wizard-grid">
              <label className="wizard-field">
                <span>Capture File Path</span>
                <input
                  type="text"
                  value={replayPath}
                  onChange={(e) => setReplayPath(e.target.value)}
                />
              </label>
              <label className="wizard-field">
                <span>Playback Speed</span>
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
                <span>Offline Full-Speed Mmap</span>
              </label>
              <label className="wizard-check">
                <input
                  type="checkbox"
                  checked={replayLoop}
                  onChange={(e) => setReplayLoop(e.target.checked)}
                />
                <span>Loop File</span>
              </label>
            </div>
          )}

          <div className="wizard-grid">
            <label className="wizard-field">
              <span>Protocol Codec</span>
              <select
                value={protocolCodec}
                onChange={(e) => setProtocolCodec(e.target.value as WizardProtocolCodec)}
              >
                <option value="sim_preset">SimPreset (Analog + Logic + State)</option>
                <option value="just_float">JustFloat (VOFA+ 00 00 80 7F)</option>
                <option value="fire_water">FireWater (SIMD CSV)</option>
                <option value="fixed_schema_binary">FixedSchemaBinary (Mixed Struct)</option>
              </select>
            </label>

            {clockMode === "tier2_software_pll" && (
              <label className="wizard-field">
                <span>PLL Nominal Sample Rate (Hz)</span>
                <input
                  type="number"
                  value={nominalRateHz}
                  onChange={(e) => setNominalRateHz(Number(e.target.value) || 500_000)}
                />
              </label>
            )}

            {clockMode === "tier1_hardware_counter" && (
              <label className="wizard-field">
                <span>Hardware Tick Clock (Hz)</span>
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
              <span>Channel Names (comma separated)</span>
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
            <span>Auto-Start Acquisition Immediately</span>
          </label>

          {errorMsg && <div className="wizard-error">{errorMsg}</div>}

          <div className="modal-actions">
            <button type="button" className="scope-btn" onClick={() => setOpen(false)}>
              Cancel
            </button>
            <button
              type="submit"
              className="scope-btn scope-btn-accent"
              disabled={submitting}
              data-testid="wizard-submit-btn"
            >
              {submitting ? "Creating..." : "Create Source"}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
