import test from "node:test";
import assert from "node:assert/strict";
import { parseDdsfFrame } from "../src/bridge/ddsf-parser.ts";
import {
  DdsfBlockKind,
  DDSF_M4_COLUMN_FLOATS,
  TriggerRuntimeStateCode,
} from "../src/bridge/ddsf-spec.ts";
import { MockSimulationCore } from "../src/bridge/mock-ddsf-core.ts";

function setupDefaultScopes(core: MockSimulationCore): {
  chIa: number;
  chVbus: number;
  chPwmA: number;
  chState: number;
} {
  const tree = core.getChannelTree();
  const sim0 = tree.sources.find((s) => s.source.source_id === "sim0");
  assert.ok(sim0, "default sim0 source should exist");
  const byPath = new Map(sim0.channels.map((c) => [c.local_path, c.channel_id]));
  const chIa = byPath.get("ia")!;
  const chVbus = byPath.get("v_bus")!;
  const chPwmA = byPath.get("pwm_a")!;
  const chState = byPath.get("ctrl_state")!;

  core.upsertScopeViewport({
    scope_id: 1,
    title: "Scope 1",
    canvas_width_px: 800,
    time_span_ns: 20_000_000, // 20 ms -> M4 compression
    pre_trigger_ratio: 0.25,
    pan_offset_ns: 0,
    sweep_mode: "roll",
    auto_timeout_ms: 120,
    trigger: {
      kind: "edge",
      channel_id: chIa,
      level: 0,
      hysteresis: 0.5,
      slope: "rising",
    },
    tracks: [
      { channel_id: chIa, lane: "analog", visible: true, y_scale: 5, y_offset: 0, color_rgba: 0x38bdf8ff },
      { channel_id: chVbus, lane: "analog", visible: true, y_scale: 25, y_offset: 50, color_rgba: 0xf59e0bff },
      { channel_id: chPwmA, lane: "digital", visible: true, y_scale: 1, y_offset: 0, color_rgba: 0x22d3eeff },
      { channel_id: chState, lane: "state_ribbon", visible: true, y_scale: 1, y_offset: 0, color_rgba: 0x10b981ff },
    ],
    sync_group_id: null,
    paused: false,
  });

  core.upsertScopeViewport({
    scope_id: 2,
    title: "Scope 2",
    canvas_width_px: 800,
    time_span_ns: 10_000_000,
    pre_trigger_ratio: 0.25,
    pan_offset_ns: 0,
    sweep_mode: "single",
    auto_timeout_ms: 120,
    trigger: {
      kind: "edge",
      channel_id: chVbus,
      level: 85.0,
      hysteresis: 2.0,
      slope: "rising",
    },
    tracks: [
      { channel_id: chVbus, lane: "analog", visible: true, y_scale: 25, y_offset: 50, color_rgba: 0xf59e0bff },
      { channel_id: chState, lane: "state_ribbon", visible: true, y_scale: 1, y_offset: 0, color_rgba: 0x10b981ff },
    ],
    sync_group_id: null,
    paused: false,
  });

  return { chIa, chVbus, chPwmA, chState };
}

test("MockSimulationCore 120Hz step + 1-sample +80V glitch retention in M4 & RawXY + Single latch/rearm", () => {
  const core = new MockSimulationCore();
  const { chVbus } = setupDefaultScopes(core);

  // Advance a couple frames before injecting manual glitch
  core.step(8.33, 1000);
  const glitchTargetNs = core.injectGlitch();

  // Step ~2ms past glitchTargetNs so glitch falls inside Scope 1 Roll & Scope 2 Single windows
  let latestFrame = parseDdsfFrame(core.step(8.33, 1009).frameBuffer);
  while (core.getNowNs() < glitchTargetNs + 2_000_000) {
    latestFrame = parseDdsfFrame(core.step(8.33, 1020).frameBuffer);
  }

  assert.equal(latestFrame.scopes.length, 2);
  assert.ok(latestFrame.snapshots.length >= 8);

  // 1. Scope 1 (Roll, 20ms span): should use M4 columns and preserve >115V spike in v_max
  const s1 = latestFrame.scopes.find((s) => s.scopeId === 1)!;
  const s1Vbus = s1.tracks.find((t) => t.channelId === chVbus)!;
  assert.equal(s1Vbus.payload.kind, DdsfBlockKind.AnalogM4Column);
  if (s1Vbus.payload.kind === DdsfBlockKind.AnalogM4Column) {
    let maxSeen = -Infinity;
    for (let i = 0; i < s1Vbus.elementCount; i += 1) {
      const vMax = s1Vbus.payload.columns[i * DDSF_M4_COLUMN_FLOATS + 3];
      if (vMax > maxSeen) maxSeen = vMax;
    }
    assert.ok(maxSeen > 115, `expected M4 v_max > 115V for 128V glitch, got ${maxSeen}`);
  }

  // 2. Scope 2 (Single, trig v_bus > 85V): should latch & pin buffer
  const s2Latched = latestFrame.scopes.find((s) => s.scopeId === 2)!;
  assert.equal(s2Latched.triggerState, TriggerRuntimeStateCode.LatchedPinned);
  assert.ok(core.getChannelTree().pinned_bytes > 0);
  const latchedStartNs = s2Latched.winTStartNs;

  // Advancing further in Roll must NOT shift Scope 2's latched trigger window
  for (let i = 0; i < 6; i += 1) {
    latestFrame = parseDdsfFrame(core.step(8.33, 1030 + i * 9).frameBuffer);
  }
  const s2StillLatched = latestFrame.scopes.find((s) => s.scopeId === 2)!;
  assert.equal(s2StillLatched.triggerState, TriggerRuntimeStateCode.LatchedPinned);
  assert.equal(s2StillLatched.winTStartNs, latchedStartNs);

  // 3. Zoom latched Scope 2 deeply (2 us span = 10,000x zoom vs 20 ms) -> switches to RawXY preserving 128V peak
  const zoomedSpanNs = 2_000;
  core.upsertScopeViewport({
    scope_id: 2,
    title: "Scope 2 Zoomed",
    canvas_width_px: 800,
    time_span_ns: zoomedSpanNs,
    pre_trigger_ratio: 0.25,
    pan_offset_ns: 0,
    sweep_mode: "single",
    auto_timeout_ms: 120,
    trigger: {
      kind: "edge",
      channel_id: chVbus,
      level: 85.0,
      hysteresis: 2.0,
      slope: "rising",
    },
    tracks: [
      { channel_id: chVbus, lane: "analog", visible: true, y_scale: 25, y_offset: 50, color_rgba: 0xf59e0bff },
    ],
    sync_group_id: null,
    paused: false,
  });

  const zoomedFrame = parseDdsfFrame(core.step(8.33, 1090).frameBuffer);
  const s2Zoomed = zoomedFrame.scopes.find((s) => s.scopeId === 2)!;
  const s2Raw = s2Zoomed.tracks.find((t) => t.channelId === chVbus)!;
  assert.equal(s2Raw.payload.kind, DdsfBlockKind.AnalogRawXY);
  if (s2Raw.payload.kind === DdsfBlockKind.AnalogRawXY) {
    let rawMax = -Infinity;
    for (let i = 0; i < s2Raw.elementCount; i += 1) {
      const y = s2Raw.payload.points[i * 2 + 1];
      if (y > rawMax) rawMax = y;
    }
    assert.ok(rawMax > 115, `expected zoomed RawXY peak > 115V, got ${rawMax}`);
  }

  // 4. Rearm Scope 2 -> releases pinned bytes and enters Armed state
  core.rearmScope(2);
  const postRearm = parseDdsfFrame(core.step(8.33, 1100).frameBuffer);
  const s2Armed = postRearm.scopes.find((s) => s.scopeId === 2)!;
  assert.equal(s2Armed.triggerState, TriggerRuntimeStateCode.Armed);
  assert.equal(core.getChannelTree().pinned_bytes, 0);
});

