import type {
  ChannelTreeSnapshotDto_Serialize,
  CreateSourceRequestDto_Deserialize,
  DownlinkReceiptDto,
  RawRecorderRequestDto_Deserialize,
  ScopeViewportConfigDto_Deserialize,
  ScopeViewportSummaryDto_Serialize,
  SeekReplayRequestDto_Deserialize,
  SendDownlinkRequestDto_Deserialize,
  SerialPortInfoDto_Serialize,
  SourceSummaryDto_Serialize,
  SyncGroupUpdateDto_Deserialize,
  WorkspaceTelemetryEvent,
} from "./bindings.ts";
import { MockSimulationCore } from "./mock-ddsf-core.ts";
import { globalTopicBus, type HighFreqTopicBus } from "./topic-bus.ts";

export interface MockDownlinkStats {
  pendingQueueDepth: number;
  totalDispatched: number;
  coalescedOverwrites: number;
  motorKp: number;
  motorFreqHz: number;
}

type WorkerOutboundMessage =
  | {
      type: "frame";
      frameBuffer: ArrayBuffer;
      telemetry: WorkspaceTelemetryEvent;
      downlinkStats: MockDownlinkStats;
    }
  | {
      type: "rpcResult";
      requestId: number;
      ok: boolean;
      result?: unknown;
      error?: string;
    };

export type TelemetryListener = (
  telemetry: WorkspaceTelemetryEvent,
  downlinkStats: MockDownlinkStats,
) => void;

/**
 * Browser-side 60/120Hz `DDSF v1` simulation controller backed by a dedicated Web Worker
 * (with automatic main-thread fallback when Workers are unavailable).
 */
export class MockDdsfController {
  private readonly bus: HighFreqTopicBus;
  private worker: Worker | null = null;
  private fallbackCore: MockSimulationCore | null = null;
  private fallbackTimer: ReturnType<typeof setInterval> | null = null;
  private fallbackLastMs = 0;

  private readonly pendingRpcs = new Map<
    number,
    { resolve: (value: unknown) => void; reject: (err: Error) => void }
  >();
  private readonly telemetryListeners = new Set<TelemetryListener>();
  private nextRequestId = 1;
  private lastTelemetryEmitMs = 0;
  private rateHz = 120;
  private running = false;
  private latestDownlinkStats: MockDownlinkStats = {
    pendingQueueDepth: 0,
    totalDispatched: 0,
    coalescedOverwrites: 0,
    motorKp: 1.0,
    motorFreqHz: 50,
  };

  constructor(bus: HighFreqTopicBus = globalTopicBus) {
    this.bus = bus;
  }

  start(rateHz = 120): void {
    this.rateHz = rateHz;
    if (this.running) {
      this.setRate(rateHz);
      return;
    }
    this.running = true;

    if (typeof Worker !== "undefined" && !this.worker && !this.fallbackCore) {
      try {
        const worker = new Worker(new URL("./mock-ddsf.worker.ts", import.meta.url), {
          type: "module",
        });
        worker.onmessage = (ev: MessageEvent<WorkerOutboundMessage>) => {
          this.handleWorkerMessage(ev.data);
        };
        worker.onerror = () => {
          this.activateFallback();
        };
        this.worker = worker;
        this.worker.postMessage({ type: "start", rateHz: this.rateHz });
        return;
      } catch {
        this.activateFallback();
        return;
      }
    }

    if (this.worker) {
      this.worker.postMessage({ type: "start", rateHz: this.rateHz });
    } else {
      this.activateFallback();
    }
  }

  stop(): void {
    this.running = false;
    if (this.worker) {
      this.worker.postMessage({ type: "stop" });
    }
    if (this.fallbackTimer !== null) {
      clearInterval(this.fallbackTimer);
      this.fallbackTimer = null;
    }
  }

  dispose(): void {
    this.stop();
    if (this.worker) {
      this.worker.terminate();
      this.worker = null;
    }
    for (const pending of this.pendingRpcs.values()) {
      pending.reject(new Error("MockDdsfController disposed"));
    }
    this.pendingRpcs.clear();
    this.telemetryListeners.clear();
  }

  setRate(rateHz: number): void {
    this.rateHz = Math.max(15, Math.min(240, rateHz));
    if (this.worker) {
      this.worker.postMessage({ type: "setRate", rateHz: this.rateHz });
    } else if (this.fallbackCore && this.running) {
      this.restartFallbackTimer();
    }
  }

  getDownlinkStats(): MockDownlinkStats {
    if (this.fallbackCore) {
      return this.fallbackCore.getDownlinkStats();
    }
    return { ...this.latestDownlinkStats };
  }

  subscribeTelemetry(listener: TelemetryListener): () => void {
    this.telemetryListeners.add(listener);
    return () => {
      this.telemetryListeners.delete(listener);
    };
  }

  private activateFallback(): void {
    if (this.worker) {
      this.worker.terminate();
      this.worker = null;
    }
    if (!this.fallbackCore) {
      this.fallbackCore = new MockSimulationCore({ autoStartDefaultSim: false });
    }
    if (this.running) {
      this.restartFallbackTimer();
    }
  }

