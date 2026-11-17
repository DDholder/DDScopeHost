import test from "node:test";
import assert from "node:assert/strict";
import { BridgeClient, HighFreqTopicBus } from "../src/bridge/index.ts";
import { parseDdsfFrame } from "../src/bridge/ddsf-parser.ts";
import { DdsfBlockKind } from "../src/bridge/ddsf-spec.ts";
import { MockSimulationCore } from "../src/bridge/mock-ddsf-core.ts";
import {
  MockSinglePhasePowerTracker,
  VIRTUAL_SOURCE_ID,
  compileVirtualFormula,
  normalizePowerFlags,
  sortVirtualNodesTopologically,
} from "../src/bridge/virtual-eval.ts";
import {
  VIRTUAL_CHANNELS_CONFIG_SCHEMA_V1,
  parseBoardConfigJson,
  parseCommandsConfigJson,
  parseProjectConfigJson,
  parseSourcesConfigJson,
  parseVirtualChannelsConfigJson,
} from "../src/workspace/config-io.ts";
import { createWorkspaceStore } from "../src/workspace/store.ts";

test("compileVirtualFormula, topological cycle guard, and MockSinglePhasePowerTracker IEEE 1459 accuracy", () => {
  const compiled = compileVirtualFormula("hypot([sim0/ia], [sim0/ib]) + (t > 0 ? 1 : 0)");
  assert.deepEqual(compiled.inputChannels, ["sim0/ia", "sim0/ib"]);
  assert.ok(Math.abs(compiled.evalAt(0.5, [3, 4]) - 6) < 1e-9);
  assert.ok(Math.abs(compiled.evalAt(0, [3, 4]) - 5) < 1e-9);

  // Cycle guard on chained virtual definitions
  assert.throws(
    () =>
      sortVirtualNodesTopologically([
        {
          node_id: "v_a",
          name: "A",
          enabled: true,
          operator: {
            kind: "formula",
            expression: "[virtual/out_b] + 1",
            output_path: "out_a",
            unit: null,
            output_digital: false,
          },
        },
        {
          node_id: "v_b",
          name: "B",
          enabled: true,
          operator: {
            kind: "formula",
            expression: "[virtual/out_a] * 2",
            output_path: "out_b",
            unit: null,
            output_digital: false,
          },
        },
      ]),
    /Cycle detected/i,
  );

  // Inductive 50Hz 220Vrms / 10Arms (+30 deg lag)
  const indTracker = new MockSinglePhasePowerTracker({
    kind: "single_phase_power",
    voltage_channel: "ac/u",
    current_channel: "ac/i",
    output_prefix: "ind",
    voltage_scale: 1,
    voltage_offset: 0,
    current_scale: 1,
    current_offset: 0,
    nominal_freq_hz: 50,
    zero_cross_hysteresis_v: 1,
    min_freq_hz: 5,
    fallback_window_ms: 100,
    outputs: normalizePowerFlags(),
  });
  const omega = Math.PI * 2 * 50;
  const uPk = 220 * Math.SQRT2;
  const iPk = 10 * Math.SQRT2;
  const phi = Math.PI / 6;
  const dtNs = 100_000; // 10 kHz
  for (let k = 0; k < 2000; k += 1) {
    const tNs = k * dtNs;
    const tSec = tNs * 1e-9;
    indTracker.processSample(
      tNs,
      uPk * Math.sin(omega * tSec),
      iPk * Math.sin(omega * tSec - phi),
    );
  }
  const latestInd = indTracker.history.at(-1)!;
  assert.ok(latestInd);
  assert.ok(Math.abs(latestInd.freq - 50) < 0.25);
  assert.ok(Math.abs(latestInd.u_rms - 220) < 1.0);
  assert.ok(Math.abs(latestInd.i_rms - 10) < 0.05);
  assert.ok(Math.abs(latestInd.p - 2200 * Math.cos(phi)) < 15);
  assert.ok(Math.abs(latestInd.q1 - 2200 * Math.sin(phi)) < 20);
  assert.ok(Math.abs(latestInd.phi_deg - 30) < 1.0);

  // DC fallback 48V * 5A -> 240W, freq = 0
  const dcTracker = new MockSinglePhasePowerTracker({
    kind: "single_phase_power",
    voltage_channel: "dc/u",
    current_channel: "dc/i",
    output_prefix: "dc",
    voltage_scale: 1,
    voltage_offset: 0,
    current_scale: 1,
    current_offset: 0,
    nominal_freq_hz: 50,
    zero_cross_hysteresis_v: 0.5,
    min_freq_hz: 5,
    fallback_window_ms: 100,
    outputs: normalizePowerFlags(),
  });
  for (let k = 0; k <= 300; k += 1) {
    dcTracker.processSample(k * 1_000_000, 48, 5);
  }
  const latestDc = dcTracker.history.at(-1)!;
  assert.equal(latestDc.freq, 0);
  assert.ok(Math.abs(latestDc.u_rms - 48) < 1e-3);
  assert.ok(Math.abs(latestDc.i_rms - 5) < 1e-3);
  assert.ok(Math.abs(latestDc.p - 240) < 1e-2);
});

