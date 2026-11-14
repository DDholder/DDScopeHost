import test from "node:test";
import assert from "node:assert/strict";
import {
  DDSF_MAGIC,
  DdsfBlockKind,
  DdsfFrameBuilder,
  HighFreqTopicBus,
  TriggerRuntimeStateCode,
  ZeroCopyDataBridgeReceiver,
  DDSF_SHM_PROTOCOL_TAG,
  type ChannelLike,
  type DataBridgeModeDto,
  type DdsfScopeView,
  type DdsfSlotReadyDto,
  type WebView2Host,
  type WebView2SharedBufferReceivedEvent,
  type ZeroCopyBridgeHostAdapter,
} from "../src/bridge/index.ts";

interface MockBridgeHarness {
  adapter: ZeroCopyBridgeHostAdapter;
  slotBuffers: ArrayBuffer[];
  releasedBuffers: ArrayBuffer[];
  listenerAddedBeforeAttach: () => boolean;
  emitSharedBuffer: (slot: number, protocol?: string) => void;
  emitSlotReady: (dto: DdsfSlotReadyDto) => void;
  emitRawFrame: (payload: unknown) => void;
}

function createMockHostHarness(options?: {
  emitBuffersOnAttach?: boolean;
  resolvedMode?: DataBridgeModeDto;
}): MockBridgeHarness {
  const emitBuffersOnAttach = options?.emitBuffersOnAttach ?? true;
  const resolvedMode = options?.resolvedMode ?? "web_view2_shared_buffer";

  const slotBuffers = [
    new ArrayBuffer(64 * 1024),
    new ArrayBuffer(64 * 1024),
    new ArrayBuffer(64 * 1024),
  ];
  const releasedBuffers: ArrayBuffer[] = [];
  const listeners = new Set<(ev: WebView2SharedBufferReceivedEvent) => void>();

  let slotChannel: ChannelLike<DdsfSlotReadyDto> | null = null;
  let rawChannel: ChannelLike<unknown> | null = null;
  let listenerPresentAtAttach = false;

  const webview: WebView2Host = {
    addEventListener(type, listener) {
      if (type === "sharedbufferreceived") {
        listeners.add(listener);
      }
    },
    removeEventListener(type, listener) {
      if (type === "sharedbufferreceived") {
        listeners.delete(listener);
      }
    },
    releaseBuffer(buffer) {
      releasedBuffers.push(buffer);
    },
  };

  const emitSharedBuffer = (
    slot: number,
    protocol: string = DDSF_SHM_PROTOCOL_TAG,
  ): void => {
    const ev: WebView2SharedBufferReceivedEvent = {
      getBuffer: () => slotBuffers[slot],
      additionalData: {
        protocol,
        slot,
        capacityBytes: slotBuffers[slot].byteLength,
      },
    };
    for (const cb of listeners) {
      cb(ev);
    }
  };

  const adapter: ZeroCopyBridgeHostAdapter = {
    getWebView2: () => webview,
    createSlotChannel: () => {
      const ch: ChannelLike<DdsfSlotReadyDto> = { onmessage: null };
      slotChannel = ch;
      return ch;
    },
    createRawFrameChannel: () => {
      const ch: ChannelLike<unknown> = { onmessage: null };
      rawChannel = ch;
      return ch;
    },
    async attachDataBridge(preferredMode, _slotCh, _rawCh) {
      listenerPresentAtAttach =
        preferredMode === "raw_ipc_channel" ? true : listeners.size === 1;
      const activeMode =
        preferredMode === "raw_ipc_channel" ? "raw_ipc_channel" : resolvedMode;
      if (activeMode === "web_view2_shared_buffer" && emitBuffersOnAttach) {
        emitSharedBuffer(0);
        emitSharedBuffer(1);
        emitSharedBuffer(2);
      }
      return {
        active_mode: activeMode,
        slot_count: activeMode === "web_view2_shared_buffer" ? 3 : 0,
        slot_bytes: activeMode === "web_view2_shared_buffer" ? 8 * 1024 * 1024 : 0,
      };
    },
    async detachDataBridge() {
      // No-op mock detach
    },
    async getDataBridgeStats() {
      return {
        active_mode: resolvedMode,
        frames_pushed: 0,
        bytes_pushed: 0,
        shm_frames_pushed: 0,
        raw_ipc_frames_pushed: 0,
        oversized_fallbacks: 0,
        send_errors: 0,
        last_frame_seq: 0,
        target_fps: 120,
      };
    },
    async setDataBridgeRate(rateHz) {
      return Math.max(15, Math.min(240, rateHz));
    },
  };

  return {
    adapter,
    slotBuffers,
    releasedBuffers,
    listenerAddedBeforeAttach: () => listenerPresentAtAttach,
    emitSharedBuffer,
    emitSlotReady: (dto) => {
      slotChannel?.onmessage?.(dto);
    },
    emitRawFrame: (payload) => {
      rawChannel?.onmessage?.(payload);
    },
  };
}

