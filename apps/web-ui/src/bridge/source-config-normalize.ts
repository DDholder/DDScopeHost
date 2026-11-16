import type {
  ClockConfigDto_Serialize,
  CreateSourceRequestDto_Deserialize,
  CreateSourceRequestDto_Serialize,
  ProtocolConfigDto_Serialize,
  TransportConfigDto_Serialize,
} from "./bindings.ts";

/**
 * Deep-clones a source creation payload and coerces optional fields to explicit
 * `null` values matching serialized `CreateSourceRequestDto_Serialize` snapshots.
 */
export function normalizeSourceConfigToSerialize(
  req: CreateSourceRequestDto_Deserialize,
): CreateSourceRequestDto_Serialize {
  let transport: TransportConfigDto_Serialize;
  switch (req.transport.kind) {
    case "udp":
      transport = {
        ...req.transport,
        multicast_group: req.transport.multicast_group ?? null,
        remote_tx_addr: req.transport.remote_tx_addr ?? null,
      };
      break;
    case "raw_file_replay":
      transport = {
        ...req.transport,
        nominal_byte_rate: req.transport.nominal_byte_rate ?? null,
      };
      break;
    default:
      transport = { ...req.transport };
      break;
  }

  const clock: ClockConfigDto_Serialize =
    req.clock.mode === "tier2_software_pll"
      ? {
          mode: "tier2_software_pll",
          nominal_sample_rate_hz: req.clock.nominal_sample_rate_hz ?? null,
        }
      : { ...req.clock };

  let protocol: ProtocolConfigDto_Serialize;
  switch (req.protocol.codec) {
    case "just_float":
      protocol = {
        codec: "just_float",
        channel_names: [...req.protocol.channel_names],
      };
      break;
    case "fire_water":
      protocol = {
        codec: "fire_water",
        channel_names: [...req.protocol.channel_names],
      };
      break;
    case "fixed_schema_binary":
      protocol = {
        codec: "fixed_schema_binary",
        header_magic: [...req.protocol.header_magic],
        fields: req.protocol.fields.map((f) => ({
          ...f,
          unit: f.unit ?? null,
          scale: f.scale ?? null,
          offset: f.offset ?? null,
        })),
        has_hw_tick_first_col: req.protocol.has_hw_tick_first_col,
      };
      break;
    default:
      protocol = { codec: "sim_preset" };
      break;
  }

  return {
    source_id: req.source_id,
    display_name: req.display_name ?? null,
    transport,
    clock,
    protocol,
    auto_start: req.auto_start,
  };
}

function isValidPort(port: number): boolean {
  return Number.isInteger(port) && port >= 1 && port <= 65_535;
}

/**
 * Checks whether `raw` looks like a valid `host:port` or `[ipv6]:port` socket target.
 */
export function isValidUdpSocketAddr(raw: string): boolean {
  const trimmed = raw.trim();
  if (!trimmed) return false;
  const bracketed = /^\[([^\]]+)\]:(\d+)$/.exec(trimmed);
  if (bracketed) {
    return bracketed[1].trim().length > 0 && isValidPort(Number(bracketed[2]));
  }
  const colonCount = (trimmed.match(/:/g) ?? []).length;
  if (colonCount !== 1) return false;
  const hostPort = /^([^:]+):(\d+)$/.exec(trimmed);
  return Boolean(hostPort && hostPort[1].trim().length > 0 && isValidPort(Number(hostPort[2])));
}

/**
 * Formats an optional UDP target IP + port into a validated `SocketAddr` string, or `null` if blank.
 */
export function formatUdpRemoteTxAddr(ipInput: string, portInput: number | string): string | null {
  const trimmed = ipInput.trim();
  if (!trimmed) return null;

  const bracketed = /^\[([^\]]+)\]:(\d+)$/.exec(trimmed);
  if (bracketed) {
    const port = Number(bracketed[2]);
    if (!isValidPort(port)) {
      throw new Error("UDP target port must be between 1 and 65535.");
    }
    return trimmed;
  }

  const colonCount = (trimmed.match(/:/g) ?? []).length;
  if (colonCount === 1) {
    const hostPort = /^([^:]+):(\d+)$/.exec(trimmed);
    if (hostPort) {
      const port = Number(hostPort[2]);
      if (!isValidPort(port)) {
        throw new Error("UDP target port must be between 1 and 65535.");
      }
      return trimmed;
    }
    throw new Error(`Invalid UDP target address '${trimmed}': expected IP or host:port.`);
  }

  const portNum =
    typeof portInput === "number" ? portInput : Number.parseInt(String(portInput).trim(), 10);
  if (!isValidPort(portNum)) {
    throw new Error("UDP target port must be between 1 and 65535.");
  }

  if (colonCount > 1 && !trimmed.startsWith("[")) {
    return `[${trimmed}]:${portNum}`;
  }
  const cleanHost = trimmed.replace(/^\[|\]$/g, "");
  return `${cleanHost}:${portNum}`;
}

/**
 * Splits a `SocketAddr` string (`127.0.0.1:9002` or `[::1]:9002`) into separate IP & port fields.
 */
export function parseUdpRemoteTxAddr(
  remoteTxAddr: string | null | undefined,
  defaultPort = 9002,
): { ip: string; port: number } {
  const trimmed = remoteTxAddr?.trim() ?? "";
  if (!trimmed) {
    return { ip: "", port: defaultPort };
  }

  const bracketed = /^\[([^\]]+)\]:(\d+)$/.exec(trimmed);
  if (bracketed) {
    const port = Number(bracketed[2]);
    return {
      ip: bracketed[1],
      port: isValidPort(port) ? port : defaultPort,
    };
  }

  const colonCount = (trimmed.match(/:/g) ?? []).length;
  if (colonCount === 1) {
    const hostPort = /^([^:]+):(\d+)$/.exec(trimmed);
    if (hostPort) {
      const port = Number(hostPort[2]);
      return {
        ip: hostPort[1],
        port: isValidPort(port) ? port : defaultPort,
      };
    }
  }

  return {
    ip: trimmed.replace(/^\[|\]$/g, ""),
    port: defaultPort,
  };
}
