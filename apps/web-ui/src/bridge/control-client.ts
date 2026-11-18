import {
  commands,
  events,
  type ChannelTreeSnapshotDto_Serialize,
  type CreateSourceRequestDto_Deserialize,
  type CreateSourceRequestDto_Serialize,
  type DownlinkReceiptDto,
  type EngineStatusDto,
  type ExternalPluginCatalogDto_Serialize,
  type RawRecorderRequestDto_Deserialize,
  type ScopeViewportConfigDto_Deserialize,
  type ScopeViewportSummaryDto_Serialize,
  type SeekReplayRequestDto_Deserialize,
  type SendDownlinkRequestDto_Deserialize,
  type SerialPortInfoDto_Serialize,
  type SourceSummaryDto_Serialize,
  type SyncGroupUpdateDto_Deserialize,
  type VirtualNodeConfigDto_Deserialize,
  type VirtualNodeConfigDto_Serialize,
  type VirtualNodeSummaryDto_Serialize,
  type WidgetPluginBundleDto_Serialize,
  type WorkspaceTelemetryEvent,
} from "./bindings.ts";
import { DDSF_VERSION_V1 } from "./ddsf-spec.ts";
import {
  MockDdsfController,
  type MockDownlinkStats,
  type TelemetryListener,
} from "./mock-ddsf.ts";
import { globalTopicBus, type HighFreqTopicBus } from "./topic-bus.ts";
import {
  ZeroCopyDataBridgeReceiver,
  type ZeroCopyBridgeHostAdapter,
} from "./zero-copy-bridge.ts";
import { getExternalPluginCatalog } from "../widgets/plugin-registry.ts";

export const DEFAULT_PROJECT_STORAGE_KEY = "ddscope.defaultProjectConfig.v1";

export interface ControlPlaneCommands {
  getEngineInfo(): Promise<EngineStatusDto>;
  listSerialPorts(): Promise<SerialPortInfoDto_Serialize[]>;
  createSource(
    config: CreateSourceRequestDto_Deserialize,
  ): Promise<SourceSummaryDto_Serialize>;
  updateSource?(
    sourceId: string,
    config: CreateSourceRequestDto_Deserialize,
  ): Promise<SourceSummaryDto_Serialize>;
  getRunBoundaries?(): Promise<(number | null)[]>;
  startSource(sourceId: string): Promise<SourceSummaryDto_Serialize>;
  stopSource(sourceId: string): Promise<SourceSummaryDto_Serialize>;
  removeSource(sourceId: string): Promise<null>;
  seekReplaySource(req: SeekReplayRequestDto_Deserialize): Promise<null>;
  setRawRecorder(
    req: RawRecorderRequestDto_Deserialize,
  ): Promise<SourceSummaryDto_Serialize>;
  getChannelTree(): Promise<ChannelTreeSnapshotDto_Serialize>;
  getSourceConfigs?(): Promise<CreateSourceRequestDto_Serialize[]>;
  upsertScopeViewport(
    config: ScopeViewportConfigDto_Deserialize,
  ): Promise<ScopeViewportSummaryDto_Serialize>;
  removeScopeViewport(scopeId: number): Promise<null>;
  rearmScope(scopeId: number): Promise<null>;
  updateSyncGroup(update: SyncGroupUpdateDto_Deserialize): Promise<null>;
  sendDownlink(req: SendDownlinkRequestDto_Deserialize): Promise<DownlinkReceiptDto>;
  loadDefaultProjectConfig?(): Promise<string | null>;
  saveDefaultProjectConfig?(configJson: string): Promise<string>;
  clearDefaultProjectConfig?(): Promise<boolean>;
  listVirtualChannels?(): Promise<VirtualNodeConfigDto_Serialize[]>;
  upsertVirtualChannel?(
    config: VirtualNodeConfigDto_Deserialize,
  ): Promise<VirtualNodeSummaryDto_Serialize>;
  removeVirtualChannel?(nodeId: string): Promise<boolean>;
  clearVirtualChannels?(): Promise<number>;
  listExternalPlugins?(): Promise<ExternalPluginCatalogDto_Serialize>;
  reloadExternalPlugins?(): Promise<ExternalPluginCatalogDto_Serialize>;
  loadWidgetPluginBundle?(pluginId: string): Promise<WidgetPluginBundleDto_Serialize>;
}