  private restartFallbackTimer(): void {
    if (this.fallbackTimer !== null) {
      clearInterval(this.fallbackTimer);
      this.fallbackTimer = null;
    }
    const intervalMs = Math.max(8, Math.round(1000 / this.rateHz));
    this.fallbackLastMs = performance.now();
    this.fallbackTimer = setInterval(() => {
      if (!this.fallbackCore) return;
      const now = performance.now();
      const dtMs = Math.max(1, Math.min(50, now - this.fallbackLastMs));
      this.fallbackLastMs = now;
      const res = this.fallbackCore.step(dtMs, Date.now());
      this.onFrameProduced(
        res.frameBuffer,
        res.telemetry,
        this.fallbackCore.getDownlinkStats(),
      );
    }, intervalMs);
  }

  private handleWorkerMessage(msg: WorkerOutboundMessage): void {
    if (msg.type === "frame") {
      this.onFrameProduced(msg.frameBuffer, msg.telemetry, msg.downlinkStats);
      return;
    }
    if (msg.type === "rpcResult") {
      const entry = this.pendingRpcs.get(msg.requestId);
      if (!entry) return;
      this.pendingRpcs.delete(msg.requestId);
      if (msg.ok) {
        entry.resolve(msg.result);
      } else {
        entry.reject(new Error(msg.error ?? "Mock worker RPC failed"));
      }
    }
  }

  private onFrameProduced(
    frameBuffer: ArrayBuffer,
    telemetry: WorkspaceTelemetryEvent,
    downlinkStats: MockDownlinkStats,
  ): void {
    this.latestDownlinkStats = { ...downlinkStats };
    this.bus.ingestFrame(frameBuffer);

    const nowMs = typeof performance !== "undefined" ? performance.now() : Date.now();
    if (nowMs - this.lastTelemetryEmitMs >= 140) {
      this.lastTelemetryEmitMs = nowMs;
      this.telemetryListeners.forEach((cb) => {
        cb(telemetry, downlinkStats);
      });
    }
  }

  private callRpc<T>(method: string, ...args: unknown[]): Promise<T> {
    if (this.worker) {
      const requestId = this.nextRequestId;
      this.nextRequestId += 1;
      return new Promise<T>((resolve, reject) => {
        this.pendingRpcs.set(requestId, {
          resolve: resolve as (v: unknown) => void,
          reject,
        });
        this.worker?.postMessage({
          type: "rpc",
          requestId,
          method,
          args,
        });
      });
    }

    if (!this.fallbackCore) {
      this.fallbackCore = new MockSimulationCore({ autoStartDefaultSim: false });
    }
    try {
      const target = this.fallbackCore as unknown as Record<
        string,
        (...params: unknown[]) => unknown
      >;
      const fn = target[method];
      const result = fn.apply(this.fallbackCore, args);
      return Promise.resolve(result as T);
    } catch (err: unknown) {
      return Promise.reject(err instanceof Error ? err : new Error(String(err)));
    }
  }

  listSerialPorts(): Promise<SerialPortInfoDto_Serialize[]> {
    return Promise.resolve([
      { port_name: "COM3 (Sim USB-CDC 12Mbps)", description: "Virtual FOC Controller", vid: 0x0483, pid: 0x5740 },
      { port_name: "COM7 (Mock UART-TTL)", description: "Telemetry Auxiliary UART", vid: 0x10c4, pid: 0xea60 },
    ]);
  }

  createSource(config: CreateSourceRequestDto_Deserialize): Promise<SourceSummaryDto_Serialize> {
    return this.callRpc<SourceSummaryDto_Serialize>("createSource", config);
  }

  startSource(sourceId: string): Promise<SourceSummaryDto_Serialize> {
    return this.callRpc<SourceSummaryDto_Serialize>("startSource", sourceId);
  }

  stopSource(sourceId: string): Promise<SourceSummaryDto_Serialize> {
    return this.callRpc<SourceSummaryDto_Serialize>("stopSource", sourceId);
  }

  removeSource(sourceId: string): Promise<null> {
    return this.callRpc<void>("removeSource", sourceId).then(() => null);
  }

  seekReplaySource(req: SeekReplayRequestDto_Deserialize): Promise<null> {
    return this.callRpc<void>("seekReplaySource", req).then(() => null);
  }

  setRawRecorder(req: RawRecorderRequestDto_Deserialize): Promise<SourceSummaryDto_Serialize> {
    return this.callRpc<SourceSummaryDto_Serialize>("setRawRecorder", req);
  }

  getChannelTree(): Promise<ChannelTreeSnapshotDto_Serialize> {
    return this.callRpc<ChannelTreeSnapshotDto_Serialize>("getChannelTree");
  }

  upsertScopeViewport(
    config: ScopeViewportConfigDto_Deserialize,
  ): Promise<ScopeViewportSummaryDto_Serialize> {
    return this.callRpc<ScopeViewportSummaryDto_Serialize>("upsertScopeViewport", config);
  }

  removeScopeViewport(scopeId: number): Promise<null> {
    return this.callRpc<void>("removeScopeViewport", scopeId).then(() => null);
  }

  rearmScope(scopeId: number): Promise<null> {
    return this.callRpc<void>("rearmScope", scopeId).then(() => null);
  }

  updateSyncGroup(update: SyncGroupUpdateDto_Deserialize): Promise<null> {
    return this.callRpc<void>("updateSyncGroup", update).then(() => null);
  }

  sendDownlink(req: SendDownlinkRequestDto_Deserialize): Promise<DownlinkReceiptDto> {
    return this.callRpc<DownlinkReceiptDto>("sendDownlink", req, Date.now());
  }

  injectGlitch(): Promise<number> {
    return this.callRpc<number>("injectGlitch");
  }
}
