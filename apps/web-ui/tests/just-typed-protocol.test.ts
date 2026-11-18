import test from "node:test";
import assert from "node:assert/strict";
import { parseDdsfFrame } from "../src/bridge/ddsf-parser.ts";
import {
  BridgeClient,
  HighFreqTopicBus,
  normalizeSourceConfigToSerialize,
} from "../src/bridge/index.ts";
import { MockSimulationCore } from "../src/bridge/mock-ddsf-core.ts";
import {
  SOURCES_CONFIG_SCHEMA_V1,
  parseSourcesConfigJson,
  serializeSourcesConfig,
  synthesizeFallbackSourceConfig,
} from "../src/workspace/config-io.ts";
import { createWorkspaceStore } from "../src/workspace/store.ts";

test("just_typed config normalization, validation guards, fallback synthesis, and JSON roundtrip", () => {
  const normalized = normalizeSourceConfigToSerialize({
    source_id: "jt_mcu",
    display_name: "MCU JustTyped",
    transport: {
      kind: "serial",
      port_name: "COM5",
      baud_rate: 921_600,
      data_bits: 8,
      parity: "none",
      stop_bits: "one",
      flow_control: "none",
      auto_reconnect: true,
    },
    clock: {
      mode: "tier2_software_pll",
      nominal_sample_rate_hz: 50_000,
    },
    protocol: {
      codec: "just_typed",
      channels: [
        { name: " v_dc " },
        { name: "err_code", data_type: "int" },
        { name: "flags_u32", data_type: "uint" },
      ],
    },
    auto_start: false,
  });

  assert.deepEqual(normalized.protocol, {
    codec: "just_typed",
    channels: [
      { name: "v_dc", data_type: "float" },
      { name: "err_code", data_type: "int" },
      { name: "flags_u32", data_type: "uint" },
    ],
  });

  const json = serializeSourcesConfig([normalized]);
  const parsed = parseSourcesConfigJson(json);
  assert.equal(parsed.schema, SOURCES_CONFIG_SCHEMA_V1);
  assert.equal(parsed.sources.length, 1);
  assert.deepEqual(parsed.sources[0].protocol, normalized.protocol);

  // Guard empty channels, blank name, duplicate name, and invalid data_type
  const wrap = (channels: unknown[]) =>
    JSON.stringify({
      schema: SOURCES_CONFIG_SCHEMA_V1,
      exportedAt: new Date().toISOString(),
      sources: [
        {
          ...normalized,
          protocol: { codec: "just_typed", channels },
        },
      ],
    });

  assert.throws(() => parseSourcesConfigJson(wrap([])), /non-empty array/);
  assert.throws(
    () => parseSourcesConfigJson(wrap([{ name: "  ", data_type: "int" }])),
    /non-empty string/,
  );
  assert.throws(
    () =>
      parseSourcesConfigJson(
        wrap([
          { name: "dup", data_type: "float" },
          { name: "dup", data_type: "uint" },
        ]),
      ),
    /duplicate name 'dup'/,
  );
  assert.throws(
    () => parseSourcesConfigJson(wrap([{ name: "x", data_type: "bool" }])),
    /data_type 'bool' is invalid/,
  );

  // synthesizeFallbackSourceConfig preserves just_typed when i32/u32 scalar channels exist
  const fallback = synthesizeFallbackSourceConfig({
    source: {
      source_id: "udp_fb",
      display_name: "UDP",
      transport_kind: "udp",
      state: "stopped",
      raw_recording_path: null,
    },
    channels: [
      {
        channel_id: 1,
        source_id: "udp_fb",
        local_path: "va",
        full_path: "udp_fb/va",
        display_name: "va",
        unit: null,
        modality: "scalar",
        scalar_type: "f32",
        state_labels: [],
      },
      {
        channel_id: 2,
        source_id: "udp_fb",
        local_path: "cnt",
        full_path: "udp_fb/cnt",
        display_name: "cnt",
        unit: null,
        modality: "scalar",
        scalar_type: "i32",
        state_labels: [],
      },
      {
        channel_id: 3,
        source_id: "udp_fb",
        local_path: "bits",
        full_path: "udp_fb/bits",
        display_name: "bits",
        unit: null,
        modality: "scalar",
        scalar_type: "u32",
        state_labels: [],
      },
    ],
  });
  assert.deepEqual(fallback.protocol, {
    codec: "just_typed",
    channels: [
      { name: "va", data_type: "float" },
      { name: "cnt", data_type: "int" },
      { name: "bits", data_type: "uint" },
    ],
  });
});

