import test from "node:test";
import assert from "node:assert/strict";
import {
  BridgeClient,
  HighFreqTopicBus,
  formatUdpRemoteTxAddr,
  parseUdpRemoteTxAddr,
} from "../src/bridge/index.ts";
import { DdsfFrameBuilder, parseDdsfFrame } from "../src/bridge/ddsf-parser.ts";
import { DdsfBlockKind, TriggerRuntimeStateCode } from "../src/bridge/ddsf-spec.ts";
import { MockSimulationCore } from "../src/bridge/mock-ddsf-core.ts";
import { ScopeGeometryBuilder } from "../src/scope-engine/geometry.ts";
import {
  evaluateTrackSampleAtNorm,
  hitTestHudHandle,
  renderScopeHud,
} from "../src/scope-engine/hud-overlay.ts";
import { computeMsoLayout, type ScopeTrackRenderConfig } from "../src/scope-engine/layout.ts";
import { formatCursorTimeNs } from "../src/scope-engine/time-format.ts";
import { createWorkspaceStore } from "../src/workspace/store.ts";

interface RecordedHudDrawCalls {
  fillTexts: string[];
  segments: Array<{ x0: number; y0: number; x1: number; y1: number }>;
}

function createMockHudContext(): {
  ctx: CanvasRenderingContext2D;
  recorded: RecordedHudDrawCalls;
} {
  const recorded: RecordedHudDrawCalls = {
    fillTexts: [],
    segments: [],
  };
  let curX = 0;
  let curY = 0;
  const stub = {
    font: "",
    textBaseline: "alphabetic",
    fillStyle: "#ffffff",
    textAlign: "left",
    strokeStyle: "#ffffff",
    lineWidth: 1,
    save() {},
    restore() {},
    setTransform() {},
    clearRect() {},
    setLineDash() {},
    fillRect() {},
    strokeRect() {},
    beginPath() {},
    closePath() {},
    fill() {},
    stroke() {},
    arc() {},
    moveTo(x: number, y: number) {
      curX = x;
      curY = y;
    },
    lineTo(x: number, y: number) {
      recorded.segments.push({ x0: curX, y0: curY, x1: x, y1: y });
      curX = x;
      curY = y;
    },
    measureText(text: string) {
      return { width: text.length * 6.2 } as TextMetrics;
    },
    fillText(text: string) {
      recorded.fillTexts.push(text);
    },
  };
  return {
    ctx: stub as unknown as CanvasRenderingContext2D,
    recorded,
  };
}

test("UDP address helpers and MockSimulationCore UDP remote_tx_addr guard", () => {
  assert.equal(formatUdpRemoteTxAddr("192.168.1.50", 9002), "192.168.1.50:9002");
  assert.equal(formatUdpRemoteTxAddr("::1", 9002), "[::1]:9002");
  assert.equal(formatUdpRemoteTxAddr("10.0.0.5:7777", 9002), "10.0.0.5:7777");
  assert.equal(formatUdpRemoteTxAddr("   ", 9002), null);
  assert.throws(() => formatUdpRemoteTxAddr("192.168.1.50", 0));
  assert.throws(() => formatUdpRemoteTxAddr("192.168.1.50", 70_000));

  assert.deepEqual(parseUdpRemoteTxAddr("192.168.1.50:9100"), {
    ip: "192.168.1.50",
    port: 9100,
  });
  assert.deepEqual(parseUdpRemoteTxAddr("[::1]:9005"), { ip: "::1", port: 9005 });
  assert.deepEqual(parseUdpRemoteTxAddr(null, 9002), { ip: "", port: 9002 });

  const core = new MockSimulationCore();
  core.createSource({
    source_id: "udp0",
    display_name: "UDP TX Rig",
    transport: {
      kind: "udp",
      bind_addr: "0.0.0.0:9001",
      multicast_group: null,
      recv_buffer_bytes: 1_048_576,
      remote_tx_addr: null,
    },
    clock: { mode: "tier2_software_pll", nominal_sample_rate_hz: 100_000 },
    protocol: { codec: "just_float", channel_names: ["u0", "u1"] },
    auto_start: true,
  });

  assert.throws(
    () =>
      core.sendDownlink({
        source_id: "udp0",
        payload: { kind: "ascii_text", text: "PING", append_newline: true },
        priority: "high",
        coalesce_key: null,
      }),
    /remote_tx_addr/,
  );

  core.updateSource("udp0", {
    source_id: "udp0",
    display_name: "UDP TX Rig",
    transport: {
      kind: "udp",
      bind_addr: "0.0.0.0:9001",
      multicast_group: null,
      recv_buffer_bytes: 1_048_576,
      remote_tx_addr: "127.0.0.1:9002",
    },
    clock: { mode: "tier2_software_pll", nominal_sample_rate_hz: 100_000 },
    protocol: { codec: "just_float", channel_names: ["u0", "u1"] },
    auto_start: true,
  });

  const receipt = core.sendDownlink({
    source_id: "udp0",
    payload: { kind: "ascii_text", text: "PING", append_newline: true },
    priority: "high",
    coalesce_key: null,
  });
  assert.equal(receipt.accepted, true);
});