test("MockSimulationCore executes all virtual operators + power analyzer and streams DDSF blocks", () => {
  const core = new MockSimulationCore();

  // "virtual" is reserved
  assert.throws(() =>
    core.createSource({
      source_id: VIRTUAL_SOURCE_ID,
      display_name: "Reserved",
      transport: {
        kind: "sim",
        sample_rate_hz: 1000,
        analog_channels: 2,
        enable_glitch: false,
        glitch_period_samples: 0,
      },
      clock: { mode: "tier3_host_arrival" },
      protocol: { kind: "sim_preset" },
      auto_start: true,
    }),
  );

  core.upsertVirtualChannel({
    node_id: "v_a2d",
    name: "IaSign",
    enabled: true,
    operator: {
      kind: "analog_to_digital",
      input_channel: "sim0/ia",
      output_path: "ia_pos",
      mode: { mode: "schmitt", threshold: 0, hysteresis: 0.2, invert: false },
    },
  });
  core.upsertVirtualChannel({
    node_id: "v_fsm",
    name: "IaBand",
    enabled: true,
    operator: {
      kind: "to_state_machine",
      output_path: "ia_band",
      mode: {
        mode: "range_map",
        input_channel: "sim0/ia",
        hysteresis: 0.2,
        rules: [
          { min: null, max: 0, code: 1, name: "NEG", color_rgba: 0x38bdf8ff },
          { min: 0, max: null, code: 2, name: "POS", color_rgba: 0x34d399ff },
        ],
        default_state: { code: 0, name: "ZERO", color_rgba: null },
      },
    },
  });
  core.upsertVirtualChannel({
    node_id: "v_d2a",
    name: "PwmDac",
    enabled: true,
    operator: {
      kind: "to_analog",
      output_path: "pwm_dac",
      unit: "V",
      mode: {
        mode: "level_map",
        input_channel: "virtual/ia_pos",
        low_value: 0,
        high_value: 5,
        threshold: 0.5,
        smoothing_tau_ms: 0,
      },
    },
  });
  core.upsertVirtualChannel({
    node_id: "v_formula",
    name: "ChainedPower",
    enabled: true,
    operator: {
      kind: "formula",
      expression: "[sim0/v_bus] * [sim0/ia] + [virtual/pwm_dac]",
      output_path: "p_chain",
      unit: "W",
      output_digital: false,
    },
  });
  core.upsertVirtualChannel({
    node_id: "v_rms",
    name: "IaRms",
    enabled: true,
    operator: {
      kind: "window_stats",
      input_channel: "sim0/ia",
      output_path: "ia_rms",
      unit: "A",
      metric: "rms",
      window_ms: 20,
    },
  });
  core.upsertVirtualChannel({
    node_id: "v_int",
    name: "IaInt",
    enabled: true,
    operator: {
      kind: "calculus",
      input_channel: "sim0/ia",
      output_path: "ia_int",
      unit: "As",
      mode: {
        mode: "integral",
        scale: 1,
        dc_offset: 0,
        initial_value: 0,
        clamp_min: -5,
        clamp_max: 5,
      },
    },
  });
  const pwrSummary = core.upsertVirtualChannel({
    node_id: "v_pwr",
    name: "GridPower",
    enabled: true,
    operator: {
      kind: "single_phase_power",
      voltage_channel: "sim0/ia",
      current_channel: "sim0/ib",
      output_prefix: "pwr",
      voltage_scale: 1,
      voltage_offset: 0,
      current_scale: 1,
      current_offset: 0,
      nominal_freq_hz: 50,
      zero_cross_hysteresis_v: 0.1,
      min_freq_hz: 5,
      fallback_window_ms: 100,
      outputs: normalizePowerFlags(),
    },
  });
  assert.equal(pwrSummary.output_channels.length, 12);

  // Reject self cycle without removing existing nodes
  assert.throws(() =>
    core.upsertVirtualChannel({
      node_id: "v_cycle",
      name: "Bad",
      enabled: true,
      operator: {
        kind: "formula",
        expression: "[virtual/loop] + 1",
        output_path: "loop",
        unit: null,
        output_digital: false,
      },
    }),
  );

  const tree = core.getChannelTree();
  const virtGrp = tree.sources.find((g) => g.source.source_id === VIRTUAL_SOURCE_ID);
  assert.ok(virtGrp);
  const byLocal = new Map(virtGrp.channels.map((c) => [c.local_path, c.channel_id]));
  assert.ok(byLocal.has("ia_pos") && byLocal.has("ia_band") && byLocal.has("pwr.u_rms"));

  core.upsertScopeViewport({
    scope_id: 1,
    title: "Virtual Scope",
    canvas_width_px: 800,
    time_span_ns: 20_000_000,
    pre_trigger_ratio: 0.25,
    pan_offset_ns: 0,
    sweep_mode: "roll",
    auto_timeout_ms: 120,
    trigger: null,
    tracks: [
      {
        channel_id: byLocal.get("p_chain")!,
        lane: "analog",
        visible: true,
        y_scale: 100,
        y_offset: 0,
        color_rgba: 0x38bdf8ff,
      },
      {
        channel_id: byLocal.get("ia_pos")!,
        lane: "digital",
        visible: true,
        y_scale: 1,
        y_offset: 0,
        color_rgba: 0x22d3eeff,
      },
      {
        channel_id: byLocal.get("ia_band")!,
        lane: "state_ribbon",
        visible: true,
        y_scale: 1,
        y_offset: 0,
        color_rgba: 0x34d399ff,
      },
      {
        channel_id: byLocal.get("pwr.u_rms")!,
        lane: "analog",
        visible: true,
        y_scale: 10,
        y_offset: 0,
        color_rgba: 0xf59e0bff,
      },
    ],
    sync_group_id: null,
    paused: false,
  });

  // Step ~250ms so AC + fallback windows accumulate
  let frameBytes = core.step(16.6, 1000).frameBuffer;
  for (let i = 1; i < 18; i += 1) {
    frameBytes = core.step(16.6, 1000 + i * 17).frameBuffer;
  }
  const frame = parseDdsfFrame(frameBytes);

  // Snapshots include virtual channels
  assert.ok(frame.snapshots.some((s) => s.channelNumId === byLocal.get("p_chain")!));
  assert.ok(frame.snapshots.some((s) => s.channelNumId === byLocal.get("pwr.u_rms")!));

  const sc = frame.scopes.find((s) => s.scopeId === 1)!;
  const kinds = new Set(sc.tracks.map((tr) => tr.payload.kind));
  assert.ok(kinds.has(DdsfBlockKind.AnalogM4Column) || kinds.has(DdsfBlockKind.AnalogRawXY));
  assert.ok(kinds.has(DdsfBlockKind.DigitalEdges));
  assert.ok(kinds.has(DdsfBlockKind.StateRibbons));
});

