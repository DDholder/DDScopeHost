import { describe, it } from "node:test";
import assert from "node:assert/strict";
import {
  BridgeClient,
  HighFreqTopicBus,
  type ChannelLike,
  type ChannelNodeDto_Serialize,
  type ChannelTreeSnapshotDto_Serialize,
  type ControlPlaneCommands,
  type ControlPlaneHostAdapter,
  type DataBridgeAttachDto,
  type DataBridgeModeDto,
  type DataBridgeStatsDto_Serialize,
  type DdsfSlotReadyDto,
  type DownlinkReceiptDto,
  type ScopeViewportConfigDto_Deserialize,
  type SendDownlinkRequestDto_Deserialize,
  type SyncGroupUpdateDto_Deserialize,
  type WorkspaceTelemetryEvent,
  type ZeroCopyBridgeHostAdapter,
} from "../src/bridge/index.ts";
import { createWorkspaceStore } from "../src/workspace/store.ts";

interface FakeHostHarness {
  adapter: ControlPlaneHostAdapter;
  attachCalls: number;
  rateCalls: number[];
  listenCalls: number;
  downlinkCalls: SendDownlinkRequestDto_Deserialize[];
  upsertedViewports: ScopeViewportConfigDto_Deserialize[];
  syncGroupUpdates: SyncGroupUpdateDto_Deserialize[];
  setChannelTree: (tree: ChannelTreeSnapshotDto_Serialize) => void;
  emitTelemetry: (evt: WorkspaceTelemetryEvent) => void;
}

function createFakeTauriHostHarness(initialTree?: ChannelTreeSnapshotDto_Serialize): FakeHostHarness {
  let channelTree: ChannelTreeSnapshotDto_Serialize = initialTree ?? {
    sources: [],
    quota_limit_bytes: 2 * 1024 * 1024 * 1024,
    quota_used_bytes: 0,
    pinned_bytes: 0,
  };
  let telemetryHandler: ((payload: WorkspaceTelemetryEvent) => void) | null = null;
  let attachCalls = 0;
  const rateCalls: number[] = [];
  let listenCalls = 0;
  const downlinkCalls: SendDownlinkRequestDto_Deserialize[] = [];
  const upsertedViewports: ScopeViewportConfigDto_Deserialize[] = [];
  const syncGroupUpdates: SyncGroupUpdateDto_Deserialize[] = [];

  const zeroCopyHostAdapter: ZeroCopyBridgeHostAdapter = {
    getWebView2: () => null,
    createSlotChannel: (): ChannelLike<DdsfSlotReadyDto> => ({ onmessage: null }),
    createRawFrameChannel: (): ChannelLike<unknown> => ({ onmessage: null }),
    attachDataBridge: async (): Promise<DataBridgeAttachDto> => {
      attachCalls += 1;
      return {
        active_mode: "raw_ipc_channel" as DataBridgeModeDto,
        slot_count: 3,
        slot_bytes: 65_536,
      };
    },
    detachDataBridge: async () => {},
    getDataBridgeStats: async (): Promise<DataBridgeStatsDto_Serialize> => ({
      active_mode: "raw_ipc_channel",
      frames_pushed: 0,
      bytes_pushed: 0,
      shm_frames_pushed: 0,
      raw_ipc_frames_pushed: 0,
      oversized_fallbacks: 0,
      send_errors: 0,
      last_frame_seq: 0,
      target_fps: rateCalls.at(-1) ?? 120,
    }),
    setDataBridgeRate: async (rateHz: number) => {
      rateCalls.push(rateHz);
      return rateHz;
    },
  };

  const commands: ControlPlaneCommands = {
    getEngineInfo: async () => ({
      core_name: "ddscope-core",
      core_version: "0.1.0",
      bridge_ready: true,
      ddsf_version: 1,
    }),
    listSerialPorts: async () => [],
    createSource: async () => {
      throw new Error("unused in unit harness");
    },
    startSource: async (sourceId) => ({
      source_id: sourceId,
      display_name: sourceId,
      transport_kind: "sim",
      state: "running",
      raw_recording_path: null,
    }),
    stopSource: async (sourceId) => ({
      source_id: sourceId,
      display_name: sourceId,
      transport_kind: "sim",
      state: "stopped",
      raw_recording_path: null,
    }),
    removeSource: async () => null,
    seekReplaySource: async () => null,
    setRawRecorder: async (req) => ({
      source_id: req.source_id,
      display_name: req.source_id,
      transport_kind: "sim",
      state: "running",
      raw_recording_path: req.enabled ? (req.output_path ?? null) : null,
    }),
    getChannelTree: async () => channelTree,
    upsertScopeViewport: async (cfg) => {
      upsertedViewports.push(cfg);
      return {
        scope_id: cfg.scope_id,
        active_track_count: cfg.tracks.filter((t) => t.visible).length,
        sync_group_id: cfg.sync_group_id ?? null,
      };
    },
    removeScopeViewport: async () => null,
    rearmScope: async () => null,
    updateSyncGroup: async (update) => {
      syncGroupUpdates.push(update);
      return null;
    },
    sendDownlink: async (req): Promise<DownlinkReceiptDto> => {
      downlinkCalls.push(req);
      return {
        accepted: true,
        coalesced_existing: false,
        result_kind: "sent_fire_and_forget",
      };
    },
  };

  const adapter: ControlPlaneHostAdapter = {
    isTauriHost: () => true,
    commands,
    listenTelemetry: async (handler) => {
      listenCalls += 1;
      telemetryHandler = handler;
      return () => {
        if (telemetryHandler === handler) {
          telemetryHandler = null;
        }
      };
    },
    zeroCopyHostAdapter,
  };

  return {
    adapter,
    get attachCalls() {
      return attachCalls;
    },
    rateCalls,
    get listenCalls() {
      return listenCalls;
    },
    downlinkCalls,
    upsertedViewports,
    syncGroupUpdates,
    setChannelTree: (tree) => {
      channelTree = tree;
    },
    emitTelemetry: (evt) => {
      telemetryHandler?.(evt);
    },
  };
}