test("MockSimulationCore freezes X-axis time when all sources stop and records run boundaries", () => {
  const core = new MockSimulationCore({ autoStartDefaultSim: false });
  const sim0 = core.getChannelTree().sources.find((s) => s.source.source_id === "sim0")!;
  const iaId = sim0.channels.find((c) => c.local_path === "ia")!.channel_id;

  core.upsertScopeViewport({
    scope_id: 1,
    title: "Roll Freeze Test",
    canvas_width_px: 800,
    time_span_ns: 20_000_000,
    pre_trigger_ratio: 0.25,
    pan_offset_ns: 0,
    sweep_mode: "roll",
    auto_timeout_ms: 100,
    trigger: null,
    tracks: [
      { channel_id: iaId, lane: "analog", visible: true, y_scale: 1, y_offset: 0, color_rgba: 0x38bdf8ff },
    ],
    sync_group_id: null,
    paused: false,
  });

  // Unstarted: timeline frozen and tracks stay empty
  const fIdle1 = parseDdsfFrame(core.step(10, 100).frameBuffer);
  const fIdle2 = parseDdsfFrame(core.step(10, 110).frameBuffer);
  assert.equal(fIdle1.header.hostNowNs, fIdle2.header.hostNowNs);
  assert.equal(fIdle1.scopes[0].tracks.length, 0);
  assert.deepEqual(core.getRunBoundaries(), []);

  // Run #1: start source and advance time
  core.startSource("sim0");
  const fRun1A = parseDdsfFrame(core.step(10, 120).frameBuffer);
  const fRun1B = parseDdsfFrame(core.step(10, 130).frameBuffer);
  assert.ok(fRun1B.header.hostNowNs > fRun1A.header.hostNowNs);
  assert.ok(fRun1B.scopes[0].tracks.length > 0);
  assert.deepEqual(core.getRunBoundaries(), []);

  // Stop all sources: timeline freezes cleanly even across long wall-clock elapsed time
  core.stopSource("sim0");
  const fStop1 = parseDdsfFrame(core.step(250, 380).frameBuffer);
  const fStop2 = parseDdsfFrame(core.step(250, 630).frameBuffer);
  assert.equal(fStop1.header.hostNowNs, fStop2.header.hostNowNs);

  // Run #2: restarting after stop records exact divider timestamp and resumes without gap
  core.startSource("sim0");
  const boundaries = core.getRunBoundaries();
  assert.equal(boundaries.length, 1);
  assert.equal(boundaries[0], fStop2.header.hostNowNs);

  const fRun2 = parseDdsfFrame(core.step(10, 640).frameBuffer);
  const deltaRun2Ns = fRun2.header.hostNowNs - fStop2.header.hostNowNs;
  assert.ok(deltaRun2Ns > 0 && deltaRun2Ns <= 20_000_000);
});

