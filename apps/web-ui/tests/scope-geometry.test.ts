import test from "node:test";
import assert from "node:assert/strict";
import { DdsfFrameBuilder, parseDdsfFrame } from "../src/bridge/ddsf-parser.ts";
import { DdsfBlockKind, TriggerRuntimeStateCode } from "../src/bridge/ddsf-spec.ts";
import {
  FLOATS_PER_VERTEX,
  ScopeGeometryBuilder,
  VERTEX_MODE_DENSE_HATCH,
} from "../src/scope-engine/geometry.ts";
import { evaluateTrackValueAtNorm } from "../src/scope-engine/hud-overlay.ts";
import { computeMsoLayout, type ScopeTrackRenderConfig } from "../src/scope-engine/layout.ts";
import { formatFrequencyHz, formatTimeNs, formatTimePerDiv } from "../src/scope-engine/time-format.ts";

function makeSampleTracks(): ScopeTrackRenderConfig[] {
  return [
    {
      channelId: 101,
      label: "sim0/ia",
      unit: "A",
      lane: "analog",
      visible: true,
      yScale: 5,
      yOffset: 0,
      colorRgba: 0x38bdf8ff,
      stateLabels: [],
    },
    {
      channelId: 104,
      label: "sim0/v_bus",
      unit: "V",
      lane: "analog",
      visible: true,
      yScale: 25,
      yOffset: 50,
      colorRgba: 0xf59e0bff,
      stateLabels: [],
    },
    {
      channelId: 105,
      label: "sim0/pwm_a",
      unit: null,
      lane: "digital",
      visible: true,
      yScale: 1,
      yOffset: 0,
      colorRgba: 0x22d3eeff,
      stateLabels: [],
    },
    {
      channelId: 108,
      label: "sim0/ctrl_state",
      unit: null,
      lane: "state_ribbon",
      visible: true,
      yScale: 1,
      yOffset: 0,
      colorRgba: 0x10b981ff,
      stateLabels: [
        { code: 0, name: "IDLE", color_rgba: null },
        { code: 2, name: "CLOSED_LOOP", color_rgba: null },
        { code: 3, name: "OCP_FAULT", color_rgba: null },
      ],
    },
  ];
}

test("MSO layout keeps analog, digital, and state ribbon lanes X-aligned in overlay & stacked", () => {
  const tracks = makeSampleTracks();
  const overlay = computeMsoLayout(960, 480, tracks, "overlay");
  assert.equal(overlay.analogSubLanes.length, 2);
  assert.equal(overlay.digitalLanes.length, 1);
  assert.equal(overlay.stateLanes.length, 1);

  for (const lane of [...overlay.analogSubLanes, ...overlay.digitalLanes, ...overlay.stateLanes]) {
    assert.equal(lane.rect.x, overlay.plotLeft);
    assert.equal(lane.rect.width, overlay.plotWidth);
  }
  assert.equal(overlay.analogSubLanes[0].rect.y, overlay.analogSubLanes[1].rect.y);

  const stacked = computeMsoLayout(960, 480, tracks, "stacked");
  assert.ok(stacked.analogSubLanes[1].rect.y > stacked.analogSubLanes[0].rect.y);
  for (const lane of [...stacked.analogSubLanes, ...stacked.digitalLanes, ...stacked.stateLanes]) {
    assert.equal(lane.rect.x, stacked.plotLeft);
    assert.equal(lane.rect.width, stacked.plotWidth);
  }
});