function makeSimPresetTree(): ChannelTreeSnapshotDto_Serialize {
  const ch = (
    channel_id: number,
    local_path: string,
    modality: ChannelNodeDto_Serialize["modality"],
    scalar_type: string | null,
    unit: string | null,
  ): ChannelNodeDto_Serialize => ({
    channel_id,
    source_id: "sim0",
    local_path,
    full_path: `sim0/${local_path}`,
    display_name: local_path,
    unit,
    modality,
    scalar_type,
    state_labels:
      modality === "state_machine"
        ? [
            { code: 0, name: "IDLE", color_rgba: 0x64748bff },
            { code: 1, name: "RUN", color_rgba: 0x10b981ff },
            { code: 3, name: "FAULT", color_rgba: 0xef4444ff },
          ]
        : [],
  });

  return {
    sources: [
      {
        source: {
          source_id: "sim0",
          display_name: "Simulated FOC Motor (2 MHz)",
          transport_kind: "sim",
          state: "running",
          raw_recording_path: null,
        },
        channels: [
          ch(1, "ch0", "scalar", "f32", "A"),
          ch(2, "ch1", "scalar", "f32", "A"),
          ch(3, "ch2", "scalar", "f32", "A"),
          ch(4, "ch3", "scalar", "f32", "V"),
          ch(5, "pwm_a", "digital_bool", "bool", null),
          ch(6, "pwm_b", "digital_bool", "bool", null),
          ch(7, "fault_pin", "digital_bool", "bool", null),
          ch(8, "ctrl_state", "state_machine", "i32", null),
        ],
      },
    ],
    quota_limit_bytes: 2 * 1024 * 1024 * 1024,
    quota_used_bytes: 0,
    pinned_bytes: 0,
  };
}