test("Workspace store updateSource preserves bindings on same-ID edit and rebinds on source_id rename", async () => {
  const bus = new HighFreqTopicBus();
  const client = new BridgeClient(bus);
  const store = createWorkspaceStore(client);

  try {
    await store.getState().initWorkspace();

    const simChannels = Object.values(store.getState().channelsById).filter(
      (c) => c.source_id === "sim0",
    );
    const iaCh = simChannels.find((c) => c.local_path === "ia")!;
    assert.ok(iaCh);

    store.getState().updateScope(1, (prev) => ({
      ...prev,
      trigger: {
        kind: "edge",
        channel_id: iaCh.channel_id,
        level: 1.5,
        hysteresis: 0.2,
        slope: "rising",
      },
      tracks: [
        {
          channelId: iaCh.channel_id,
          lane: "analog",
          visible: true,
          yScale: 2,
          yOffset: 1,
          colorRgba: 0xf97316ff,
        },
      ],
    }));

    const meterId = store.getState().addWidgetToCanvas(1, "digital_meter", { col: 0, row: 12 });
    store.getState().bindChannelToWidget(1, meterId, iaCh.channel_id);
    const cmdId = store.getState().addCommand({
      name: "Sim Gain",
      format: "string",
      template: "kp=%f",
      appendNewline: true,
      targetSourceId: "sim0",
      priority: "normal",
    });
    store.getState().updateDownlinkConfig({ targetSource: "sim0" });

    // 1. Same-ID edit: keeps channel IDs & updates config
    await store.getState().updateSource("sim0", {
      source_id: "sim0",
      display_name: "Power Stage Sim Edited",
      transport: {
        kind: "sim",
        sample_rate_hz: 500_000,
        analog_channels: 4,
        enable_glitch: true,
        glitch_period_samples: 500_000,
      },
      clock: { mode: "tier1_hardware_counter", tick_hz: 500_000, counter_bits: 32 },
      protocol: { codec: "sim_preset" },
      auto_start: true,
    });

    assert.equal(
      store.getState().sourceConfigsById["sim0"]?.display_name,
      "Power Stage Sim Edited",
    );
    assert.equal(store.getState().scopes[1].tracks[0].channelId, iaCh.channel_id);
    assert.equal(store.getState().scopes[1].trigger?.kind, "edge");
    assert.equal(
      store.getState().scopes[1].trigger?.kind === "edge" &&
        store.getState().scopes[1].trigger.channel_id,
      iaCh.channel_id,
    );
    assert.equal(
      store.getState().canvases[1].widgets.find((w) => w.id === meterId)?.primaryBinding?.channelId,
      iaCh.channel_id,
    );
    assert.equal(store.getState().commands.find((c) => c.id === cmdId)?.targetSourceId, "sim0");

    // 2. Create rig_a (just_float) and rename rig_a -> rig_b:
    //    verifies sourceConfigsById move, track/trigger/widget rebind by local_path,
    //    and command/downlink targetSourceId remapping.
    await store.getState().createSource({
      source_id: "rig_a",
      display_name: "Rig A",
      transport: {
        kind: "udp",
        bind_addr: "0.0.0.0:9105",
        multicast_group: null,
        recv_buffer_bytes: 1_048_576,
        remote_tx_addr: "127.0.0.1:9106",
      },
      clock: { mode: "tier2_software_pll", nominal_sample_rate_hz: 50_000 },
      protocol: { codec: "just_float", channel_names: ["va", "vb"] },
      auto_start: true,
    });
    const rigVa = Object.values(store.getState().channelsById).find(
      (c) => c.full_path === "rig_a/va",
    )!;
    assert.ok(rigVa);

    store.getState().bindChannelToScope(1, rigVa.channel_id);
    store.getState().updateScope(1, (prev) => ({
      ...prev,
      trigger: {
        kind: "edge",
        channel_id: rigVa.channel_id,
        level: 2.5,
        hysteresis: 0.1,
        slope: "rising",
      },
      tracks: prev.tracks.map((tr) =>
        tr.channelId === rigVa.channel_id
          ? { ...tr, yScale: 4, yOffset: -2, colorRgba: 0x22c55eff }
          : tr,
      ),
    }));
    store.getState().bindChannelToWidget(1, meterId, rigVa.channel_id);
    const rigCmdId = store.getState().addCommand({
      name: "Rig Gain",
      format: "string",
      template: "rg=%f",
      appendNewline: true,
      targetSourceId: "rig_a",
      priority: "normal",
    });
    store.getState().updateDownlinkConfig({ targetSource: "rig_a" });

    await store.getState().updateSource("rig_a", {
      source_id: "rig_b",
      display_name: "Rig B Renamed",
      transport: {
        kind: "udp",
        bind_addr: "0.0.0.0:9105",
        multicast_group: null,
        recv_buffer_bytes: 1_048_576,
        remote_tx_addr: "127.0.0.1:9200",
      },
      clock: { mode: "tier2_software_pll", nominal_sample_rate_hz: 100_000 },
      protocol: { codec: "just_float", channel_names: ["va", "vb"] },
      auto_start: true,
    });

    assert.equal(store.getState().sourceConfigsById["rig_a"], undefined);
    assert.equal(
      store.getState().sourceConfigsById["rig_b"]?.display_name,
      "Rig B Renamed",
    );
    const newRigVa = Object.values(store.getState().channelsById).find(
      (c) => c.full_path === "rig_b/va",
    );
    assert.ok(newRigVa);

    const reboundScope = store.getState().scopes[1];
    const reboundTrack = reboundScope.tracks.find((tr) => tr.channelId === newRigVa.channel_id);
    assert.ok(reboundTrack);
    assert.equal(reboundTrack.yScale, 4);
    assert.equal(reboundTrack.yOffset, -2);
    assert.equal(reboundTrack.colorRgba, 0x22c55eff);
    assert.equal(
      reboundScope.trigger?.kind === "edge" && reboundScope.trigger.channel_id,
      newRigVa.channel_id,
    );

    const reboundWidget = store
      .getState()
      .canvases[1].widgets.find((w) => w.id === meterId);
    assert.equal(reboundWidget?.primaryBinding?.channelId, newRigVa.channel_id);

    const reboundCmd = store.getState().commands.find((c) => c.id === rigCmdId);
    assert.equal(reboundCmd?.targetSourceId, "rig_b");
    assert.equal(store.getState().downlinkConfig.targetSource, "rig_b");
  } finally {
    client.stopDataPlane();
  }
});

