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
