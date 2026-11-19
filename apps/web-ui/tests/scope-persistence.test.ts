import test from "node:test";
import assert from "node:assert/strict";
import { BridgeClient, HighFreqTopicBus } from "../src/bridge/index.ts";
import { DdsfFrameBuilder, parseDdsfFrame } from "../src/bridge/ddsf-parser.ts";
import { DdsfBlockKind, TriggerRuntimeStateCode } from "../src/bridge/ddsf-spec.ts";
import { Canvas2dScopeBackend } from "../src/scope-engine/canvas2d-backend.ts";
import { ScopeGeometryBuilder } from "../src/scope-engine/geometry.ts";
import { computeMsoLayout, type ScopeTrackRenderConfig } from "../src/scope-engine/layout.ts";
import {
  THERMAL_LUT_STOPS,
  buildAcquisitionSignature,
  buildPersistenceViewKey,
  computePersistenceDecayFactor,
  getPersistenceSettleDurationSec,
  getPersistenceTauSec,
  sampleThermalPaletteRgb,
  type PersistenceRenderState,
} from "../src/scope-engine/persistence.ts";
import { createWorkspaceStore } from "../src/workspace/store.ts";

function makeSampleTracks(): ScopeTrackRenderConfig[] {
  return [
    {
      channelId: 101,
      label: "sim0/ia",
      unit: "A",
      lane: "analog",
      visible: true,
      yScale: 1,
      yOffset: 0,
      colorRgba: 0x38bdf8ff,
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
      stateLabels: [{ code: 0, name: "IDLE", color_rgba: null }],
    },
  ];
}

function makeMock2dContext(ops: string[]): CanvasRenderingContext2D {
  const noop = () => {};
  return {
    fillStyle: "#000",
    strokeStyle: "#000",
    globalAlpha: 1,
    globalCompositeOperation: "source-over",
    save: () => ops.push("save"),
    restore: () => ops.push("restore"),
    setTransform: noop,
    clearRect: () => ops.push("clearRect"),
    fillRect: () => ops.push("fillRect"),
    beginPath: noop,
    rect: noop,
    clip: () => ops.push("clip"),
    moveTo: noop,
    lineTo: noop,
    closePath: noop,
    fill: () => ops.push("fill"),
    drawImage: () => ops.push("drawImage"),
  } as unknown as CanvasRenderingContext2D;
}

test("persistence decay physics and settle duration invariants", () => {
  assert.equal(getPersistenceTauSec("off"), 0);
  assert.equal(getPersistenceTauSec("short"), 0.28);
  assert.equal(getPersistenceTauSec("medium"), 1.0);
  assert.equal(getPersistenceTauSec("long"), 2.6);
  assert.equal(getPersistenceTauSec("infinite"), Infinity);

  assert.equal(getPersistenceSettleDurationSec("off"), 0);
  assert.equal(getPersistenceSettleDurationSec("infinite"), Infinity);
  assert.ok(
    Math.abs(getPersistenceSettleDurationSec("short") - 5.5 * 0.28) < 1e-9,
  );
  assert.ok(
    Math.abs(getPersistenceSettleDurationSec("medium") - 5.5 * 1.0) < 1e-9,
  );
  assert.ok(
    Math.abs(getPersistenceSettleDurationSec("long") - 5.5 * 2.6) < 1e-9,
  );

  assert.equal(computePersistenceDecayFactor("off", 0.016, false), 0);
  assert.equal(computePersistenceDecayFactor("infinite", 0.016, false), 1);
  assert.equal(computePersistenceDecayFactor("short", 0.016, true), 1);
  assert.equal(computePersistenceDecayFactor("medium", 0, false), 1);
  assert.equal(computePersistenceDecayFactor("medium", -0.5, false), 1);
  assert.equal(computePersistenceDecayFactor("medium", Number.NaN, false), 1);

  const dt = 0.05;
  const fShort = computePersistenceDecayFactor("short", dt, false);
  const fMedium = computePersistenceDecayFactor("medium", dt, false);
  const fLong = computePersistenceDecayFactor("long", dt, false);
  assert.ok(fShort > 0 && fShort < fMedium && fMedium < fLong && fLong < 1);
});

