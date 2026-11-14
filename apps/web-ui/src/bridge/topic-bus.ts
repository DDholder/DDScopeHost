import {
  DDSF_FLAG_PINNED_ACTIVE,
  DDSF_FLAG_QUOTA_WARNING,
} from "./ddsf-spec.ts";
import {
  parseDdsfFrame,
  type DdsfFrameHeaderView,
  type DdsfFrameView,
  type DdsfScopeView,
  type DdsfSnapshotView,
} from "./ddsf-parser.ts";

export interface DataPlaneMetrics {
  ddsfFps: number;
  bytesPerSec: number;
  frameSeq: number;
  scopeCount: number;
  snapshotCount: number;
  quotaWarning: boolean;
  pinnedActive: boolean;
  parseErrors: number;
}

export type ScopeFrameListener = (
  scope: DdsfScopeView,
  header: DdsfFrameHeaderView,
) => void;

export type SnapshotFrameListener = (
  snapshots: DdsfSnapshotView[],
  header: DdsfFrameHeaderView,
) => void;

export type MetricsListener = (metrics: DataPlaneMetrics) => void;

/**
 * Lock-free high-frequency pub/sub bus dispatching zero-copy `DDSF v1` views
 * directly to scope renderers and DOM-ref widgets, bypassing React VDOM.
 */
export class HighFreqTopicBus {
  private readonly scopeListeners = new Map<number, Set<ScopeFrameListener>>();
  private readonly snapshotListeners = new Set<SnapshotFrameListener>();
  private readonly metricsListeners = new Set<MetricsListener>();

  private readonly latestScopes = new Map<
    number,
    { scope: DdsfScopeView; header: DdsfFrameHeaderView }
  >();
  private readonly latestSnapshots = new Map<number, DdsfSnapshotView>();

  private windowStartMs = 0;
  private windowFrames = 0;
  private windowBytes = 0;
  private lastMetricsEmitMs = 0;

  private metrics: DataPlaneMetrics = {
    ddsfFps: 0,
    bytesPerSec: 0,
    frameSeq: 0,
    scopeCount: 0,
    snapshotCount: 0,
    quotaWarning: false,
    pinnedActive: false,
    parseErrors: 0,
  };

  ingestFrame(
    buffer: ArrayBufferLike,
    byteOffset = 0,
    byteLength = buffer.byteLength - byteOffset,
  ): boolean {
    let parsed: DdsfFrameView;
    try {
      parsed = parseDdsfFrame(buffer, byteOffset, byteLength);
    } catch {
      this.metrics = {
        ...this.metrics,
        parseErrors: this.metrics.parseErrors + 1,
      };
      this.emitMetricsNow();
      return false;
    }

    const { header, scopes, snapshots } = parsed;

    for (let i = 0; i < scopes.length; i += 1) {
      const scope = scopes[i];
      this.latestScopes.set(scope.scopeId, { scope, header });
      const listeners = this.scopeListeners.get(scope.scopeId);
      if (listeners) {
        listeners.forEach((cb) => {
          cb(scope, header);
        });
      }
    }

    if (snapshots.length > 0) {
      for (let i = 0; i < snapshots.length; i += 1) {
        this.latestSnapshots.set(snapshots[i].channelNumId, snapshots[i]);
      }
      this.snapshotListeners.forEach((cb) => {
        cb(snapshots, header);
      });
    }

    const nowMs = typeof performance !== "undefined" ? performance.now() : Date.now();
    if (this.windowStartMs === 0) {
      this.windowStartMs = nowMs;
      this.lastMetricsEmitMs = nowMs;
    }

    this.windowFrames += 1;
    this.windowBytes += parsed.byteLength;

    const elapsedMs = nowMs - this.windowStartMs;
    if (elapsedMs >= 500) {
      const ddsfFps = (this.windowFrames * 1000) / elapsedMs;
      const bytesPerSec = (this.windowBytes * 1000) / elapsedMs;
      this.metrics = {
        ddsfFps,
        bytesPerSec,
        frameSeq: header.frameSeq,
        scopeCount: header.scopeCount,
        snapshotCount: header.snapshotCount,
        quotaWarning: (header.flags & DDSF_FLAG_QUOTA_WARNING) !== 0,
        pinnedActive: (header.flags & DDSF_FLAG_PINNED_ACTIVE) !== 0,
        parseErrors: this.metrics.parseErrors,
      };
      this.windowStartMs = nowMs;
      this.windowFrames = 0;
      this.windowBytes = 0;
      if (nowMs - this.lastMetricsEmitMs >= 250) {
        this.lastMetricsEmitMs = nowMs;
        this.emitMetricsNow();
      }
    } else {
      this.metrics = {
        ...this.metrics,
        frameSeq: header.frameSeq,
        scopeCount: header.scopeCount,
        snapshotCount: header.snapshotCount,
        quotaWarning: (header.flags & DDSF_FLAG_QUOTA_WARNING) !== 0,
        pinnedActive: (header.flags & DDSF_FLAG_PINNED_ACTIVE) !== 0,
      };
      if (this.windowFrames === 1) {
        this.emitMetricsNow();
      }
    }

    return true;
  }

  subscribeScope(scopeId: number, listener: ScopeFrameListener): () => void {
    let set = this.scopeListeners.get(scopeId);
    if (!set) {
      set = new Set();
      this.scopeListeners.set(scopeId, set);
    }
    set.add(listener);

    const latest = this.latestScopes.get(scopeId);
    if (latest) {
      listener(latest.scope, latest.header);
    }

    return () => {
      const current = this.scopeListeners.get(scopeId);
      if (current) {
        current.delete(listener);
        if (current.size === 0) {
          this.scopeListeners.delete(scopeId);
        }
      }
    };
  }

  getLatestScopeFrame(
    scopeId: number,
  ): { scope: DdsfScopeView; header: DdsfFrameHeaderView } | null {
    return this.latestScopes.get(scopeId) ?? null;
  }

  deleteScopeCache(scopeId: number): void {
    this.latestScopes.delete(scopeId);
  }

  subscribeSnapshots(listener: SnapshotFrameListener): () => void {
    this.snapshotListeners.add(listener);
    return () => {
      this.snapshotListeners.delete(listener);
    };
  }

  getLatestSnapshot(channelId: number): DdsfSnapshotView | null {
    return this.latestSnapshots.get(channelId) ?? null;
  }

  subscribeMetrics(listener: MetricsListener): () => void {
    this.metricsListeners.add(listener);
    listener(this.metrics);
    return () => {
      this.metricsListeners.delete(listener);
    };
  }

  getMetrics(): DataPlaneMetrics {
    return this.metrics;
  }

  private emitMetricsNow(): void {
    const snap = this.metrics;
    this.metricsListeners.forEach((cb) => {
      cb(snap);
    });
  }
}

export const globalTopicBus = new HighFreqTopicBus();