function packAnalogTestFrame(frameSeq: number): ArrayBuffer {
  const builder = new DdsfFrameBuilder();
  return builder.packToSlice({
    frameSeq,
    hostNowNs: frameSeq * 8_333_333,
    flags: 0,
    scopes: [
      {
        scopeId: 1,
        triggerState: TriggerRuntimeStateCode.AutoFreerun,
        mode: 1,
        winTStartNs: 0,
        winTSpanNs: 20_000_000,
        subpixelOffsetNs: 0,
        tracks: [
          {
            channelId: 101,
            blockKind: DdsfBlockKind.AnalogRawXY,
            points: [0.25, 3.5, 0.75, -2.5],
          },
        ],
      },
    ],
    snapshots: [],
  });
}

test("WebView2 SharedBuffer zero-copy ingestion preserves slot ArrayBuffer identity", async () => {
  const harness = createMockHostHarness({ emitBuffersOnAttach: true });
  const topicBus = new HighFreqTopicBus();
  const receiver = new ZeroCopyDataBridgeReceiver(topicBus, harness.adapter);

  const scopes: DdsfScopeView[] = [];
  const unsub = topicBus.subscribeScope(1, (scope) => {
    scopes.push(scope);
  });

  const attachInfo = await receiver.attach(null);
  assert.equal(attachInfo.active_mode, "web_view2_shared_buffer");
  assert.equal(harness.listenerAddedBeforeAttach(), true);
  assert.equal(receiver.getReceiverStats().mappedSlots, 3);

  const packed = packAnalogTestFrame(12);
  new Uint8Array(harness.slotBuffers[1]).set(new Uint8Array(packed));

  harness.emitSlotReady({
    slot: 1,
    byte_len: packed.byteLength,
    frame_seq: 12,
  });

  assert.equal(scopes.length, 1);
  const track = scopes[0].tracks[0];
  assert.equal(track.payload.kind, DdsfBlockKind.AnalogRawXY);
  if (track.payload.kind === DdsfBlockKind.AnalogRawXY) {
    assert.equal(
      track.payload.points.buffer,
      harness.slotBuffers[1],
      "Float32Array must view the WebView2 SharedBuffer backing store with zero copy",
    );
    assert.equal(track.payload.points[1], 3.5);
  }

  const stats = receiver.getReceiverStats();
  assert.equal(stats.shmFramesIngested, 1);
  assert.equal(stats.staleSlotDrops, 0);
  assert.equal(topicBus.getMetrics().parseErrors, 0);

  unsub();
  await receiver.detach();
  assert.equal(harness.releasedBuffers.length, 3);
  assert.equal(receiver.getReceiverStats().mappedSlots, 0);
});