test("buildPersistenceViewKey detects coordinate changes and ignores non-analog / color mode changes", () => {
  const baseTracks = makeSampleTracks();
  const baseInput = {
    physicalWidth: 960,
    physicalHeight: 480,
    analogRegion: { x: 60, y: 24, width: 878, height: 376 },
    timeSpanNs: 20_000_000,
    panOffsetNs: 0,
    preTriggerRatio: 0.25,
    yAxisScale: 15,
    yAxisOffset: 0,
    analogLayout: "overlay" as const,
    sweepMode: "auto" as const,
    tracks: baseTracks,
    clearToken: 0,
  };

  const kBase = buildPersistenceViewKey(baseInput);
  assert.notEqual(buildPersistenceViewKey({ ...baseInput, physicalWidth: 961 }), kBase);
  assert.notEqual(buildPersistenceViewKey({ ...baseInput, timeSpanNs: 10_000_000 }), kBase);
  assert.notEqual(buildPersistenceViewKey({ ...baseInput, panOffsetNs: -500_000 }), kBase);
  assert.notEqual(buildPersistenceViewKey({ ...baseInput, preTriggerRatio: 0.5 }), kBase);
  assert.notEqual(buildPersistenceViewKey({ ...baseInput, yAxisScale: 30 }), kBase);
  assert.notEqual(buildPersistenceViewKey({ ...baseInput, yAxisOffset: 10 }), kBase);
  assert.notEqual(buildPersistenceViewKey({ ...baseInput, analogLayout: "stacked" }), kBase);
  assert.notEqual(buildPersistenceViewKey({ ...baseInput, sweepMode: "single" }), kBase);
  assert.notEqual(buildPersistenceViewKey({ ...baseInput, clearToken: 1 }), kBase);

  // Visible analog track yScale/yOffset or visibility change invalidates buffer
  const scaledAnalogTracks = baseTracks.map((tr) =>
    tr.channelId === 101 ? { ...tr, yScale: 2 } : tr,
  );
  assert.notEqual(buildPersistenceViewKey({ ...baseInput, tracks: scaledAnalogTracks }), kBase);

  const hiddenAnalogTracks = baseTracks.map((tr) =>
    tr.channelId === 101 ? { ...tr, visible: false } : tr,
  );
  assert.notEqual(buildPersistenceViewKey({ ...baseInput, tracks: hiddenAnalogTracks }), kBase);

  // Unrelated non-analog track changes (with unchanged analogRegion) do NOT invalidate
  const modifiedDigitalTracks = baseTracks.map((tr) =>
    tr.lane === "digital" ? { ...tr, yScale: 4, yOffset: 12 } : tr,
  );
  assert.equal(buildPersistenceViewKey({ ...baseInput, tracks: modifiedDigitalTracks }), kBase);
});

test("buildAcquisitionSignature prevents duplicate deposits and catches Roll fill updates", () => {
  assert.equal(buildAcquisitionSignature(null), null);

  const builder = new DdsfFrameBuilder();
  const makeScopeView = (
    winTStartNs: number,
    subpixelOffsetNs: number,
    points: number[],
  ) => {
    const buf = builder.pack({
      frameSeq: 1,
      hostNowNs: 100_000_000,
      scopes: [
        {
          scopeId: 1,
          triggerState: TriggerRuntimeStateCode.Roll,
          mode: 0,
          winTStartNs,
          winTSpanNs: 20_000_000,
          subpixelOffsetNs,
          tracks: [
            {
              channelId: 101,
              blockKind: DdsfBlockKind.AnalogRawXY,
              points,
            },
          ],
        },
      ],
      snapshots: [],
    });
    return parseDdsfFrame(buf).scopes[0];
  };

  const viewA = makeScopeView(0, 0, [0.1, 1.0, 0.5, 2.5]);
  const viewACopy = makeScopeView(0, 0, [0.1, 1.0, 0.5, 2.5]);
  assert.equal(buildAcquisitionSignature(viewA), buildAcquisitionSignature(viewACopy));

  // Initial Roll fill advance with winTStartNs still 0 must produce a new signature
  const viewRollAdvance = makeScopeView(0, 0, [0.1, 1.0, 0.5, 2.5, 0.7, -1.2]);
  assert.notEqual(
    buildAcquisitionSignature(viewA),
    buildAcquisitionSignature(viewRollAdvance),
  );

  // Window time or subpixel advance produces a new signature
  const viewShifted = makeScopeView(1_000_000, 12.5, [0.1, 1.0, 0.5, 2.5]);
  assert.notEqual(buildAcquisitionSignature(viewA), buildAcquisitionSignature(viewShifted));
});

