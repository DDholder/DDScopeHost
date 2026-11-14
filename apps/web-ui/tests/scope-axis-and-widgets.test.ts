import test from "node:test";
import assert from "node:assert/strict";
import { BridgeClient, HighFreqTopicBus } from "../src/bridge/index.ts";
import { DdsfFrameBuilder, parseDdsfFrame } from "../src/bridge/ddsf-parser.ts";
import { DdsfBlockKind, TriggerRuntimeStateCode } from "../src/bridge/ddsf-spec.ts";
import { MockSimulationCore } from "../src/bridge/mock-ddsf-core.ts";
import { FLOATS_PER_VERTEX, ScopeGeometryBuilder } from "../src/scope-engine/geometry.ts";
import {
  channelValueToAxis,
  channelValueToY,
  computeHorizontalPanBounds,
  computeMsoLayout,
  yToChannelValue,
  zoomXAxisAtCursor,
  zoomYAxisAtCursor,
  type ScopeTrackRenderConfig,
} from "../src/scope-engine/layout.ts";
import { createWorkspaceStore } from "../src/workspace/store.ts";

test("Whole-axis vertical coordinate mapping & independent per-channel scale/offset", () => {
  const rect = { x: 40, y: 20, width: 800, height: 320 };
  const axis = { yAxisScale: 10, yAxisOffset: 0 }; // +/- 40 across full height

  // Channel A: identity scale=1, offset=0; raw=20 -> axis=20 -> 0.5 - 20/80 = 0.25 -> y = 20 + 80 = 100
  const yA = channelValueToY(20, rect, axis, 1, 0);
  assert.ok(Math.abs(yA - 100) < 1e-6);

  // Channel B: independent scale=0.5, offset=-10; raw=60 -> axis=20 -> identical Y position on shared axis
  assert.equal(channelValueToAxis(60, 0.5, -10), 20);
  const yB = channelValueToY(60, rect, axis, 0.5, -10);
  assert.ok(Math.abs(yB - 100) < 1e-6);

  // Moving the whole axis offset by +20 shifts BOTH channels equally
  const shiftedAxis = { yAxisScale: 10, yAxisOffset: 20 };
  const yAShifted = channelValueToY(20, rect, shiftedAxis, 1, 0);
  const yBShifted = channelValueToY(60, rect, shiftedAxis, 0.5, -10);
  assert.ok(Math.abs(yAShifted - 180) < 1e-6);
  assert.ok(Math.abs(yAShifted - yBShifted) < 1e-6);

  // Inverse mapping recovers original raw physical values independently per channel
  assert.ok(Math.abs(yToChannelValue(yAShifted, rect, shiftedAxis, 1, 0) - 20) < 1e-6);
  assert.ok(Math.abs(yToChannelValue(yBShifted, rect, shiftedAxis, 0.5, -10) - 60) < 1e-6);
});

test("zoomYAxisAtCursor keeps coordinate under cursor stationary", () => {
  const rect = { x: 50, y: 10, width: 700, height: 400 };
  const oldAxis = { yAxisScale: 20, yAxisOffset: 15 };
  const cursorY = rect.y + rect.height * 0.3;

  const valBefore = yToChannelValue(cursorY, rect, oldAxis, 1, 0);
  const zoomedIn = zoomYAxisAtCursor(cursorY, rect, oldAxis.yAxisScale, oldAxis.yAxisOffset, 0.72);
  const valAfterIn = yToChannelValue(cursorY, rect, zoomedIn, 1, 0);
  assert.ok(Math.abs(valBefore - valAfterIn) < 1e-6);

  const zoomedOut = zoomYAxisAtCursor(cursorY, rect, zoomedIn.yAxisScale, zoomedIn.yAxisOffset, 1.45);
  const valAfterOut = yToChannelValue(cursorY, rect, zoomedOut, 1, 0);
  assert.ok(Math.abs(valBefore - valAfterOut) < 1e-6);
});