test("MockSimulationCore SyncGroup broadcast and TxCoalescer latest-value-wins", () => {
  const core = new MockSimulationCore();
  const { chIa, chVbus } = setupDefaultScopes(core);

  core.upsertScopeViewport({
    scope_id: 1,
    title: "S1",
    canvas_width_px: 800,
    time_span_ns: 20_000_000,
    pre_trigger_ratio: 0.25,
    pan_offset_ns: 0,
    sweep_mode: "roll",
    auto_timeout_ms: 120,
    trigger: null,
    tracks: [{ channel_id: chIa, lane: "analog", visible: true, y_scale: 5, y_offset: 0, color_rgba: 0x38bdf8ff }],
    sync_group_id: 1,
    paused: false,
  });
  core.upsertScopeViewport({
    scope_id: 2,
    title: "S2",
    canvas_width_px: 800,
    time_span_ns: 10_000_000,
    pre_trigger_ratio: 0.25,
    pan_offset_ns: 0,
    sweep_mode: "roll",
    auto_timeout_ms: 120,
    trigger: null,
    tracks: [{ channel_id: chVbus, lane: "analog", visible: true, y_scale: 25, y_offset: 50, color_rgba: 0xf59e0bff }],
    sync_group_id: 1,
    paused: false,
  });

  core.updateSyncGroup({
    group_id: 1,
    origin_scope_id: 1,
    time_span_ns: 5_000_000,
    center_offset_ns: -250_000,
    cursor_a_ratio: null,
    cursor_b_ratio: null,
  });

  const frame = parseDdsfFrame(core.step(8.33, 2000).frameBuffer);
  const s2 = frame.scopes.find((s) => s.scopeId === 2)!;
  assert.equal(s2.winTSpanNs, 5_000_000);

  // Coalescer: 5 rapid normal keyed sends at t=5000ms (< 90ms drain window)
  const r1 = core.sendDownlink(
    {
      source_id: "sim0",
      priority: "normal",
      coalesce_key: "sim0:kp",
      payload: { kind: "key_value", key: "kp", value: 1.5 },
    },
    5000,
  );
  assert.equal(r1.coalesced_existing, false);

  for (let i = 0; i < 4; i += 1) {
    const rN = core.sendDownlink(
      {
        source_id: "sim0",
        priority: "normal",
        coalesce_key: "sim0:kp",
        payload: { kind: "key_value", key: "kp", value: 2.0 + i * 0.25 },
      },
      5005 + i * 2,
    );
    assert.equal(rN.coalesced_existing, true);
    assert.equal(rN.result_kind, "coalesced_in_place");
  }

  const stats = core.getDownlinkStats();
  assert.equal(stats.pendingQueueDepth, 1);
  assert.equal(stats.coalescedOverwrites, 4);
  assert.equal(stats.motorKp, 2.75);

  // High priority bypasses queue immediately
  const rHigh = core.sendDownlink(
    {
      source_id: "sim0",
      priority: "high",
      coalesce_key: "sim0:kp",
      payload: { kind: "key_value", key: "kp", value: 3.25 },
    },
    5020,
  );
  assert.equal(rHigh.coalesced_existing, false);
  assert.equal(rHigh.result_kind, "dispatched_high_priority");

  // Draining after >= 90ms clears queue
  const drained = core.drainCoalescedQueue(5200);
  assert.equal(drained, 1);
  assert.equal(core.getDownlinkStats().pendingQueueDepth, 0);
});