test("sampleThermalPaletteRgb matches LUT stops and smoothly interpolates", () => {
  assert.deepEqual(sampleThermalPaletteRgb(-0.5), [...THERMAL_LUT_STOPS[0].rgb]);
  assert.deepEqual(sampleThermalPaletteRgb(2.0), [...THERMAL_LUT_STOPS[THERMAL_LUT_STOPS.length - 1].rgb]);

  for (const stop of THERMAL_LUT_STOPS) {
    const rgb = sampleThermalPaletteRgb(stop.t);
    for (let i = 0; i < 3; i += 1) {
      assert.ok(Math.abs(rgb[i] - stop.rgb[i]) < 1e-6);
    }
  }

  const midFirst = sampleThermalPaletteRgb(0.125);
  const s0 = THERMAL_LUT_STOPS[0].rgb;
  const s1 = THERMAL_LUT_STOPS[1].rgb;
  for (let i = 0; i < 3; i += 1) {
    assert.ok(Math.abs(midFirst[i] - (s0[i] + s1[i]) * 0.5) < 1e-6);
  }
});

test("ScopeGeometryBuilder partitions grid, analog, and overlay vertices accurately", () => {
  const tracks = makeSampleTracks();
  const layout = computeMsoLayout(960, 480, tracks, "overlay");
  const ddsf = new DdsfFrameBuilder();
  const buf = ddsf.pack({
    frameSeq: 1,
    hostNowNs: 100_000_000,
    scopes: [
      {
        scopeId: 1,
        triggerState: TriggerRuntimeStateCode.AutoFreerun,
        mode: 1,
        winTStartNs: 50_000_000,
        winTSpanNs: 20_000_000,
        subpixelOffsetNs: 0,
        tracks: [
          {
            channelId: 101,
            blockKind: DdsfBlockKind.AnalogRawXY,
            points: [0, 0, 0.5, 5, 1, 0],
          },
          {
            channelId: 105,
            blockKind: DdsfBlockKind.DigitalEdges,
            digitalInitLevel: 0,
            edges: [0.2, 0.6],
          },
          {
            channelId: 108,
            blockKind: DdsfBlockKind.StateRibbons,
            ribbons: [{ x0Norm: 0, x1Norm: 1, stateId: 0, colorIdx: 0 }],
          },
        ],
      },
    ],
    snapshots: [],
  });
  const scopeData = parseDdsfFrame(buf).scopes[0];

  const builder = new ScopeGeometryBuilder();
  const built = builder.build(layout, 1, scopeData, tracks);
  assert.ok(built.gridVertexCount > 0);
  assert.ok(built.analogVertexCount > 0);
  assert.ok(built.overlaySignalVertexCount > 0);
  assert.equal(
    built.analogVertexCount + built.overlaySignalVertexCount,
    built.signalVertexCount,
  );
  assert.equal(
    built.gridVertexCount + built.signalVertexCount,
    built.totalVertexCount,
  );

  // Analog-only scope yields overlaySignalVertexCount === 0
  const analogOnlyTracks = tracks.filter((tr) => tr.lane === "analog");
  const analogOnlyLayout = computeMsoLayout(960, 480, analogOnlyTracks, "overlay");
  const analogOnlyBuilt = builder.build(analogOnlyLayout, 1, scopeData, analogOnlyTracks);
  assert.ok(analogOnlyBuilt.analogVertexCount > 0);
  assert.equal(analogOnlyBuilt.overlaySignalVertexCount, 0);
  assert.equal(analogOnlyBuilt.analogVertexCount, analogOnlyBuilt.signalVertexCount);

  const idleBuilt = builder.build(layout, 1, null, tracks);
  assert.equal(idleBuilt.analogVertexCount, 0);
  assert.equal(idleBuilt.overlaySignalVertexCount, 0);
  assert.equal(idleBuilt.signalVertexCount, 0);
});