test("ScopeGeometryBuilder applies shared axisConfig across all tracks while preserving per-channel conditioning", () => {
  const tracks: ScopeTrackRenderConfig[] = [
    {
      channelId: 1,
      label: "ia",
      unit: "A",
      lane: "analog",
      visible: true,
      yScale: 1,
      yOffset: 0,
      colorRgba: 0x38bdf8ff,
      stateLabels: [],
    },
    {
      channelId: 2,
      label: "v_bus",
      unit: "V",
      lane: "analog",
      visible: true,
      yScale: 0.5,
      yOffset: -20, // raw 40V -> 40*0.5 - 20 = 0 axis units
      colorRgba: 0xf59e0bff,
      stateLabels: [],
    },
  ];

  const layout = computeMsoLayout(800, 320, tracks, "overlay");
  const fb = new DdsfFrameBuilder();
  const frameBuf = fb.pack({
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
            channelId: 1,
            blockKind: DdsfBlockKind.AnalogRawXY,
            points: [0, 0, 1, 0],
          },
          {
            channelId: 2,
            blockKind: DdsfBlockKind.AnalogRawXY,
            points: [0, 40, 1, 40],
          },
        ],
      },
    ],
    snapshots: [],
  });
  const parsed = parseDdsfFrame(frameBuf);

  const geo = new ScopeGeometryBuilder();
  const resCentered = geo.build(layout, 1, parsed.scopes[0], tracks, {
    yAxisScale: 10,
    yAxisOffset: 0,
  });
  const sigBase = resCentered.gridVertexCount * FLOATS_PER_VERTEX;
  const avgSegY = (verts: Float32Array, segIdx: number): number => {
    const base = sigBase + segIdx * 6 * FLOATS_PER_VERTEX;
    return (verts[base + 1] + verts[base + 2 * FLOATS_PER_VERTEX + 1]) * 0.5;
  };

  // Both ia=0 and v_bus=40*0.5-20=0 map to axis=0 -> exact center Y of analogRegion
  const centerY = layout.analogRegion.y + layout.analogRegion.height * 0.5;
  assert.ok(Math.abs(avgSegY(resCentered.vertices, 0) - centerY) < 1e-3);
  assert.ok(Math.abs(avgSegY(resCentered.vertices, 1) - centerY) < 1e-3);

  // Build shifted version via separate builder instance so previous vertices stay intact
  const geoShifted = new ScopeGeometryBuilder();
  const resShifted = geoShifted.build(layout, 1, parsed.scopes[0], tracks, {
    yAxisScale: 10,
    yAxisOffset: 20,
  });
  const expectedShiftedY = layout.analogRegion.y + layout.analogRegion.height * 0.75;
  assert.ok(Math.abs(avgSegY(resShifted.vertices, 0) - expectedShiftedY) < 1e-3);
  assert.ok(Math.abs(avgSegY(resShifted.vertices, 1) - expectedShiftedY) < 1e-3);
});

test("computeHorizontalPanBounds stops X pan when untriggered or buffer cannot move & zoomXAxisAtCursor anchors time", () => {
  const emptyBounds = computeHorizontalPanBounds({
    hasScopeData: false,
    hasVisibleTracks: true,
    triggerState: TriggerRuntimeStateCode.Armed,
    totalTrackElements: 0,
    sweepMode: "normal",
    paused: false,
    timeSpanNs: 10_000_000,
    currentPanNs: -2_000_000,
    winTStartNs: 0,
    winTSpanNs: 10_000_000,
    hostNowNs: 50_000_000,
    floorWinStartNs: 0,
  });
  assert.equal(emptyBounds.canPan, false);
  assert.equal(emptyBounds.minPanNs, -2_000_000);
  assert.equal(emptyBounds.maxPanNs, -2_000_000);

  // Cursor-centered X zoom invariant check at u = 0.75 across [plotLeft .. plotLeft+plotWidth]
  const oldSpan = 20_000_000;
  const oldPan = -5_000_000;
  const zeroRatio = 0.25;
  const plotLeft = 60;
  const plotWidth = 800;
  const cursorX = plotLeft + plotWidth * 0.75;
  const zoomed = zoomXAxisAtCursor(cursorX, plotLeft, plotWidth, oldSpan, oldPan, zeroRatio, 0.5);

  const cursorTimeBefore = oldPan + (0.75 - zeroRatio) * oldSpan;
  const cursorTimeAfter = zoomed.panOffsetNs + (0.75 - zeroRatio) * zoomed.timeSpanNs;
  assert.ok(Math.abs(cursorTimeBefore - cursorTimeAfter) <= 2);

  // Clamped zoom X checks pan bounds
  const rollLimited = zoomXAxisAtCursor(
    cursorX,
    plotLeft,
    plotWidth,
    oldSpan,
    oldPan,
    1.0,
    2.0,
    (newSpanNs) =>
      computeHorizontalPanBounds({
        hasScopeData: true,
        hasVisibleTracks: true,
        triggerState: TriggerRuntimeStateCode.Roll,
        totalTrackElements: 100,
        sweepMode: "roll",
        preTriggerRatio: 0.25,
        paused: false,
        timeSpanNs: oldSpan,
        targetSpanNs: newSpanNs,
        currentPanNs: oldPan,
        winTStartNs: 70_000_000,
        winTSpanNs: oldSpan,
        hostNowNs: 100_000_000,
        floorWinStartNs: 75_000_000, // 25ms buffer < 40ms newSpanNs -> locked to 0
      }),
  );
  assert.equal(rollLimited.timeSpanNs, 40_000_000);
  assert.equal(rollLimited.panOffsetNs, 0);
});