test("Early DdsfSlotReadyDto notifications are queued until sharedbufferreceived arrives", async () => {
  const harness = createMockHostHarness({ emitBuffersOnAttach: false });
  const topicBus = new HighFreqTopicBus();
  const receiver = new ZeroCopyDataBridgeReceiver(topicBus, harness.adapter);

  await receiver.attach("web_view2_shared_buffer");
  assert.equal(receiver.getReceiverStats().mappedSlots, 0);

  const packed = packAnalogTestFrame(5);
  new Uint8Array(harness.slotBuffers[0]).set(new Uint8Array(packed));

  // Notification arrives before WebView2 fires `sharedbufferreceived`.
  harness.emitSlotReady({
    slot: 0,
    byte_len: packed.byteLength,
    frame_seq: 5,
  });
  assert.equal(receiver.getReceiverStats().shmFramesIngested, 0);
  assert.equal(topicBus.getMetrics().parseErrors, 0);

  // Buffer arrives and immediately replays queued slot notification.
  harness.emitSharedBuffer(0);
  assert.equal(receiver.getReceiverStats().shmFramesIngested, 1);
  assert.equal(topicBus.getMetrics().frameSeq, 5);
  assert.equal(topicBus.getMetrics().parseErrors, 0);

  await receiver.detach();
});

test("Seqlock guard drops stale or zero-magic slots without parseErrors", async () => {
  const harness = createMockHostHarness({ emitBuffersOnAttach: true });
  const topicBus = new HighFreqTopicBus();
  const receiver = new ZeroCopyDataBridgeReceiver(topicBus, harness.adapter);

  await receiver.attach("web_view2_shared_buffer");

  const packedNew = packAnalogTestFrame(10);
  new Uint8Array(harness.slotBuffers[2]).set(new Uint8Array(packedNew));

  // Stale notification (`frame_seq = 7` while slot 2 holds `frameSeq = 10`).
  harness.emitSlotReady({
    slot: 2,
    byte_len: packedNew.byteLength,
    frame_seq: 7,
  });
  assert.equal(receiver.getReceiverStats().staleSlotDrops, 1);
  assert.equal(receiver.getReceiverStats().shmFramesIngested, 0);
  assert.equal(topicBus.getMetrics().parseErrors, 0);

  // Writer mid-write sentinel (`magic = 0` seqlock lock).
  const dv = new DataView(harness.slotBuffers[2]);
  assert.equal(dv.getUint32(0, true), DDSF_MAGIC);
  dv.setUint32(0, 0, true);
  harness.emitSlotReady({
    slot: 2,
    byte_len: packedNew.byteLength,
    frame_seq: 10,
  });

  assert.equal(receiver.getReceiverStats().staleSlotDrops, 2);
  assert.equal(receiver.getReceiverStats().shmFramesIngested, 0);
  assert.equal(topicBus.getMetrics().parseErrors, 0);

  await receiver.detach();
});

test("RawIpcChannel fallback ingests ArrayBuffer and Uint8Array views", async () => {
  const harness = createMockHostHarness({ resolvedMode: "raw_ipc_channel" });
  const topicBus = new HighFreqTopicBus();
  const receiver = new ZeroCopyDataBridgeReceiver(topicBus, harness.adapter);

  const attachInfo = await receiver.attach("raw_ipc_channel");
  assert.equal(attachInfo.active_mode, "raw_ipc_channel");

  const frame1 = packAnalogTestFrame(1);
  harness.emitRawFrame(frame1);

  const frame2 = packAnalogTestFrame(2);
  const offsetStorage = new Uint8Array(frame2.byteLength + 16);
  offsetStorage.set(new Uint8Array(frame2), 8);
  const sliceView = offsetStorage.subarray(8, 8 + frame2.byteLength);
  harness.emitRawFrame(sliceView);

  const stats = receiver.getReceiverStats();
  assert.equal(stats.rawFramesIngested, 2);
  assert.equal(stats.shmFramesIngested, 0);
  assert.equal(topicBus.getMetrics().frameSeq, 2);
  assert.equal(topicBus.getMetrics().parseErrors, 0);

  await receiver.detach();
});