test("WorkspaceStore virtual channel lifecycle, Watch Table helper, and ddscope.virtual_channels.v1 + project roundtrip", async () => {
  const bus = new HighFreqTopicBus();
  const client = new BridgeClient(bus);
  const store = createWorkspaceStore(client);

  try {
    await store.getState().initWorkspace();

    await store.getState().upsertVirtualChannel({
      node_id: "v_1",
      name: "Bus Ripple",
      enabled: true,
      operator: {
        kind: "window_stats",
        input_channel: "sim0/v_bus",
        output_path: "vbus_rms",
        unit: "V",
        metric: "rms",
        window_ms: 20,
      },
    });

    await store.getState().upsertVirtualChannel({
      node_id: "v_2",
      name: "Motor Power",
      enabled: true,
      operator: {
        kind: "single_phase_power",
        voltage_channel: "sim0/ia",
        current_channel: "sim0/ib",
        output_prefix: "mtr",
        voltage_scale: 1,
        voltage_offset: 0,
        current_scale: 1,
        current_offset: 0,
        nominal_freq_hz: 50,
        zero_cross_hysteresis_v: 0.1,
        min_freq_hz: 5,
        fallback_window_ms: 100,
        outputs: normalizePowerFlags(),
      },
    });

    // Place Watch Table with all 12 outputs on active canvas
    const watchId = store.getState().placeVirtualOutputsWatchTable("v_2");
    assert.ok(watchId.length > 0);
    const activeCid = store.getState().activeCanvasId!;
    const watchWidget = store.getState().canvases[activeCid].widgets.find((w) => w.id === watchId)!;
    assert.equal(watchWidget.type, "watch_table");
    assert.equal(watchWidget.bindings.length, 12);

    // Bind virtual channel to MSO scope 1 as well
    const virtVbus = Object.values(store.getState().channelsById).find(
      (c) => c.full_path === "virtual/vbus_rms",
    )!;
    assert.ok(virtVbus);
    store.getState().bindChannelToScope(1, virtVbus.channel_id);

    // Physical sources export MUST exclude virtual source
    const exportedSources = parseSourcesConfigJson(store.getState().exportSourcesConfig());
    assert.equal(
      exportedSources.sources.some((s) => s.source_id === VIRTUAL_SOURCE_ID),
      false,
    );

    // Standalone ddscope.virtual_channels.v1 export/import + cross-schema guards
    const virtualJson = store.getState().exportVirtualChannelsConfig();
    const parsedVirt = parseVirtualChannelsConfigJson(virtualJson);
    assert.equal(parsedVirt.schema, VIRTUAL_CHANNELS_CONFIG_SCHEMA_V1);
    assert.equal(parsedVirt.virtualChannels.length, 2);

    assert.throws(() => parseSourcesConfigJson(virtualJson), /Virtual Channels panel/);
    assert.throws(() => parseBoardConfigJson(virtualJson), /Virtual Channels panel/);
    assert.throws(() => parseProjectConfigJson(virtualJson), /Virtual Channels panel/);
    assert.throws(() => parseCommandsConfigJson(virtualJson), /Virtual Channels panel/);

    const exportedProjectJson = store.getState().exportProjectConfig();
    store.getState().setSidebarTab("virtual");
    const exportedWithTab = store.getState().exportProjectConfig();
    const parsedProj = parseProjectConfigJson(exportedWithTab);
    assert.equal(parsedProj.virtualChannels?.length, 2);
    assert.equal(parsedProj.board.ui.sidebarTab, "virtual");

    // Clear all virtual channels and re-import project -> virtual channels + scope tracks + watch table restore cleanly
    await store.getState().clearVirtualChannels();
    assert.equal(store.getState().virtualChannels.length, 0);

    const importRes = await store.getState().importProjectConfig(exportedProjectJson);
    assert.equal(importRes.missingChannelPaths.length, 0);
    assert.equal(store.getState().virtualChannels.length, 2);

    const restoredVbus = Object.values(store.getState().channelsById).find(
      (c) => c.full_path === "virtual/vbus_rms",
    )!;
    assert.ok(store.getState().scopes[1].tracks.some((tr) => tr.channelId === restoredVbus.channel_id));
    const restoredWatch = store
      .getState()
      .canvases[store.getState().activeCanvasId!].widgets.find((w) => w.type === "watch_table" && w.bindings.length === 12);
    assert.ok(restoredWatch);

    // Test standalone importVirtualChannelsConfig replaceExisting
    await store.getState().clearVirtualChannels();
    const vImpRes = await store
      .getState()
      .importVirtualChannelsConfig(virtualJson, { replaceExisting: true });
    assert.equal(vImpRes.importedCount, 2);
    assert.equal(store.getState().virtualChannels.length, 2);
  } finally {
    client.stopDataPlane();
  }
});