test("MockSimulationCore freezes Roll window on pause and clamps excessive left panOffsetNs", () => {
  const core = new MockSimulationCore();
  const sim0 = core.getChannelTree().sources[0];
  const chIa = sim0.channels[0].channel_id;

  core.upsertScopeViewport({
    scope_id: 1,
    title: "Roll Test",
    canvas_width_px: 800,
    time_span_ns: 10_000_000,
    pre_trigger_ratio: 0.25,
    pan_offset_ns: 0,
    sweep_mode: "roll",
    auto_timeout_ms: 120,
    trigger: null,
    tracks: [
      { channel_id: chIa, lane: "analog", visible: true, y_scale: 1, y_offset: 0, color_rgba: 0x38bdf8ff },
    ],
    sync_group_id: null,
    paused: false,
  });

  // Advance ~30 ms so history exists, then attempt an absurdly huge negative pan (-10 seconds)
  for (let i = 0; i < 4; i += 1) {
    core.step(8.33, 1000 + i);
  }

  core.upsertScopeViewport({
    scope_id: 1,
    title: "Roll Test",
    canvas_width_px: 800,
    time_span_ns: 10_000_000,
    pre_trigger_ratio: 0.25,
    pan_offset_ns: -10_000_000_000,
    sweep_mode: "roll",
    auto_timeout_ms: 120,
    trigger: null,
    tracks: [
      { channel_id: chIa, lane: "analog", visible: true, y_scale: 1, y_offset: 0, color_rgba: 0x38bdf8ff },
    ],
    sync_group_id: null,
    paused: false,
  });

  const clampedFrame = parseDdsfFrame(core.step(8.33, 1010).frameBuffer);
  // Window start must be clamped to >= 0 (never negative / phantom time before sim0 start)
  assert.ok(clampedFrame.scopes[0].winTStartNs >= 0n);

  // Freeze on pause
  core.upsertScopeViewport({
    scope_id: 1,
    title: "Roll Test",
    canvas_width_px: 800,
    time_span_ns: 10_000_000,
    pre_trigger_ratio: 0.25,
    pan_offset_ns: 0,
    sweep_mode: "roll",
    auto_timeout_ms: 120,
    trigger: null,
    tracks: [
      { channel_id: chIa, lane: "analog", visible: true, y_scale: 1, y_offset: 0, color_rgba: 0x38bdf8ff },
    ],
    sync_group_id: null,
    paused: true,
  });

  const pausedFrame1 = parseDdsfFrame(core.step(8.33, 1011).frameBuffer);
  const pausedFrame2 = parseDdsfFrame(core.step(8.33, 1012).frameBuffer);
  assert.equal(pausedFrame2.scopes[0].winTStartNs, pausedFrame1.scopes[0].winTStartNs);
});

test("Workspace store seeds default dashboard canvas, manages widgets, and defaults bound channels to 1x/+0", async () => {
  const bus = new HighFreqTopicBus();
  const client = new BridgeClient(bus);
  const store = createWorkspaceStore(client);

  await store.getState().initWorkspace();
  const initial = store.getState();

  assert.deepEqual(initial.scopeOrder, [1, 2]);
  assert.equal(initial.scopes[1].yAxisScale, 15);
  assert.equal(initial.scopes[1].yAxisOffset, 0);
  assert.equal(initial.scopes[1].wheelZoomTarget, "x");
  assert.deepEqual(initial.canvasOrder, [1]);
  assert.ok(initial.canvases[1]);
  assert.ok(initial.canvases[1].widgets.length >= 6);

  // Adding a digital_meter and binding a channel
  const allChannels = Object.values(initial.channelsById);
  const targetCh = allChannels[0];
  const widgetId = store.getState().addWidgetToCanvas(1, "digital_meter", { col: 0, row: 12 });
  store.getState().bindChannelToWidget(1, widgetId, targetCh.channel_id);

  const boundWidget = store
    .getState()
    .canvases[1].widgets.find((w) => w.id === widgetId);
  assert.ok(boundWidget);
  assert.deepEqual(boundWidget.primaryBinding, {
    channelId: targetCh.channel_id,
    scale: 1,
    offset: 0,
  });

  // Embedded mso_scope widget creates an undocked scope without mutating Dockview scopeOrder
  const embeddedWidgetId = store.getState().addWidgetToCanvas(1, "mso_scope");
  const afterEmbed = store.getState();
  const embeddedWidget = afterEmbed.canvases[1].widgets.find((w) => w.id === embeddedWidgetId);
  assert.ok(embeddedWidget && embeddedWidget.scopeId !== null);
  assert.deepEqual(afterEmbed.scopeOrder, [1, 2]);

  // Removing embedded widget cleans up its private scope
  store.getState().removeCanvasWidget(1, embeddedWidgetId);
  assert.equal(store.getState().scopes[embeddedWidget.scopeId!], undefined);

  // Binding a new channel onto an empty added scope initializes per-channel scale=1, offset=0
  const extraScopeId = store.getState().addScope("Custom Scope");
  const lastCh = allChannels[allChannels.length - 1];
  store.getState().bindChannelToScope(extraScopeId, lastCh.channel_id);
  const track = store
    .getState()
    .scopes[extraScopeId].tracks.find((t) => t.channelId === lastCh.channel_id);
  assert.ok(track);
  assert.equal(track.yScale, 1);
  assert.equal(track.yOffset, 0);

  client.stopDataPlane();
});