export interface ControlPlaneHostAdapter {
  isTauriHost(): boolean;
  commands: ControlPlaneCommands;
  listenTelemetry(
    handler: (payload: WorkspaceTelemetryEvent) => void,
  ): Promise<() => void>;
  zeroCopyHostAdapter?: ZeroCopyBridgeHostAdapter;
}

export function createDefaultControlPlaneHostAdapter(): ControlPlaneHostAdapter {
  return {
    isTauriHost,
    commands,
    listenTelemetry: (handler) =>
      events.workspaceTelemetryEvent.listen((ev) => handler(ev.payload)),
  };
}

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
export async function fetchEngineStatus(
  host: Pick<ControlPlaneHostAdapter, "isTauriHost" | "commands"> = {
    isTauriHost,
    commands,
  },
): Promise<RuntimeStatus> {
  const webgpuAvailable = typeof navigator !== "undefined" && "gpu" in navigator;

  if (host.isTauriHost()) {
    const engine = await host.commands.getEngineInfo();
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

interface PendingCoalescedEntry {
  req: SendDownlinkRequestDto_Deserialize;
  resolve: (receipt: DownlinkReceiptDto) => void;
  reject: (err: unknown) => void;
  scheduled: boolean;
}

/**
 * Unified control-plane and data-plane facade routing between `tauri-specta`
 * and `MockDdsfController` seamlessly.
 */
export class BridgeClient {
  readonly topicBus: HighFreqTopicBus;
  readonly mock: MockDdsfController;
  readonly dataBridge: ZeroCopyDataBridgeReceiver;
  private readonly host: ControlPlaneHostAdapter;
  private useMockDataPlane: boolean;
  private tauriEventUnlisten: (() => void) | null = null;
  private tauriListenPromise: Promise<void> | null = null;
  private readonly telemetryListeners = new Set<TelemetryListener>();
  private lastTelemetryEvt: WorkspaceTelemetryEvent | null = null;
  private startPromise: Promise<void> | null = null;

  private readonly pendingCoalesced = new Map<string, PendingCoalescedEntry>();
  private readonly inFlightCoalesced = new Set<string>();
  private inFlightDirectCount = 0;
  private liveStats: MockDownlinkStats = {
    pendingQueueDepth: 0,
    totalDispatched: 0,
    coalescedOverwrites: 0,
    motorKp: 1.0,
    motorFreqHz: 50,
  };

  constructor(
    bus: HighFreqTopicBus = globalTopicBus,
    host: ControlPlaneHostAdapter = createDefaultControlPlaneHostAdapter(),
  ) {
    this.topicBus = bus;
    this.host = host;
    this.useMockDataPlane = !host.isTauriHost();
    this.mock = new MockDdsfController(bus);
    this.dataBridge = new ZeroCopyDataBridgeReceiver(
      bus,
      host.zeroCopyHostAdapter,
    );
  }

  isMockMode(): boolean {
    return this.useMockDataPlane;
  }

  getDownlinkStats(): MockDownlinkStats {
    if (this.useMockDataPlane || !this.host.isTauriHost()) {
      return this.mock.getDownlinkStats();
    }
    return { ...this.liveStats };
  }

  startDataPlane(rateHz = 120): Promise<void> {
    if (!this.host.isTauriHost()) {
      this.useMockDataPlane = true;
      this.mock.start(rateHz);
      return Promise.resolve();
    }
    if (this.startPromise) {
      return this.startPromise;
    }
    const run = this.startTauriDataPlane(rateHz).finally(() => {
      if (this.startPromise === run) {
        this.startPromise = null;
      }
    });
    this.startPromise = run;
    return run;
  }

  private async startTauriDataPlane(rateHz: number): Promise<void> {
    try {
      const info = await this.host.commands.getEngineInfo();
      if (!info.bridge_ready) {
        await this.dataBridge.detach();
        this.detachTauriTelemetry();
        this.useMockDataPlane = true;
        this.mock.start(rateHz);
        return;
      }
      this.useMockDataPlane = false;
      this.mock.stop();
      await this.dataBridge.attach(null);
      await this.dataBridge.setRate(rateHz);
      this.ensureTauriTelemetryListener();
    } catch (err: unknown) {
      let bridgeReady = true;
      try {
        const info = await this.host.commands.getEngineInfo();
        bridgeReady = info.bridge_ready;
      } catch {
        bridgeReady = false;
      }
      if (isStubNotImplementedError(err) || !bridgeReady) {
        await this.dataBridge.detach();
        this.detachTauriTelemetry();
        this.useMockDataPlane = true;
        this.mock.start(rateHz);
      }
    }
  }

  stopDataPlane(): void {
    this.mock.stop();
    void this.dataBridge.detach();
    this.detachTauriTelemetry();
  }

  setMockRate(rateHz: number): void {
    if (!this.useMockDataPlane && this.host.isTauriHost()) {
      void this.dataBridge.setRate(rateHz);
      return;
    }
    this.mock.setRate(rateHz);
  }

  private detachTauriTelemetry(): void {
    if (this.tauriEventUnlisten) {
      this.tauriEventUnlisten();
      this.tauriEventUnlisten = null;
    }
    this.tauriListenPromise = null;
  }

  private ensureTauriTelemetryListener(): void {
    if (
      !this.host.isTauriHost() ||
      this.useMockDataPlane ||
      this.telemetryListeners.size === 0 ||
      this.tauriEventUnlisten ||
      this.tauriListenPromise
    ) {
      return;
    }
    const listenPromise = this.host
      .listenTelemetry((payload) => {
        this.lastTelemetryEvt = payload;
        if (!this.useMockDataPlane) {
          const stats = this.getDownlinkStats();
          for (const listener of this.telemetryListeners) {
            listener(payload, stats);
          }
        }
      })
      .then((unlisten) => {
        if (this.telemetryListeners.size === 0 || this.useMockDataPlane) {
          unlisten();
          return;
        }
        this.tauriEventUnlisten = unlisten;
      })
      .catch(() => {
        // Ignore event listener failures in headless mock contexts
      })
      .finally(() => {
        if (this.tauriListenPromise === listenPromise) {
          this.tauriListenPromise = null;
        }
      });
    this.tauriListenPromise = listenPromise;
  }

  private emitCachedLiveStats(): void {
    if (this.useMockDataPlane || !this.lastTelemetryEvt) {
      return;
    }
    const stats = this.getDownlinkStats();
    for (const listener of this.telemetryListeners) {
      listener(this.lastTelemetryEvt, stats);
    }
  }

  subscribeTelemetry(listener: TelemetryListener): () => void {
    this.telemetryListeners.add(listener);
    const unsubMock = this.mock.subscribeTelemetry(listener);
    this.ensureTauriTelemetryListener();

    return () => {
      unsubMock();
      this.telemetryListeners.delete(listener);
      if (this.telemetryListeners.size === 0) {
        this.detachTauriTelemetry();
      }
    };
  }

  private async withTauriOrMock<T>(
    tauriCall: () => Promise<T>,
    mockCall: () => Promise<T>,
  ): Promise<T> {
    if (this.useMockDataPlane || !this.host.isTauriHost()) {
      return mockCall();
    }
    try {
      return await tauriCall();
    } catch (err: unknown) {
      if (isStubNotImplementedError(err)) {
        // Pre-M4 Tauri stub detected: automatically fall back to browser mock worker
        void this.dataBridge.detach();
        this.detachTauriTelemetry();
        this.useMockDataPlane = true;
        this.mock.start(120);
        return mockCall();
      }
      throw err;
    }
  }

  listSerialPorts(): Promise<SerialPortInfoDto_Serialize[]> {
    return this.withTauriOrMock(
      () => this.host.commands.listSerialPorts(),
      () => this.mock.listSerialPorts(),
    );
  }

  createSource(
    config: CreateSourceRequestDto_Deserialize,
  ): Promise<SourceSummaryDto_Serialize> {
    return this.withTauriOrMock(
      () => this.host.commands.createSource(config),
      () => this.mock.createSource(config),
    );
  }

  updateSource(
    sourceId: string,
    config: CreateSourceRequestDto_Deserialize,
  ): Promise<SourceSummaryDto_Serialize> {
    return this.withTauriOrMock(
      () =>
        this.host.commands.updateSource
          ? this.host.commands.updateSource(sourceId, config)
          : Promise.reject(new Error("updateSource command is unavailable on this host")),
      () => this.mock.updateSource(sourceId, config),
    );
  }

  async getRunBoundaries(): Promise<number[]> {
    const raw = await this.withTauriOrMock<(number | null)[]>(
      () =>
        this.host.commands.getRunBoundaries
          ? this.host.commands.getRunBoundaries()
          : Promise.resolve([]),
      () => this.mock.getRunBoundaries(),
    );
    return raw.filter((v): v is number => typeof v === "number" && Number.isFinite(v));
  }

  startSource(sourceId: string): Promise<SourceSummaryDto_Serialize> {
    return this.withTauriOrMock(
      () => this.host.commands.startSource(sourceId),
      () => this.mock.startSource(sourceId),
    );
  }

  stopSource(sourceId: string): Promise<SourceSummaryDto_Serialize> {
    return this.withTauriOrMock(
      () => this.host.commands.stopSource(sourceId),
      () => this.mock.stopSource(sourceId),
    );
  }

  removeSource(sourceId: string): Promise<null> {
    return this.withTauriOrMock(
      () => this.host.commands.removeSource(sourceId),
      () => this.mock.removeSource(sourceId),
    );
  }

  seekReplaySource(req: SeekReplayRequestDto_Deserialize): Promise<null> {
    return this.withTauriOrMock(
      () => this.host.commands.seekReplaySource(req),
      () => this.mock.seekReplaySource(req),
    );
  }

  setRawRecorder(
    req: RawRecorderRequestDto_Deserialize,
  ): Promise<SourceSummaryDto_Serialize> {
    return this.withTauriOrMock(
      () => this.host.commands.setRawRecorder(req),
      () => this.mock.setRawRecorder(req),
    );
  }

  getChannelTree(): Promise<ChannelTreeSnapshotDto_Serialize> {
    return this.withTauriOrMock(
      () => this.host.commands.getChannelTree(),
      () => this.mock.getChannelTree(),
    );
  }

  getSourceConfigs(): Promise<CreateSourceRequestDto_Serialize[]> {
    return this.withTauriOrMock(
      () =>
        this.host.commands.getSourceConfigs
          ? this.host.commands.getSourceConfigs()
          : Promise.resolve([]),
      () => this.mock.getSourceConfigs(),
    );
  }

  upsertScopeViewport(
    config: ScopeViewportConfigDto_Deserialize,
  ): Promise<ScopeViewportSummaryDto_Serialize> {
    return this.withTauriOrMock(
      () => this.host.commands.upsertScopeViewport(config),
      () => this.mock.upsertScopeViewport(config),
    );
  }

  removeScopeViewport(scopeId: number): Promise<null> {
    return this.withTauriOrMock(
      () => this.host.commands.removeScopeViewport(scopeId),
      () => this.mock.removeScopeViewport(scopeId),
    );
  }

  rearmScope(scopeId: number): Promise<null> {
    return this.withTauriOrMock(
      () => this.host.commands.rearmScope(scopeId),
      () => this.mock.rearmScope(scopeId),
    );
  }

  updateSyncGroup(update: SyncGroupUpdateDto_Deserialize): Promise<null> {
    return this.withTauriOrMock(
      () => this.host.commands.updateSyncGroup(update),
      () => this.mock.updateSyncGroup(update),
    );
  }

  private refreshPendingQueueDepth(): void {
    this.liveStats.pendingQueueDepth =
      this.pendingCoalesced.size +
      this.inFlightCoalesced.size +
      this.inFlightDirectCount;
  }

  private trackLiveDownlinkParams(req: SendDownlinkRequestDto_Deserialize): void {
    if (
      req.payload.kind === "key_value" &&
      typeof req.payload.value === "number" &&
      Number.isFinite(req.payload.value)
    ) {
      if (req.payload.key === "kp") {
        this.liveStats.motorKp = req.payload.value;
      } else if (req.payload.key === "freq_hz") {
        this.liveStats.motorFreqHz = req.payload.value;
      }
      return;
    }

    if (req.payload.kind === "ascii_text") {
      const txt = req.payload.text.trim();
      const lower = txt.toLowerCase();
      if (lower.startsWith("kp=")) {
        const parsed = Number.parseFloat(txt.slice(3));
        if (Number.isFinite(parsed)) {
          this.liveStats.motorKp = parsed;
        }
      } else if (lower.startsWith("freq_hz=")) {
        const parsed = Number.parseFloat(txt.slice(8));
        if (Number.isFinite(parsed)) {
          this.liveStats.motorFreqHz = parsed;
        }
      } else if (lower.startsWith("freq=")) {
        const parsed = Number.parseFloat(txt.slice(5));
        if (Number.isFinite(parsed)) {
          this.liveStats.motorFreqHz = parsed;
        }
      }
    }
  }

  private flushCoalescedKey(compositeKey: string): void {
    if (this.inFlightCoalesced.has(compositeKey)) {
      return;
    }
    const entry = this.pendingCoalesced.get(compositeKey);
    if (!entry) {
      return;
    }
    this.pendingCoalesced.delete(compositeKey);
    this.inFlightCoalesced.add(compositeKey);
    this.refreshPendingQueueDepth();
    this.emitCachedLiveStats();

    void this.withTauriOrMock(
      () => this.host.commands.sendDownlink(entry.req),
      () => this.mock.sendDownlink(entry.req),
    )
      .then((receipt) => {
        if (!this.useMockDataPlane && this.host.isTauriHost()) {
          if (receipt.coalesced_existing) {
            this.liveStats.coalescedOverwrites += 1;
          } else {
            this.liveStats.totalDispatched += 1;
          }
        }
        entry.resolve(receipt);
      })
      .catch((err) => {
        entry.reject(err);
      })
      .finally(() => {
        this.inFlightCoalesced.delete(compositeKey);
        if (this.pendingCoalesced.has(compositeKey)) {
          queueMicrotask(() => this.flushCoalescedKey(compositeKey));
        }
        this.refreshPendingQueueDepth();
        this.emitCachedLiveStats();
      });
  }

  async sendDownlink(
    req: SendDownlinkRequestDto_Deserialize,
  ): Promise<DownlinkReceiptDto> {
    if (this.useMockDataPlane || !this.host.isTauriHost()) {
      return this.mock.sendDownlink(req);
    }

    this.trackLiveDownlinkParams(req);
    const coalesceKey = req.coalesce_key?.trim() ?? "";
    if (!coalesceKey) {
      this.inFlightDirectCount += 1;
      this.refreshPendingQueueDepth();
      this.emitCachedLiveStats();
      try {
        const receipt = await this.withTauriOrMock(
          () => this.host.commands.sendDownlink(req),
          () => this.mock.sendDownlink(req),
        );
        if (!this.useMockDataPlane && this.host.isTauriHost()) {
          if (receipt.coalesced_existing) {
            this.liveStats.coalescedOverwrites += 1;
          } else {
            this.liveStats.totalDispatched += 1;
          }
        }
        return receipt;
      } finally {
        this.inFlightDirectCount = Math.max(0, this.inFlightDirectCount - 1);
        this.refreshPendingQueueDepth();
        this.emitCachedLiveStats();
      }
    }

    const compositeKey = `${req.source_id}::${coalesceKey}`;
    return new Promise<DownlinkReceiptDto>((resolve, reject) => {
      const existing = this.pendingCoalesced.get(compositeKey);
      if (existing) {
        this.liveStats.coalescedOverwrites += 1;
        existing.resolve({
          accepted: true,
          coalesced_existing: true,
          result_kind: "sent_fire_and_forget",
        });
        existing.req = req;
        existing.resolve = resolve;
        existing.reject = reject;
        this.refreshPendingQueueDepth();
        this.emitCachedLiveStats();
        return;
      }

      const entry: PendingCoalescedEntry = {
        req,
        resolve,
        reject,
        scheduled: true,
      };
      this.pendingCoalesced.set(compositeKey, entry);
      this.refreshPendingQueueDepth();
      this.emitCachedLiveStats();
      queueMicrotask(() => this.flushCoalescedKey(compositeKey));
    });
  }

  async injectGlitch(simSourceIds?: string[]): Promise<void> {
    if (this.useMockDataPlane || !this.host.isTauriHost()) {
      await this.mock.injectGlitch();
      return;
    }

    let targets = simSourceIds?.filter((id) => id.trim().length > 0) ?? [];
    if (targets.length === 0) {
      const tree = await this.getChannelTree();
      targets = tree.sources
        .filter(
          (g) => g.source.transport_kind === "sim" && g.source.state === "running",
        )
        .map((g) => g.source.source_id);
      if (targets.length === 0) {
        targets = tree.sources
          .filter((g) => g.source.transport_kind === "sim")
          .map((g) => g.source.source_id);
      }
    }

    await Promise.all(
      targets.map((sourceId) =>
        this.sendDownlink({
          source_id: sourceId,
          priority: "high",
          coalesce_key: null,
          payload: {
            kind: "ascii_text",
            text: "GLITCH",
            append_newline: true,
          },
        }),
      ),
    );
  }

  injectMockGlitch(): Promise<void> {
    return this.injectGlitch();
  }

  async loadDefaultProjectConfig(): Promise<string | null> {
    if (this.host.isTauriHost() && this.host.commands.loadDefaultProjectConfig) {
      try {
        return await this.host.commands.loadDefaultProjectConfig();
      } catch (err: unknown) {
        if (!isStubNotImplementedError(err)) {
          throw err;
        }
      }
    }
    if (typeof window !== "undefined" && window.localStorage) {
      try {
        return window.localStorage.getItem(DEFAULT_PROJECT_STORAGE_KEY);
      } catch {
        return null;
      }
    }
    return null;
  }

  async saveDefaultProjectConfig(
    configJson: string,
  ): Promise<{ location: string; mode: "exe-dir" | "browser-storage" }> {
    if (this.host.isTauriHost() && this.host.commands.saveDefaultProjectConfig) {
      try {
        const savedPath = await this.host.commands.saveDefaultProjectConfig(configJson);
        return { location: savedPath, mode: "exe-dir" };
      } catch (err: unknown) {
        if (!isStubNotImplementedError(err)) {
          throw err;
        }
      }
    }
    if (typeof window !== "undefined" && window.localStorage) {
      window.localStorage.setItem(DEFAULT_PROJECT_STORAGE_KEY, configJson);
      return {
        location: `localStorage/${DEFAULT_PROJECT_STORAGE_KEY}`,
        mode: "browser-storage",
      };
    }
    throw new Error("No persistent default project config storage available in this environment.");
  }

  async clearDefaultProjectConfig(): Promise<boolean> {
    if (this.host.isTauriHost() && this.host.commands.clearDefaultProjectConfig) {
      try {
        return await this.host.commands.clearDefaultProjectConfig();
      } catch (err: unknown) {
        if (!isStubNotImplementedError(err)) {
          throw err;
        }
      }
    }
    if (typeof window !== "undefined" && window.localStorage) {
      try {
        const existed = window.localStorage.getItem(DEFAULT_PROJECT_STORAGE_KEY) !== null;
        window.localStorage.removeItem(DEFAULT_PROJECT_STORAGE_KEY);
        return existed;
      } catch {
        return false;
      }
    }
    return false;
  }

  listVirtualChannels(): Promise<VirtualNodeConfigDto_Serialize[]> {
    return this.withTauriOrMock(
      () =>
        this.host.commands.listVirtualChannels
          ? this.host.commands.listVirtualChannels()
          : Promise.resolve([]),
      () => this.mock.listVirtualChannels(),
    );
  }

  upsertVirtualChannel(
    config: VirtualNodeConfigDto_Deserialize,
  ): Promise<VirtualNodeSummaryDto_Serialize> {
    return this.withTauriOrMock(
      () => {
        if (!this.host.commands.upsertVirtualChannel) {
          throw new Error("upsertVirtualChannel is not implemented on this host adapter");
        }
        return this.host.commands.upsertVirtualChannel(config);
      },
      () => this.mock.upsertVirtualChannel(config),
    );
  }

  removeVirtualChannel(nodeId: string): Promise<boolean> {
    return this.withTauriOrMock(
      () =>
        this.host.commands.removeVirtualChannel
          ? this.host.commands.removeVirtualChannel(nodeId)
          : Promise.resolve(false),
      () => this.mock.removeVirtualChannel(nodeId),
    );
  }

  clearVirtualChannels(): Promise<number> {
    return this.withTauriOrMock(
      () =>
        this.host.commands.clearVirtualChannels
          ? this.host.commands.clearVirtualChannels()
          : Promise.resolve(0),
      () => this.mock.clearVirtualChannels(),
    );
  }

  async listExternalPlugins(): Promise<ExternalPluginCatalogDto_Serialize> {
    if (this.host.isTauriHost() && this.host.commands.listExternalPlugins) {
      try {
        return await this.host.commands.listExternalPlugins();
      } catch (err: unknown) {
        if (!isStubNotImplementedError(err)) {
          throw err;
        }
      }
    }
    if (typeof window !== "undefined" && typeof fetch === "function") {
      try {
        const res = await fetch("/api/ddscope-plugins/catalog");
        if (res.ok) {
          return (await res.json()) as ExternalPluginCatalogDto_Serialize;
        }
      } catch {
        // Ignore dev server fetch errors when running offline/headless tests
      }
    }
    return getExternalPluginCatalog();
  }

  async reloadExternalPlugins(): Promise<ExternalPluginCatalogDto_Serialize> {
    if (this.host.isTauriHost() && this.host.commands.reloadExternalPlugins) {
      try {
        return await this.host.commands.reloadExternalPlugins();
      } catch (err: unknown) {
        if (!isStubNotImplementedError(err)) {
          throw err;
        }
      }
    }
    if (typeof window !== "undefined" && typeof fetch === "function") {
      try {
        const res = await fetch("/api/ddscope-plugins/reload", { method: "POST" });
        if (res.ok) {
          return (await res.json()) as ExternalPluginCatalogDto_Serialize;
        }
      } catch {
        // Ignore dev server fetch errors when running offline/headless tests
      }
    }
    return getExternalPluginCatalog();
  }

  async loadWidgetPluginBundle(pluginId: string): Promise<WidgetPluginBundleDto_Serialize> {
    if (this.host.isTauriHost() && this.host.commands.loadWidgetPluginBundle) {
      try {
        return await this.host.commands.loadWidgetPluginBundle(pluginId);
      } catch (err: unknown) {
        if (!isStubNotImplementedError(err)) {
          throw err;
        }
      }
    }
    if (typeof window !== "undefined" && typeof fetch === "function") {
      const res = await fetch(`/api/ddscope-plugins/widget/${encodeURIComponent(pluginId)}`);
      if (res.ok) {
        return (await res.json()) as WidgetPluginBundleDto_Serialize;
      }
      throw new Error(`External widget plugin '${pluginId}' bundle returned HTTP ${res.status}`);
    }
    throw new Error(`External widget plugin '${pluginId}' loader unavailable in this environment`);
  }
}

export const bridgeClient = new BridgeClient(globalTopicBus);
export type { MockDownlinkStats, WorkspaceTelemetryEvent };
