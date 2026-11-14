import {
  commands,
  events,
  type ChannelTreeSnapshotDto_Serialize,
  type CreateSourceRequestDto_Deserialize,
  type DownlinkReceiptDto,
  type EngineStatusDto,
  type RawRecorderRequestDto_Deserialize,
  type ScopeViewportConfigDto_Deserialize,
  type ScopeViewportSummaryDto_Serialize,
  type SeekReplayRequestDto_Deserialize,
  type SendDownlinkRequestDto_Deserialize,
  type SerialPortInfoDto_Serialize,
  type SourceSummaryDto_Serialize,
  type SyncGroupUpdateDto_Deserialize,
  type WorkspaceTelemetryEvent,
} from "./bindings.ts";
import { DDSF_VERSION_V1 } from "./ddsf-spec.ts";
import {
  MockDdsfController,
  type MockDownlinkStats,
  type TelemetryListener,
} from "./mock-ddsf.ts";
import { globalTopicBus, type HighFreqTopicBus } from "./topic-bus.ts";

export type HostRuntimeMode = "tauri" | "browser-mock";

export interface RuntimeStatus {
  mode: HostRuntimeMode;
  engine: EngineStatusDto;
  webgpuAvailable: boolean;
}

/**
 * Checks whether the frontend is hosted inside a Tauri WebView or a standalone browser.
 */