test("evaluateTrackSampleAtNorm, hitTestHudHandle axis gutters, and formatCursorTimeNs", () => {
  const tracks: ScopeTrackRenderConfig[] = [
    {
      channelId: 101,
      label: "raw_ch",
      unit: "V",
      lane: "analog",
      visible: true,
      yScale: 1,
      yOffset: 0,
      colorRgba: 0x38bdf8ff,
      stateLabels: [],
    },
    {
      channelId: 102,
      label: "m4_ch",
      unit: "A",
      lane: "analog",
      visible: true,
      yScale: 1,
      yOffset: 0,
      colorRgba: 0xf59e0bff,
      stateLabels: [],
    },
    {
      channelId: 103,
      label: "pwm",
      unit: null,
      lane: "digital",
      visible: true,
      yScale: 1,
      yOffset: 0,
      colorRgba: 0x22d3eeff,
      stateLabels: [],
    },
    {
      channelId: 104,
      label: "fsm",
      unit: null,
      lane: "state_ribbon",
      visible: true,
      yScale: 1,
      yOffset: 0,
      colorRgba: 0x10b981ff,
      stateLabels: [{ code: 2, name: "RUN", color_rgba: null }],
    },
  ];

  const fb = new DdsfFrameBuilder();
  const buf = fb.pack({
    frameSeq: 1,
    hostNowNs: 100_000_000,
    scopes: [
      {
        scopeId: 1,
        triggerState: TriggerRuntimeStateCode.Roll,
        mode: 0,
        winTStartNs: 0,
        winTSpanNs: 20_000_000,
        subpixelOffsetNs: 0,
        tracks: [
          {
            channelId: 101,
            blockKind: DdsfBlockKind.AnalogRawXY,
            points: [0.5, 10, 1.0, 20],
          },
          {
            channelId: 102,
            blockKind: DdsfBlockKind.AnalogM4Column,
            columns: [0.5, 10, 8, 12, 14, 1.0, 30, 28, 32, 34],
          },
          {
            channelId: 103,
            blockKind: DdsfBlockKind.DigitalEdges,
            digitalInitLevel: 0,
            edges: [0.25, 0.75],
          },
          {
            channelId: 104,
            blockKind: DdsfBlockKind.StateRibbons,
            ribbons: [{ x0Norm: 0.2, x1Norm: 0.8, stateId: 2, colorIdx: 2 }],
          },
        ],
      },
    ],
    snapshots: [],
  });
  const scopeView = parseDdsfFrame(buf).scopes[0];

  assert.equal(evaluateTrackSampleAtNorm(scopeView.tracks[0], 0.1, tracks[0]), null);
  assert.deepEqual(evaluateTrackSampleAtNorm(scopeView.tracks[0], 0.75, tracks[0]), {
    kind: "analog",
    value: 15,
  });

  assert.equal(evaluateTrackSampleAtNorm(scopeView.tracks[1], 0.1, tracks[1]), null);
  assert.deepEqual(evaluateTrackSampleAtNorm(scopeView.tracks[1], 0.5, tracks[1]), {
    kind: "analog",
    value: 12,
  });
  assert.deepEqual(evaluateTrackSampleAtNorm(scopeView.tracks[1], 1.0, tracks[1]), {
    kind: "analog",
    value: 32,
  });

  assert.deepEqual(evaluateTrackSampleAtNorm(scopeView.tracks[2], 0.1, tracks[2]), {
    kind: "digital",
    level: 0,
  });
  assert.deepEqual(evaluateTrackSampleAtNorm(scopeView.tracks[2], 0.5, tracks[2]), {
    kind: "digital",
    level: 1,
  });
  assert.deepEqual(evaluateTrackSampleAtNorm(scopeView.tracks[2], 0.9, tracks[2]), {
    kind: "digital",
    level: 0,
  });

  assert.equal(evaluateTrackSampleAtNorm(scopeView.tracks[3], 0.05, tracks[3]), null);
  assert.deepEqual(evaluateTrackSampleAtNorm(scopeView.tracks[3], 0.5, tracks[3]), {
    kind: "state",
    stateId: 2,
    label: "RUN",
    colorRgba: null,
  });

  assert.equal(formatCursorTimeNs(500, 10_000), "500 ns");
  assert.equal(formatCursorTimeNs(125_400, 500_000), "125.40 µs");
  assert.equal(formatCursorTimeNs(-1_234_500, 10_000_000), "-1.2345 ms");
  assert.equal(formatCursorTimeNs(2_500_000_000, 1_000_000_000), "2.500 s");

  const layout = computeMsoLayout(900, 420, tracks, "overlay");
  const baseHudState = {
    locale: "en" as const,
    sweepMode: "roll" as const,
    timeSpanNs: 20_000_000,
    preTriggerRatio: 0.25,
    panOffsetNs: 0,
    yAxisScale: 10,
    yAxisOffset: 0,
    trigger: null,
    cursorsEnabled: true,
    cursorARatio: 0.3,
    cursorBRatio: 0.75,
    activeAnalogChannelId: 101,
    tracks,
    scopeData: scopeView,
    ribbonLabels: [],
    dataModeTag: "MIXED" as const,
    backendKind: "webgpu" as const,
    fps: 60,
    p99FrameMs: 4.2,
    draggingHandle: "none" as const,
    runBoundaries: [{ xNorm: 0.5, runNumber: 2 }],
  };

  const yHoverX = layout.plotLeft - 18;
  const yHoverY = layout.analogRegion.y + layout.analogRegion.height * 0.25;
  assert.equal(hitTestHudHandle(yHoverX, yHoverY, layout, baseHudState), "yGutter");
  const { ctx: yCtx, recorded: yRec } = createMockHudContext();
  renderScopeHud(yCtx, layout, 1, {
    ...baseHudState,
    hoverHandle: "yGutter",
    pointerHover: { cssX: yHoverX, cssY: yHoverY },
  });
  assert.ok(yRec.fillTexts.some((txt) => txt.includes("RUN #2")));
  assert.ok(yRec.fillTexts.some((txt) => txt.includes("raw_ch:")));
  assert.ok(
    yRec.segments.some(
      (s) =>
        Math.abs(s.x0 - yHoverX) < 1 &&
        Math.abs(s.x1 - layout.plotRight) < 1 &&
        Math.abs(s.y0 - yHoverY) < 1 &&
        Math.abs(s.y1 - yHoverY) < 1,
    ),
  );

  const xHoverX = layout.plotLeft + layout.plotWidth * 0.5;
  const xHoverY = layout.plotBottom + 12;
  assert.equal(hitTestHudHandle(xHoverX, xHoverY, layout, baseHudState), "xGutter");
  const { ctx: xCtx, recorded: xRec } = createMockHudContext();
  renderScopeHud(xCtx, layout, 1, {
    ...baseHudState,
    hoverHandle: "xGutter",
    pointerHover: { cssX: xHoverX, cssY: xHoverY },
  });
  assert.ok(
    xRec.segments.some(
      (s) =>
        Math.abs(s.x0 - xHoverX) < 1 &&
        Math.abs(s.x1 - xHoverX) < 1 &&
        Math.abs(s.y0 - xHoverY) < 1 &&
        Math.abs(s.y1 - layout.plotTop) < 1,
    ),
  );
  assert.ok(xRec.fillTexts.some((txt) => txt.includes("-10.000 ms")));

  // Plot-interior hover must NOT draw a floating inspection card or hover hairline
  const plotHoverX = layout.plotLeft + layout.plotWidth * 0.4;
  const plotHoverY = layout.analogRegion.y + layout.analogRegion.height * 0.5;
  assert.equal(
    hitTestHudHandle(plotHoverX, plotHoverY, layout, { ...baseHudState, cursorsEnabled: false }),
    "plot",
  );
  const { ctx: plotCtx, recorded: plotRec } = createMockHudContext();
  renderScopeHud(plotCtx, layout, 1, {
    ...baseHudState,
    cursorsEnabled: false,
    runBoundaries: [],
    hoverHandle: "plot",
    pointerHover: { cssX: plotHoverX, cssY: plotHoverY },
  });
  assert.equal(
    plotRec.fillTexts.some((txt) => txt.startsWith("Time:")),
    false,
  );
  assert.equal(
    plotRec.segments.some((s) => Math.abs(s.x0 - plotHoverX) < 1 && Math.abs(s.x1 - plotHoverX) < 1),
    false,
  );
});