test("ScopeGeometryBuilder coalesces <2px dense PWM transitions to hatch vertices & emits ribbon labels", () => {
  const tracks = makeSampleTracks();
  const layout = computeMsoLayout(900, 420, tracks, "overlay");

  // 40 edges inside [0.20 .. 0.24] -> each pulse < 1px wide on a 818px plot
  const denseEdges: number[] = [];
  for (let i = 0; i < 40; i += 1) {
    denseEdges.push(0.2 + i * 0.001);
  }

  const builder = new DdsfFrameBuilder();
  const frameBuf = builder.pack({
    frameSeq: 1,
    hostNowNs: 100_000_000,
    scopes: [
      {
        scopeId: 1,
        triggerState: TriggerRuntimeStateCode.Roll,
        mode: 0,
        winTStartNs: 50_000_000,
        winTSpanNs: 20_000_000,
        subpixelOffsetNs: -10_000,
        tracks: [
          {
            channelId: 101,
            blockKind: DdsfBlockKind.AnalogRawXY,
            points: [0, 0, 0.5, 5.0, 1.0, 0],
          },
          {
            channelId: 105,
            blockKind: DdsfBlockKind.DigitalEdges,
            digitalInitLevel: 0,
            edges: denseEdges,
          },
          {
            channelId: 108,
            blockKind: DdsfBlockKind.StateRibbons,
            ribbons: [
              { x0Norm: 0.0, x1Norm: 0.6, stateId: 2, colorIdx: 2 },
              { x0Norm: 0.6, x1Norm: 1.0, stateId: 3, colorIdx: 3 },
            ],
          },
        ],
      },
    ],
    snapshots: [],
  });

  const frame = parseDdsfFrame(frameBuf);
  const geomBuilder = new ScopeGeometryBuilder();
  const geom = geomBuilder.build(layout, 1, frame.scopes[0], tracks);

  assert.equal(geom.vertices.length, geom.totalVertexCount * FLOATS_PER_VERTEX);
  assert.equal(geom.dataModeTag, "RAW");

  let hatchVertices = 0;
  for (let i = 0; i < geom.totalVertexCount; i += 1) {
    const mode = geom.vertices[i * FLOATS_PER_VERTEX + 10];
    if (mode === VERTEX_MODE_DENSE_HATCH) hatchVertices += 1;
  }
  assert.ok(hatchVertices >= 6, `expected >= 6 dense hatch vertices, got ${hatchVertices}`);

  assert.equal(geom.ribbonLabels.length, 2);
  assert.equal(geom.ribbonLabels[0].text, "CLOSED_LOOP");
  assert.equal(geom.ribbonLabels[1].text, "OCP_FAULT");
});

test("evaluateTrackValueAtNorm & time/frequency formatters", () => {
  const builder = new DdsfFrameBuilder();
  const buf = builder.pack({
    frameSeq: 1,
    hostNowNs: 1_000_000,
    scopes: [
      {
        scopeId: 1,
        triggerState: TriggerRuntimeStateCode.Roll,
        mode: 0,
        winTStartNs: 0,
        winTSpanNs: 1_000_000,
        subpixelOffsetNs: 0,
        tracks: [
          {
            channelId: 101,
            blockKind: DdsfBlockKind.AnalogRawXY,
            points: [0.0, 0.0, 1.0, 10.0],
          },
          {
            channelId: 102,
            blockKind: DdsfBlockKind.AnalogM4Column,
            columns: [0.0, 40, 38, 42, 42, 1.0, 50, 48, 52, 50],
          },
        ],
      },
    ],
    snapshots: [],
  });
  const frame = parseDdsfFrame(buf);
  const vRaw = evaluateTrackValueAtNorm(frame.scopes[0].tracks[0], 0.25);
  assert.ok(vRaw !== null && Math.abs(vRaw - 2.5) < 1e-5);
  const vM4 = evaluateTrackValueAtNorm(frame.scopes[0].tracks[1], 0.0);
  assert.equal(vM4, 41);

  assert.equal(formatTimeNs(500), "500 ns");
  assert.equal(formatTimeNs(250_000), "250.0 µs");
  assert.equal(formatTimePerDiv(20_000_000), "2.00 ms/div");
  assert.equal(formatFrequencyHz(20_000), "20.00 kHz");
});