describe("M4 BridgeClient & WorkspaceStore live Tauri wiring", () => {
  it("startDataPlane(120) handshakes without mock worker and getChannelTree preserves empty sources", async () => {
    const bus = new HighFreqTopicBus();
    const harness = createFakeTauriHostHarness();
    const client = new BridgeClient(bus, harness.adapter);

    let telemetryCount = 0;
    const unsub = client.subscribeTelemetry(() => {
      telemetryCount += 1;
    });

    await client.startDataPlane(120);
    assert.equal(client.isMockMode(), false);
    assert.equal(harness.attachCalls, 1);
    assert.deepEqual(harness.rateCalls, [120]);
    assert.equal(harness.listenCalls, 1);
    assert.equal(bus.getMetrics().frameSeq, 0);

    const tree = await client.getChannelTree();
    assert.deepEqual(tree.sources, []);

    harness.emitTelemetry({
      timestamp_ms: 1_000,
      sources: [],
      quota_used_bytes: 0,
      quota_limit_bytes: 2 * 1024 * 1024 * 1024,
      pinned_bytes: 0,
    });
    assert.equal(telemetryCount, 1);

    unsub();
    client.stopDataPlane();
  });

  it("coalesces 10x synchronous key_value burst, preserves motorKp across telemetry events, and injects GLITCH", async () => {
    const bus = new HighFreqTopicBus();
    const harness = createFakeTauriHostHarness(makeSimPresetTree());
    const client = new BridgeClient(bus, harness.adapter);

    let lastStatsKp = -1;
    const unsub = client.subscribeTelemetry((_evt, stats) => {
      lastStatsKp = stats.motorKp;
    });

    await client.startDataPlane(120);

    const promises: Array<Promise<DownlinkReceiptDto>> = [];
    for (let i = 1; i <= 10; i += 1) {
      promises.push(
        client.sendDownlink({
          source_id: "sim0",
          payload: {
            kind: "key_value",
            key: "kp",
            value: Number((1.0 + i * 0.1).toFixed(2)),
          },
          priority: "normal",
          coalesce_key: "param:kp",
        }),
      );
    }

    const receipts = await Promise.all(promises);
    assert.equal(harness.downlinkCalls.length, 1);
    assert.deepEqual(harness.downlinkCalls[0].payload, {
      kind: "key_value",
      key: "kp",
      value: 2.0,
    });

    for (let i = 0; i < 9; i += 1) {
      assert.equal(receipts[i].accepted, true);
      assert.equal(receipts[i].coalesced_existing, true);
    }
    assert.equal(receipts[9].accepted, true);
    assert.equal(receipts[9].coalesced_existing, false);

    const stats = client.getDownlinkStats();
    assert.equal(stats.coalescedOverwrites, 9);
    assert.equal(stats.totalDispatched, 1);
    assert.equal(stats.motorKp, 2.0);

    harness.emitTelemetry({
      timestamp_ms: 2_000,
      sources: [],
      quota_used_bytes: 4096,
      quota_limit_bytes: 2 * 1024 * 1024 * 1024,
      pinned_bytes: 0,
    });
    assert.equal(lastStatsKp, 2.0);
    assert.equal(client.getDownlinkStats().motorKp, 2.0);

    await client.injectGlitch(["sim0"]);
    assert.equal(harness.downlinkCalls.length, 2);
    assert.deepEqual(harness.downlinkCalls[1], {
      source_id: "sim0",
      payload: {
        kind: "ascii_text",
        text: "GLITCH",
        append_newline: true,
      },
      priority: "high",
      coalesce_key: null,
    });

    unsub();
    client.stopDataPlane();
  });

  it("createWorkspaceStore binds live SimPresetCodec ch0..ch3 IDs 1..8 and sends finite SyncGroup cursor patches", async () => {
    const bus = new HighFreqTopicBus();
    const harness = createFakeTauriHostHarness(makeSimPresetTree());
    const client = new BridgeClient(bus, harness.adapter);

    const store = createWorkspaceStore(client, async () => ({
      mode: "tauri",
      engine: await harness.adapter.commands.getEngineInfo(),
      webgpuAvailable: false,
    }));

    await store.getState().initWorkspace();
    const state = store.getState();
    assert.equal(state.initialized, true);
    assert.equal(state.runtimeStatus?.mode, "tauri");

    const scope1 = state.scopes[1];
    const scope2 = state.scopes[2];
    assert.ok(scope1);
    assert.ok(scope2);
    assert.deepEqual(
      scope1.tracks.map((t) => t.channelId),
      [1, 2, 4, 5, 6, 7, 8],
    );
    assert.deepEqual(
      scope2.tracks.map((t) => t.channelId),
      [4, 7, 8],
    );
    assert.equal(scope2.sweepMode, "single");
    assert.deepEqual(scope2.trigger, {
      kind: "edge",
      channel_id: 4,
      level: 85,
      hysteresis: 2.0,
      slope: "rising",
    });
    assert.equal(harness.upsertedViewports.length, 2);

    store.getState().updateScope(1, (prev) => ({ ...prev, syncGroupId: 1 }));
    store.getState().updateScope(2, (prev) => ({ ...prev, syncGroupId: 1 }));

    harness.syncGroupUpdates.length = 0;
    store.getState().broadcastSyncGroup(1, 1, {
      cursorsEnabled: true,
      cursorARatio: 0.25,
      cursorBRatio: 0.75,
    });

    const syncedScope2 = store.getState().scopes[2];
    assert.equal(syncedScope2.cursorsEnabled, true);
    assert.equal(syncedScope2.cursorARatio, 0.25);
    assert.equal(syncedScope2.cursorBRatio, 0.75);

    assert.equal(harness.syncGroupUpdates.length, 1);
    const updateDto = harness.syncGroupUpdates[0];
    assert.equal(Number.isFinite(updateDto.time_span_ns), true);
    assert.equal(Number.isFinite(updateDto.center_offset_ns), true);
    assert.equal(updateDto.cursor_a_ratio, 0.25);
    assert.equal(updateDto.cursor_b_ratio, 0.75);

    client.stopDataPlane();
  });
});