test("Canvas2dScopeBackend safely handles persistence in headless Node and offscreen canvas mocks", () => {
  const tracks = makeSampleTracks();
  const layout = computeMsoLayout(640, 360, tracks, "overlay");
  const ddsf = new DdsfFrameBuilder();
  const buf = ddsf.pack({
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
            points: [0, 0, 0.5, 4, 1, -2],
          },
        ],
      },
    ],
    snapshots: [],
  });
  const scopeData = parseDdsfFrame(buf).scopes[0];
  const geom = new ScopeGeometryBuilder().build(layout, 1, scopeData, tracks);

  const mainOps: string[] = [];
  const mainCtx = makeMock2dContext(mainOps);
  const backend = new Canvas2dScopeBackend(mainCtx);

  const basePersist: PersistenceRenderState = {
    mode: "medium",
    colorMode: "phosphor",
    decayFactor: 0.92,
    dtSec: 0.08,
    tauSec: 1.0,
    depositCurrentFrame: true,
    clear: false,
  };

  assert.doesNotThrow(() => {
    backend.render(640, 360, 1, layout, geom);
    backend.render(640, 360, 1, layout, geom, basePersist);
    backend.render(640, 360, 1, layout, geom, {
      ...basePersist,
      colorMode: "thermal",
      clear: true,
    });
  });

  // Also verify offscreen persistence pipeline when OffscreenCanvas is available
  const offOps: string[] = [];
  const origOffscreen = (globalThis as Record<string, unknown>).OffscreenCanvas;
  try {
    (globalThis as Record<string, unknown>).OffscreenCanvas = class MockOffscreenCanvas {
      width: number;
      height: number;
      constructor(w: number, h: number) {
        this.width = w;
        this.height = h;
      }
      getContext() {
        return makeMock2dContext(offOps);
      }
    };
    const offBackend = new Canvas2dScopeBackend(makeMock2dContext(mainOps));
    offBackend.render(640, 360, 1, layout, geom, basePersist);
    offBackend.render(640, 360, 1, layout, geom, {
      ...basePersist,
      colorMode: "thermal",
      dtSec: 0.2,
    });
    assert.ok(offOps.includes("clearRect"));
    assert.ok(offOps.includes("fill"));
    assert.ok(mainOps.includes("drawImage"));
    offBackend.dispose();
  } finally {
    if (origOffscreen === undefined) {
      delete (globalThis as Record<string, unknown>).OffscreenCanvas;
    } else {
      (globalThis as Record<string, unknown>).OffscreenCanvas = origOffscreen;
    }
    backend.dispose();
  }
});

test("Persistence settings round-trip through board/project config export and import with legacy defaults", async () => {
  const bus = new HighFreqTopicBus();
  const client = new BridgeClient(bus);
  const store = createWorkspaceStore(client);

  try {
    await store.getState().initWorkspace();

    assert.equal(store.getState().scopes[1].persistenceMode, "off");
    assert.equal(store.getState().scopes[1].persistenceColorMode, "phosphor");

    store.getState().updateScope(1, (prev) => ({
      ...prev,
      persistenceMode: "long",
      persistenceColorMode: "thermal",
    }));

    const boardJson = store.getState().exportBoardConfig();
    const projectJson = store.getState().exportProjectConfig();

    // Mutate to different settings before re-importing
    store.getState().updateScope(1, (prev) => ({
      ...prev,
      persistenceMode: "off",
      persistenceColorMode: "phosphor",
    }));

    await store.getState().importBoardConfig(boardJson);
    assert.equal(store.getState().scopes[1].persistenceMode, "long");
    assert.equal(store.getState().scopes[1].persistenceColorMode, "thermal");

    store.getState().updateScope(1, (prev) => ({
      ...prev,
      persistenceMode: "short",
      persistenceColorMode: "phosphor",
    }));

    await store.getState().importProjectConfig(projectJson);
    assert.equal(store.getState().scopes[1].persistenceMode, "long");
    assert.equal(store.getState().scopes[1].persistenceColorMode, "thermal");

    // Verify legacy project files lacking persistence fields default cleanly
    const parsedLegacy = JSON.parse(projectJson) as {
      board: {
        standaloneScopes: Array<{ config: Record<string, unknown> }>;
        pages?: Array<{ standaloneScopes: Array<{ config: Record<string, unknown> }> }>;
      };
    };
    for (const s of parsedLegacy.board.standaloneScopes) {
      delete s.config.persistenceMode;
      delete s.config.persistenceColorMode;
    }
    for (const pg of parsedLegacy.board.pages ?? []) {
      for (const s of pg.standaloneScopes) {
        delete s.config.persistenceMode;
        delete s.config.persistenceColorMode;
      }
    }

    await store.getState().importProjectConfig(JSON.stringify(parsedLegacy));
    assert.equal(store.getState().scopes[1].persistenceMode, "off");
    assert.equal(store.getState().scopes[1].persistenceColorMode, "phosphor");
  } finally {
    client.stopDataPlane();
  }
});