test("MockSimulationCore creates float/int/uint just_typed channels, emits integer values, and supports ChannelCounter", () => {
  const core = new MockSimulationCore();

  core.createSource({
    source_id: "jt0",
    display_name: "JustTyped Mock",
    transport: {
      kind: "udp",
      bind_addr: "0.0.0.0:9010",
      multicast_group: null,
      recv_buffer_bytes: 1_048_576,
      remote_tx_addr: null,
    },
    clock: {
      mode: "channel_counter",
      channel: "tick_u32",
      ns_per_count: 1_000,
      rollover_modulus: 65_536,
      drift_compensation: true,
    },
    protocol: {
      codec: "just_typed",
      channels: [
        { name: "va_f32", data_type: "float" },
        { name: "iq_i32", data_type: "int" },
        { name: "tick_u32", data_type: "uint" },
      ],
    },
    auto_start: true,
  });

  const tree = core.getChannelTree();
  const grp = tree.sources.find((s) => s.source.source_id === "jt0");
  assert.ok(grp);
  assert.equal(grp.channels.length, 3);
  assert.equal(grp.channels[0].scalar_type, "f32");
  assert.equal(grp.channels[1].scalar_type, "i32");
  assert.equal(grp.channels[2].scalar_type, "u32");

  const chF = grp.channels[0].channel_id;
  const chI = grp.channels[1].channel_id;
  const chU = grp.channels[2].channel_id;

  const stepped = core.step(10, 1000);
  const frame = parseDdsfFrame(stepped.frameBuffer);
  const snapMap = new Map(frame.snapshots.map((s) => [s.channelNumId, s.valF64]));

  assert.equal(Number.isFinite(snapMap.get(chF)!), true);
  assert.equal(Number.isInteger(snapMap.get(chI)!), true);
  assert.equal(Number.isInteger(snapMap.get(chU)!), true);
  assert.ok(snapMap.get(chU)! >= 0);

  // Reject missing/invalid ChannelCounter target and duplicate channel names
  assert.throws(() =>
    core.createSource({
      source_id: "jt_bad",
      transport: {
        kind: "udp",
        bind_addr: "0.0.0.0:9011",
        multicast_group: null,
        recv_buffer_bytes: 1_048_576,
        remote_tx_addr: null,
      },
      clock: {
        mode: "channel_counter",
        channel: "missing_ch",
        ns_per_count: 1_000,
      },
      protocol: {
        codec: "just_typed",
        channels: [{ name: "only_ch", data_type: "uint" }],
      },
      auto_start: false,
    }),
  );
});

test("WorkspaceStore createSource, updateSource, and export/import project preserve just_typed channels", async () => {
  const bus = new HighFreqTopicBus();
  const client = new BridgeClient(bus);
  const store = createWorkspaceStore(client);

  try {
    await store.getState().initWorkspace();

    await store.getState().createSource({
      source_id: "jt_drv",
      display_name: "Drive Telemetry",
      transport: {
        kind: "udp",
        bind_addr: "0.0.0.0:9300",
        multicast_group: null,
        recv_buffer_bytes: 1_048_576,
        remote_tx_addr: null,
      },
      clock: {
        mode: "tier2_software_pll",
        nominal_sample_rate_hz: 20_000,
      },
      protocol: {
        codec: "just_typed",
        channels: [
          { name: "bus_v", data_type: "float" },
          { name: "pos_cnt", data_type: "int" },
          { name: "status_bits", data_type: "uint" },
        ],
      },
      auto_start: false,
    });

    let channels = Object.values(store.getState().channelsById).filter(
      (c) => c.source_id === "jt_drv",
    );
    assert.equal(channels.length, 3);
    assert.equal(channels.find((c) => c.local_path === "bus_v")?.scalar_type, "f32");
    assert.equal(channels.find((c) => c.local_path === "pos_cnt")?.scalar_type, "i32");
    assert.equal(channels.find((c) => c.local_path === "status_bits")?.scalar_type, "u32");

    // Bind a bit_parser widget to status_bits and verify updateSource + project roundtrip
    const cid = store.getState().activeCanvasId!;
    const statusCh = channels.find((c) => c.local_path === "status_bits")!;
    const wid = store.getState().addWidgetToCanvas(cid, "bit_parser", {
      channelId: statusCh.channel_id,
    });

    await store.getState().updateSource("jt_drv", {
      source_id: "jt_drv",
      display_name: "Drive Telemetry Updated",
      transport: {
        kind: "udp",
        bind_addr: "0.0.0.0:9301",
        multicast_group: null,
        recv_buffer_bytes: 1_048_576,
        remote_tx_addr: null,
      },
      clock: {
        mode: "tier2_software_pll",
        nominal_sample_rate_hz: 40_000,
      },
      protocol: {
        codec: "just_typed",
        channels: [
          { name: "bus_v", data_type: "float" },
          { name: "pos_cnt", data_type: "int" },
          { name: "status_bits", data_type: "uint" },
          { name: "fault_code", data_type: "uint" },
        ],
      },
      auto_start: false,
    });

    channels = Object.values(store.getState().channelsById).filter(
      (c) => c.source_id === "jt_drv",
    );
    assert.equal(channels.length, 4);
    assert.equal(channels.find((c) => c.local_path === "fault_code")?.scalar_type, "u32");

    const projectJson = store.getState().exportProjectConfig();
    await store.getState().removeSource("jt_drv");
    assert.equal(
      Object.values(store.getState().channelsById).some((c) => c.source_id === "jt_drv"),
      false,
    );

    await store.getState().importProjectConfig(projectJson);
    const restoredSrc = store.getState().sourceConfigsById["jt_drv"];
    assert.ok(restoredSrc);
    assert.deepEqual(restoredSrc.protocol, {
      codec: "just_typed",
      channels: [
        { name: "bus_v", data_type: "float" },
        { name: "pos_cnt", data_type: "int" },
        { name: "status_bits", data_type: "uint" },
        { name: "fault_code", data_type: "uint" },
      ],
    });

    const restoredWidget = store
      .getState()
      .canvases[cid].widgets.find((w) => w.id === wid);
    const restoredStatusCh = Object.values(store.getState().channelsById).find(
      (c) => c.full_path === "jt_drv/status_bits",
    );
    assert.ok(restoredWidget && restoredStatusCh);
    assert.equal(restoredWidget.primaryBinding?.channelId, restoredStatusCh.channel_id);
  } finally {
    client.stopDataPlane();
  }
});