test("ScopeGeometryBuilder splits analog RawXY and M4 connecting segments across runBoundariesNorm", () => {
  const rawTrack: ScopeTrackRenderConfig[] = [
    {
      channelId: 101,
      label: "raw",
      unit: "V",
      lane: "analog",
      visible: true,
      yScale: 1,
      yOffset: 0,
      colorRgba: 0x38bdf8ff,
      stateLabels: [],
    },
  ];
  const m4Track: ScopeTrackRenderConfig[] = [
    {
      channelId: 102,
      label: "m4",
      unit: "V",
      lane: "analog",
      visible: true,
      yScale: 1,
      yOffset: 0,
      colorRgba: 0xf59e0bff,
      stateLabels: [],
    },
  ];

  const layoutRaw = computeMsoLayout(800, 300, rawTrack, "overlay");
  const layoutM4 = computeMsoLayout(800, 300, m4Track, "overlay");

  const fb = new DdsfFrameBuilder();
  const parsed = parseDdsfFrame(
    fb.pack({
      frameSeq: 1,
      hostNowNs: 10_000_000,
      scopes: [
        {
          scopeId: 1,
          triggerState: TriggerRuntimeStateCode.Roll,
          mode: 0,
          winTStartNs: 0,
          winTSpanNs: 10_000_000,
          subpixelOffsetNs: 0,
          tracks: [
            {
              channelId: 101,
              blockKind: DdsfBlockKind.AnalogRawXY,
              points: [0.2, 1, 0.4, 2, 0.6, 8],
            },
            {
              channelId: 102,
              blockKind: DdsfBlockKind.AnalogM4Column,
              columns: [
                0.2, 1, 1, 1, 1,
                0.4, 2, 2, 2, 2,
                0.6, 8, 8, 8, 8,
              ],
            },
          ],
        },
      ],
      snapshots: [],
    }),
  ).scopes[0];

  const geomRawFull = new ScopeGeometryBuilder().build(layoutRaw, 1, parsed, rawTrack);
  const geomRawSplit = new ScopeGeometryBuilder().build(
    layoutRaw,
    1,
    parsed,
    rawTrack,
    undefined,
    [0.5],
  );
  assert.equal(geomRawFull.signalVertexCount - geomRawSplit.signalVertexCount, 6);

  const geomM4Full = new ScopeGeometryBuilder().build(layoutM4, 1, parsed, m4Track);
  const geomM4Split = new ScopeGeometryBuilder().build(
    layoutM4,
    1,
    parsed,
    m4Track,
    undefined,
    [0.5],
  );
  assert.equal(geomM4Full.signalVertexCount - geomM4Split.signalVertexCount, 6);
});