export function isTauriHost(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/**
 * Fetches core engine build metadata via `tauri-specta` in desktop mode,
 * or falls back to mock metadata for standalone browser development.
 */
export async function fetchEngineStatus(): Promise<RuntimeStatus> {
  const webgpuAvailable = typeof navigator !== "undefined" && "gpu" in navigator;

  if (isTauriHost()) {
    const engine = await commands.getEngineInfo();
    return {
      mode: "tauri",
      engine,
      webgpuAvailable,
    };
  }

  return {
    mode: "browser-mock",
    engine: {
      core_name: "ddscope-core (browser mock)",
      core_version: "0.1.0",
      bridge_ready: false,
      ddsf_version: DDSF_VERSION_V1,
    },
    webgpuAvailable,
  };
}

function isStubNotImplementedError(err: unknown): boolean {
  if (typeof err === "object" && err !== null && "code" in err) {
    return (err as { code?: string }).code === "NOT_IMPLEMENTED";
  }
  const msg = err instanceof Error ? err.message : String(err);
  return msg.includes("NOT_IMPLEMENTED") || msg.includes("not yet wired");
}

/**
 * Unified control-plane and data-plane facade routing between `tauri-specta`
 * and `MockDdsfController` seamlessly.
 */
export class BridgeClient {
  readonly topicBus: HighFreqTopicBus;
  readonly mock: MockDdsfController;
  private useMockDataPlane = !isTauriHost();
  private tauriEventUnlisten: (() => void) | null = null;

  constructor(bus: HighFreqTopicBus = globalTopicBus) {
    this.topicBus = bus;
    this.mock = new MockDdsfController(bus);
  }

  isMockMode(): boolean {
    return this.useMockDataPlane;
  }

  startDataPlane(rateHz = 120): void {
    if (this.useMockDataPlane) {
      this.mock.start(rateHz);
    }
  }

  stopDataPlane(): void {
    this.mock.stop();
    if (this.tauriEventUnlisten) {
      this.tauriEventUnlisten();
      this.tauriEventUnlisten = null;
    }
  }

  setMockRate(rateHz: number): void {
    this.mock.setRate(rateHz);
  }

  subscribeTelemetry(listener: TelemetryListener): () => void {
    const unsubMock = this.mock.subscribeTelemetry(listener);

    if (isTauriHost() && !this.tauriEventUnlisten) {
      events.workspaceTelemetryEvent
        .listen((ev) => {
          if (!this.useMockDataPlane) {
            listener(ev.payload, {
              pendingQueueDepth: 0,
              totalDispatched: 0,
              coalescedOverwrites: 0,
              motorKp: 1,
              motorFreqHz: 50,
            });
          }
        })
        .then((unlisten) => {
          this.tauriEventUnlisten = unlisten;
        })
        .catch(() => {
          // Ignore event listener failures in headless mock contexts
        });
    }

    return () => {
      unsubMock();
    };
  }

  private async withTauriOrMock<T>(
    tauriCall: () => Promise<T>,
    mockCall: () => Promise<T>,
  ): Promise<T> {
    if (this.useMockDataPlane || !isTauriHost()) {
      return mockCall();
    }
    try {
      return await tauriCall();
    } catch (err: unknown) {
      if (isStubNotImplementedError(err)) {
        // Pre-M4 Tauri stub detected: automatically fall back to browser mock worker
        this.useMockDataPlane = true;
        this.mock.start(120);
        return mockCall();
      }
      throw err;
    }
  }

  listSerialPorts(): Promise<SerialPortInfoDto_Serialize[]> {
    return this.withTauriOrMock(
      () => commands.listSerialPorts(),
      () => this.mock.listSerialPorts(),
    );
  }

  createSource(
    config: CreateSourceRequestDto_Deserialize,
  ): Promise<SourceSummaryDto_Serialize> {
    return this.withTauriOrMock(
      () => commands.createSource(config),
      () => this.mock.createSource(config),
    );
  }

  startSource(sourceId: string): Promise<SourceSummaryDto_Serialize> {
    return this.withTauriOrMock(
      () => commands.startSource(sourceId),
      () => this.mock.startSource(sourceId),
    );
  }

  stopSource(sourceId: string): Promise<SourceSummaryDto_Serialize> {
    return this.withTauriOrMock(
      () => commands.stopSource(sourceId),
      () => this.mock.stopSource(sourceId),
    );
  }

  removeSource(sourceId: string): Promise<null> {
    return this.withTauriOrMock(
      () => commands.removeSource(sourceId),
      () => this.mock.removeSource(sourceId),
    );
  }

  seekReplaySource(req: SeekReplayRequestDto_Deserialize): Promise<null> {
    return this.withTauriOrMock(
      () => commands.seekReplaySource(req),
      () => this.mock.seekReplaySource(req),
    );
  }

  setRawRecorder(
    req: RawRecorderRequestDto_Deserialize,
  ): Promise<SourceSummaryDto_Serialize> {
    return this.withTauriOrMock(
      () => commands.setRawRecorder(req),
      () => this.mock.setRawRecorder(req),
    );
  }

  async getChannelTree(): Promise<ChannelTreeSnapshotDto_Serialize> {
    if (!this.useMockDataPlane && isTauriHost()) {
      const snap = await commands.getChannelTree();
      if (snap.sources.length > 0) {
        return snap;
      }
      // If Tauri stub returned empty tree before M4 wiring, also probe createSource fallback check
      return this.mock.getChannelTree();
    }
    return this.mock.getChannelTree();
  }

  upsertScopeViewport(
    config: ScopeViewportConfigDto_Deserialize,
  ): Promise<ScopeViewportSummaryDto_Serialize> {
    return this.withTauriOrMock(
      () => commands.upsertScopeViewport(config),
      () => this.mock.upsertScopeViewport(config),
    );
  }

  removeScopeViewport(scopeId: number): Promise<null> {
    return this.withTauriOrMock(
      () => commands.removeScopeViewport(scopeId),
      () => this.mock.removeScopeViewport(scopeId),
    );
  }

  rearmScope(scopeId: number): Promise<null> {
    return this.withTauriOrMock(
      () => commands.rearmScope(scopeId),
      () => this.mock.rearmScope(scopeId),
    );
  }

  updateSyncGroup(update: SyncGroupUpdateDto_Deserialize): Promise<null> {
    return this.withTauriOrMock(
      () => commands.updateSyncGroup(update),
      () => this.mock.updateSyncGroup(update),
    );
  }

  sendDownlink(req: SendDownlinkRequestDto_Deserialize): Promise<DownlinkReceiptDto> {
    return this.withTauriOrMock(
      () => commands.sendDownlink(req),
      () => this.mock.sendDownlink(req),
    );
  }

  injectMockGlitch(): Promise<number> {
    return this.mock.injectGlitch();
  }
}

export const bridgeClient = new BridgeClient(globalTopicBus);
export type { MockDownlinkStats, WorkspaceTelemetryEvent };
