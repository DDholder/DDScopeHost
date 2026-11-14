import { create, type StoreApi, type UseBoundStore } from "zustand";
import type {
  ChannelNodeDto_Serialize,
  CreateSourceRequestDto_Deserialize,
  DownlinkReceiptDto,
  ScopeLaneKindDto,
  ScopeViewportConfigDto_Deserialize,
  SeekReplayRequestDto_Deserialize,
  SendDownlinkRequestDto_Deserialize,
  SourceChannelGroupDto_Serialize,
  SourceTelemetryDto,
} from "../bridge/bindings.ts";
import {
  BridgeClient,
  bridgeClient,
  fetchEngineStatus,
  type DataPlaneMetrics,
  type MockDownlinkStats,
  type RuntimeStatus,
} from "../bridge/index.ts";
import {
  detectInitialLocale,
  getDefaultScopeTitle,
  persistLocale,
  translateDefaultScopeTitle,
  type UiLocale,
} from "../i18n/index.ts";
import type {
  ScopeTrackUiBinding,
  ScopeUiConfig,
} from "../scope-engine/types.ts";

const TRACE_COLORS = [
  0x38bdf8ff, // sky
  0x34d399ff, // emerald
  0xf59e0bff, // amber
  0xfb7185ff, // rose
  0xa78bfaff, // violet
  0x22d3eeff, // cyan
  0xf97316ff, // orange
  0xe879f9ff, // fuchsia
];

export interface DownlinkLogEntry {
  id: number;
  timestamp: string;
  sourceId: string;
  summary: string;
  coalesced: boolean;
  resultKind: string;
}

export interface WorkspaceStoreState {
  initialized: boolean;
  initError: string | null;
  runtimeStatus: RuntimeStatus | null;
  channelGroups: SourceChannelGroupDto_Serialize[];
  channelsById: Record<number, ChannelNodeDto_Serialize>;
  telemetryBySource: Record<string, SourceTelemetryDto>;
  quota: {
    usedBytes: number;
    limitBytes: number;
    pinnedBytes: number;
  };
  dataPlaneMetrics: DataPlaneMetrics;
  downlinkStats: MockDownlinkStats;
  sliderEventCount: number;
  txLog: DownlinkLogEntry[];

  scopes: Record<number, ScopeUiConfig>;
  scopeOrder: number[];
  focusedScopeId: number | null;
  nextScopeId: number;
  canvasWidthsByScope: Record<number, number>;

  locale: UiLocale;
  mockRateHz: 60 | 120;
  sidebarCollapsed: boolean;
  downlinkBarCollapsed: boolean;
  sourceWizardOpen: boolean;

  setLocale: (locale: UiLocale) => void;
  initWorkspace: () => Promise<void>;
  refreshChannelTree: () => Promise<void>;
  setMockRateHz: (rate: 60 | 120) => void;
  setSidebarCollapsed: (collapsed: boolean) => void;
  setDownlinkBarCollapsed: (collapsed: boolean) => void;
  setSourceWizardOpen: (open: boolean) => void;
  setFocusedScopeId: (scopeId: number | null) => void;

  createSource: (req: CreateSourceRequestDto_Deserialize) => Promise<void>;
  startSource: (sourceId: string) => Promise<void>;
  stopSource: (sourceId: string) => Promise<void>;
  removeSource: (sourceId: string) => Promise<void>;
  toggleRawRecorder: (sourceId: string, enabled: boolean) => Promise<void>;
  seekReplay: (req: SeekReplayRequestDto_Deserialize) => Promise<void>;

  addScope: (title?: string) => number;
  removeScope: (scopeId: number) => void;
  updateScope: (scopeId: number, updater: (prev: ScopeUiConfig) => ScopeUiConfig) => void;
  setScopeCanvasWidth: (scopeId: number, widthPx: number) => void;
  rearmScope: (scopeId: number) => void;
  bindChannelToScope: (scopeId: number, channelId: number) => void;
  broadcastSyncGroup: (
    groupId: number,
    originScopeId: number,
    patch: {
      timeSpanNs?: number;
      panOffsetNs?: number;
      cursorsEnabled?: boolean;
      cursorARatio?: number;
      cursorBRatio?: number;
    },
  ) => void;

  sendDownlink: (
    req: SendDownlinkRequestDto_Deserialize,
    labelSummary?: string,
    isSliderEvent?: boolean,
  ) => Promise<DownlinkReceiptDto>;
  injectGlitch: () => Promise<void>;
}

function toViewportDto(
  cfg: ScopeUiConfig,
  canvasWidthPx: number,
): ScopeViewportConfigDto_Deserialize {
  return {
    scope_id: cfg.scopeId,
    title: cfg.title,
    canvas_width_px: Math.max(128, Math.round(canvasWidthPx || 820)),
    time_span_ns: cfg.timeSpanNs,
    pre_trigger_ratio: cfg.preTriggerRatio,
    pan_offset_ns: cfg.panOffsetNs,
    sweep_mode: cfg.sweepMode,
    auto_timeout_ms: cfg.autoTimeoutMs,
    trigger: cfg.trigger,
    tracks: cfg.tracks.map((t) => ({
      channel_id: t.channelId,
      lane: t.lane,
      visible: t.visible,
      y_scale: t.yScale,
      y_offset: t.yOffset,
      color_rgba: t.colorRgba,
    })),
    sync_group_id: cfg.syncGroupId,
    paused: cfg.paused,
  };
}

function defaultLaneForChannel(ch: ChannelNodeDto_Serialize): ScopeLaneKindDto {
  if (ch.modality === "digital_bool") return "digital";
  if (ch.modality === "state_machine") return "state_ribbon";
  return "analog";
}

function defaultYScaleAndOffset(ch: ChannelNodeDto_Serialize): {
  yScale: number;
  yOffset: number;
} {
  if (
    ch.local_path === "v_bus" ||
    (ch.source_id === "sim0" && ch.local_path === "ch3")
  ) {
    return { yScale: 25, yOffset: 50 };
  }
  if (
    ch.local_path === "ia" ||
    ch.local_path === "ib" ||
    ch.local_path === "ic" ||
    (ch.source_id === "sim0" &&
      (ch.local_path === "ch0" || ch.local_path === "ch1" || ch.local_path === "ch2"))
  ) {
    return { yScale: 5, yOffset: 0 };
  }
  return { yScale: 2.5, yOffset: 0 };
}

function buildDefaultScopeConfigs(
  channelsById: Record<number, ChannelNodeDto_Serialize>,
  locale: UiLocale,
): Record<number, ScopeUiConfig> {
  const allChannels = Object.values(channelsById);
  const byPath = new Map<string, ChannelNodeDto_Serialize>();
  for (const c of allChannels) {
    byPath.set(c.full_path, c);
  }
  const findFirst = (...paths: string[]): ChannelNodeDto_Serialize | undefined => {
    for (const p of paths) {
      const found = byPath.get(p);
      if (found) return found;
    }
    return undefined;
  };

  if (allChannels.length === 0) {
    const scope1Empty: ScopeUiConfig = {
      scopeId: 1,
      title: getDefaultScopeTitle("scope1", locale),
      timeSpanNs: 20_000_000,
      preTriggerRatio: 0.25,
      panOffsetNs: 0,
      sweepMode: "roll",
      autoTimeoutMs: 120,
      trigger: null,
      tracks: [],
      syncGroupId: null,
      paused: false,
      analogLayout: "overlay",
      cursorsEnabled: false,
      cursorARatio: 0.28,
      cursorBRatio: 0.68,
      activeAnalogChannelId: null,
    };
    const scope2Empty: ScopeUiConfig = {
      scopeId: 2,
      title: getDefaultScopeTitle("scope2", locale),
      timeSpanNs: 10_000_000,
      preTriggerRatio: 0.25,
      panOffsetNs: 0,
      sweepMode: "single",
      autoTimeoutMs: 120,
      trigger: null,
      tracks: [],
      syncGroupId: null,
      paused: false,
      analogLayout: "overlay",
      cursorsEnabled: false,
      cursorARatio: 0.25,
      cursorBRatio: 0.55,
      activeAnalogChannelId: null,
    };
    return { 1: scope1Empty, 2: scope2Empty };
  }

  const chIa =
    findFirst("sim0/ia", "sim0/ch0") ??
    allChannels.find((c) => c.modality === "scalar");
  const chIb =
    findFirst("sim0/ib", "sim0/ch1") ??
    allChannels.find(
      (c) => c.modality === "scalar" && c.channel_id !== chIa?.channel_id,
    );
  const chVbus =
    findFirst("sim0/v_bus", "sim0/ch3") ??
    allChannels.find(
      (c) =>
        c.modality === "scalar" &&
        c.channel_id !== chIa?.channel_id &&
        c.channel_id !== chIb?.channel_id,
    );
  const chPwmA =
    findFirst("sim0/pwm_a") ??
    allChannels.find((c) => c.modality === "digital_bool");
  const chPwmB =
    findFirst("sim0/pwm_b") ??
    allChannels.find(
      (c) => c.modality === "digital_bool" && c.channel_id !== chPwmA?.channel_id,
    );
  const chFault =
    findFirst("sim0/fault_pin") ??
    allChannels.find(
      (c) =>
        c.modality === "digital_bool" &&
        c.channel_id !== chPwmA?.channel_id &&
        c.channel_id !== chPwmB?.channel_id,
    );
  const chState =
    findFirst("sim0/ctrl_state") ??
    allChannels.find((c) => c.modality === "state_machine");

  const scope1Tracks: ScopeTrackUiBinding[] = [];
  if (chIa) {
    scope1Tracks.push({
      channelId: chIa.channel_id,
      lane: "analog",
      visible: true,
      ...defaultYScaleAndOffset(chIa),
      colorRgba: 0x38bdf8ff,
    });
  }
  if (chIb) {
    scope1Tracks.push({
      channelId: chIb.channel_id,
      lane: "analog",
      visible: true,
      ...defaultYScaleAndOffset(chIb),
      colorRgba: 0x34d399ff,
    });
  }
  if (chVbus) {
    scope1Tracks.push({
      channelId: chVbus.channel_id,
      lane: "analog",
      visible: true,
      ...defaultYScaleAndOffset(chVbus),
      colorRgba: 0xf59e0bff,
    });
  }
  if (chPwmA) {
    scope1Tracks.push({
      channelId: chPwmA.channel_id,
      lane: "digital",
      visible: true,
      yScale: 1,
      yOffset: 0,
      colorRgba: 0x22d3eeff,
    });
  }
  if (chPwmB) {
    scope1Tracks.push({
      channelId: chPwmB.channel_id,
      lane: "digital",
      visible: true,
      yScale: 1,
      yOffset: 0,
      colorRgba: 0xa78bfaff,
    });
  }
  if (chFault) {
    scope1Tracks.push({
      channelId: chFault.channel_id,
      lane: "digital",
      visible: true,
      yScale: 1,
      yOffset: 0,
      colorRgba: 0xfb7185ff,
    });
  }
  if (chState) {
    scope1Tracks.push({
      channelId: chState.channel_id,
      lane: "state_ribbon",
      visible: true,
      yScale: 1,
      yOffset: 0,
      colorRgba: 0x10b981ff,
    });
  }

  const scope1: ScopeUiConfig = {
    scopeId: 1,
    title: getDefaultScopeTitle("scope1", locale),
    timeSpanNs: 20_000_000, // 20 ms (2 ms/div)
    preTriggerRatio: 0.25,
    panOffsetNs: 0,
    sweepMode: "roll",
    autoTimeoutMs: 120,
    trigger: chIa
      ? {
          kind: "edge",
          channel_id: chIa.channel_id,
          level: 0,
          hysteresis: 0.5,
          slope: "rising",
        }
      : null,
    tracks: scope1Tracks,
    syncGroupId: null,
    paused: false,
    analogLayout: "overlay",
    cursorsEnabled: false,
    cursorARatio: 0.28,
    cursorBRatio: 0.68,
    activeAnalogChannelId: chIa?.channel_id ?? null,
  };

  const scope2TriggerCh = chVbus ?? chIa;
  const scope2Tracks: ScopeTrackUiBinding[] = [];
  if (scope2TriggerCh) {
    scope2Tracks.push({
      channelId: scope2TriggerCh.channel_id,
      lane: "analog",
      visible: true,
      ...defaultYScaleAndOffset(scope2TriggerCh),
      colorRgba: 0xf59e0bff,
    });
  }
  if (chFault) {
    scope2Tracks.push({
      channelId: chFault.channel_id,
      lane: "digital",
      visible: true,
      yScale: 1,
      yOffset: 0,
      colorRgba: 0xfb7185ff,
    });
  }
  if (chState) {
    scope2Tracks.push({
      channelId: chState.channel_id,
      lane: "state_ribbon",
      visible: true,
      yScale: 1,
      yOffset: 0,
      colorRgba: 0x10b981ff,
    });
  }

  const scope2: ScopeUiConfig = {
    scopeId: 2,
    title: getDefaultScopeTitle("scope2", locale),
    timeSpanNs: 10_000_000, // 10 ms (1 ms/div)
    preTriggerRatio: 0.25,
    panOffsetNs: 0,
    sweepMode: "single",
    autoTimeoutMs: 120,
    trigger: scope2TriggerCh
      ? {
          kind: "edge",
          channel_id: scope2TriggerCh.channel_id,
          level: chVbus ? 85 : 0,
          hysteresis: 2.0,
          slope: "rising",
        }
      : null,
    tracks: scope2Tracks,
    syncGroupId: null,
    paused: false,
    analogLayout: "overlay",
    cursorsEnabled: false,
    cursorARatio: 0.25,
    cursorBRatio: 0.55,
    activeAnalogChannelId: scope2TriggerCh?.channel_id ?? null,
  };

  return { 1: scope1, 2: scope2 };
}

export function createWorkspaceStore(
  client: BridgeClient = bridgeClient,
  fetchStatus: () => Promise<RuntimeStatus> = fetchEngineStatus,
): UseBoundStore<StoreApi<WorkspaceStoreState>> {
  let nextTxLogId = 1;
  let telemetryUnsub: (() => void) | null = null;
  let metricsUnsub: (() => void) | null = null;

  return create<WorkspaceStoreState>((set, get) => ({
    initialized: false,
    initError: null,
    runtimeStatus: null,
    channelGroups: [],
    channelsById: {},
    telemetryBySource: {},
    quota: {
      usedBytes: 0,
      limitBytes: 2 * 1024 * 1024 * 1024,
      pinnedBytes: 0,
    },
    dataPlaneMetrics: {
      ddsfFps: 0,
      bytesPerSec: 0,
      frameSeq: 0,
      scopeCount: 0,
      snapshotCount: 0,
      quotaWarning: false,
      pinnedActive: false,
      parseErrors: 0,
    },
    downlinkStats: {
      pendingQueueDepth: 0,
      totalDispatched: 0,
      coalescedOverwrites: 0,
      motorKp: 1,
      motorFreqHz: 50,
    },
    sliderEventCount: 0,
    txLog: [],

    scopes: {},
    scopeOrder: [],
    focusedScopeId: 1,
    nextScopeId: 3,
    canvasWidthsByScope: { 1: 760, 2: 760 },

    locale: detectInitialLocale(),
    mockRateHz: 120,
    sidebarCollapsed: false,
    downlinkBarCollapsed: false,
    sourceWizardOpen: false,

    setLocale: (nextLocale) => {
      persistLocale(nextLocale);
      const state = get();
      const nextScopes: Record<number, ScopeUiConfig> = {};
      let changed = false;
      for (const [idStr, sc] of Object.entries(state.scopes)) {
        const nextTitle = translateDefaultScopeTitle(sc.title, nextLocale);
        if (nextTitle !== sc.title) {
          changed = true;
          nextScopes[Number(idStr)] = { ...sc, title: nextTitle };
        } else {
          nextScopes[Number(idStr)] = sc;
        }
      }
      set({
        locale: nextLocale,
        ...(changed ? { scopes: nextScopes } : {}),
      });
    },

    initWorkspace: async () => {
      if (get().initialized) return;
      persistLocale(get().locale);
      try {
        await client.startDataPlane(get().mockRateHz);
        const rawStatus = await fetchStatus();
        const status: RuntimeStatus =
          rawStatus.mode === "tauri" && client.isMockMode()
            ? {
                ...rawStatus,
                mode: "browser-mock",
              }
            : rawStatus;

        if (!telemetryUnsub) {
          telemetryUnsub = client.subscribeTelemetry((telemetry, downlinkStats) => {
            const telMap: Record<string, SourceTelemetryDto> = {};
            for (const s of telemetry.sources) {
              telMap[s.source_id] = s;
            }
            set({
              telemetryBySource: telMap,
              quota: {
                usedBytes: telemetry.quota_used_bytes ?? 0,
                limitBytes: telemetry.quota_limit_bytes ?? 2 * 1024 * 1024 * 1024,
                pinnedBytes: telemetry.pinned_bytes ?? 0,
              },
              downlinkStats,
            });
          });
        }

        if (!metricsUnsub) {
          metricsUnsub = client.topicBus.subscribeMetrics((metrics) => {
            set({ dataPlaneMetrics: metrics });
          });
        }

        const tree = await client.getChannelTree();
        const chMap: Record<number, ChannelNodeDto_Serialize> = {};
        for (const grp of tree.sources) {
          for (const ch of grp.channels) {
            chMap[ch.channel_id] = ch;
          }
        }

        const defaultScopes = buildDefaultScopeConfigs(chMap, get().locale);
        await Promise.all(
          Object.values(defaultScopes).map((sc) =>
            client.upsertScopeViewport(toViewportDto(sc, 760)),
          ),
        );

        set({
          initialized: true,
          initError: null,
          runtimeStatus: status,
          channelGroups: tree.sources,
          channelsById: chMap,
          quota: {
            usedBytes: tree.quota_used_bytes ?? 0,
            limitBytes: tree.quota_limit_bytes ?? 2 * 1024 * 1024 * 1024,
            pinnedBytes: tree.pinned_bytes ?? 0,
          },
          scopes: defaultScopes,
          scopeOrder: [1, 2],
          focusedScopeId: 1,
          nextScopeId: 3,
        });
      } catch (err: unknown) {
        set({
          initError: err instanceof Error ? err.message : String(err),
        });
      }
    },

    refreshChannelTree: async () => {
      const tree = await client.getChannelTree();
      const chMap: Record<number, ChannelNodeDto_Serialize> = {};
      for (const grp of tree.sources) {
        for (const ch of grp.channels) {
          chMap[ch.channel_id] = ch;
        }
      }
      set({
        channelGroups: tree.sources,
        channelsById: chMap,
        quota: {
          usedBytes: tree.quota_used_bytes ?? 0,
          limitBytes: tree.quota_limit_bytes ?? 2 * 1024 * 1024 * 1024,
          pinnedBytes: tree.pinned_bytes ?? 0,
        },
      });
    },

    setMockRateHz: (rate) => {
      client.setMockRate(rate);
      set({ mockRateHz: rate });
    },

    setSidebarCollapsed: (collapsed) => set({ sidebarCollapsed: collapsed }),
    setDownlinkBarCollapsed: (collapsed) => set({ downlinkBarCollapsed: collapsed }),
    setSourceWizardOpen: (open) => set({ sourceWizardOpen: open }),
    setFocusedScopeId: (scopeId) => set({ focusedScopeId: scopeId }),

    createSource: async (req) => {
      await client.createSource(req);
      await get().refreshChannelTree();
    },

    startSource: async (sourceId) => {
      await client.startSource(sourceId);
      await get().refreshChannelTree();
    },

    stopSource: async (sourceId) => {
      await client.stopSource(sourceId);
      await get().refreshChannelTree();
    },

    removeSource: async (sourceId) => {
      await client.removeSource(sourceId);
      await get().refreshChannelTree();
    },

    toggleRawRecorder: async (sourceId, enabled) => {
      await client.setRawRecorder({
        source_id: sourceId,
        enabled,
        output_path: null,
      });
      await get().refreshChannelTree();
    },

    seekReplay: async (req) => {
      await client.seekReplaySource(req);
      await get().refreshChannelTree();
    },

    addScope: (title) => {
      const state = get();
      const scopeId = state.nextScopeId;
      const allChannels = Object.values(state.channelsById);
      const firstAnalog = allChannels.find((c) => c.modality === "scalar");

      const tracks: ScopeTrackUiBinding[] = firstAnalog
        ? [
            {
              channelId: firstAnalog.channel_id,
              lane: "analog",
              visible: true,
              ...defaultYScaleAndOffset(firstAnalog),
              colorRgba: TRACE_COLORS[(scopeId - 1) % TRACE_COLORS.length],
            },
          ]
        : [];

      const newScope: ScopeUiConfig = {
        scopeId,
        title: title ?? getDefaultScopeTitle("generic", state.locale, scopeId),
        timeSpanNs: 20_000_000,
        preTriggerRatio: 0.25,
        panOffsetNs: 0,
        sweepMode: "auto",
        autoTimeoutMs: 120,
        trigger: firstAnalog
          ? {
              kind: "edge",
              channel_id: firstAnalog.channel_id,
              level: 0,
              hysteresis: 0.5,
              slope: "rising",
            }
          : null,
        tracks,
        syncGroupId: null,
        paused: false,
        analogLayout: "overlay",
        cursorsEnabled: false,
        cursorARatio: 0.28,
        cursorBRatio: 0.68,
        activeAnalogChannelId: firstAnalog?.channel_id ?? null,
      };

      set({
        scopes: { ...state.scopes, [scopeId]: newScope },
        scopeOrder: [...state.scopeOrder, scopeId],
        focusedScopeId: scopeId,
        nextScopeId: scopeId + 1,
      });

      client
        .upsertScopeViewport(toViewportDto(newScope, 760))
        .catch(() => {});

      return scopeId;
    },

    removeScope: (scopeId) => {
      const state = get();
      if (!state.scopes[scopeId]) return;
      const nextScopes = { ...state.scopes };
      delete nextScopes[scopeId];
      const nextOrder = state.scopeOrder.filter((id) => id !== scopeId);
      const nextFocused =
        state.focusedScopeId === scopeId ? (nextOrder[0] ?? null) : state.focusedScopeId;

      set({
        scopes: nextScopes,
        scopeOrder: nextOrder,
        focusedScopeId: nextFocused,
      });
      client.topicBus.deleteScopeCache(scopeId);
      client.removeScopeViewport(scopeId).catch(() => {});
    },

    updateScope: (scopeId, updater) => {
      const state = get();
      const prev = state.scopes[scopeId];
      if (!prev) return;
      let next = updater(prev);
      const canvasW = state.canvasWidthsByScope[scopeId] ?? 760;

      const joinedSyncGroup =
        next.syncGroupId !== null && next.syncGroupId !== prev.syncGroupId;
      if (joinedSyncGroup) {
        const peer = Object.values(state.scopes).find(
          (sc) => sc.scopeId !== scopeId && sc.syncGroupId === next.syncGroupId,
        );
        if (peer) {
          next = {
            ...next,
            timeSpanNs: peer.timeSpanNs,
            panOffsetNs: peer.panOffsetNs,
            cursorsEnabled: peer.cursorsEnabled,
            cursorARatio: peer.cursorARatio,
            cursorBRatio: peer.cursorBRatio,
          };
        }
      }

      set({
        scopes: {
          ...state.scopes,
          [scopeId]: next,
        },
      });

      client
        .upsertScopeViewport(toViewportDto(next, canvasW))
        .then(() => {
          if (joinedSyncGroup && next.syncGroupId !== null) {
            return client.updateSyncGroup({
              group_id: next.syncGroupId,
              origin_scope_id: scopeId,
              time_span_ns: next.timeSpanNs,
              center_offset_ns: next.panOffsetNs,
              cursor_a_ratio: next.cursorARatio,
              cursor_b_ratio: next.cursorBRatio,
            });
          }
          return null;
        })
        .catch(() => {});
    },

    setScopeCanvasWidth: (scopeId, widthPx) => {
      const state = get();
      const prevW = state.canvasWidthsByScope[scopeId] ?? 0;
      if (Math.abs(prevW - widthPx) < 24) return;
      const scope = state.scopes[scopeId];
      if (!scope) return;

      set({
        canvasWidthsByScope: {
          ...state.canvasWidthsByScope,
          [scopeId]: widthPx,
        },
      });
      client
        .upsertScopeViewport(toViewportDto(scope, widthPx))
        .catch(() => {});
    },

    rearmScope: (scopeId) => {
      get().updateScope(scopeId, (prev) => ({
        ...prev,
        paused: false,
        panOffsetNs: 0,
      }));
      client.rearmScope(scopeId).catch(() => {});
    },

    bindChannelToScope: (scopeId, channelId) => {
      const state = get();
      const ch = state.channelsById[channelId];
      const scope = state.scopes[scopeId];
      if (!ch || !scope) return;

      if (scope.tracks.some((t) => t.channelId === channelId)) {
        get().updateScope(scopeId, (prev) => ({
          ...prev,
          tracks: prev.tracks.map((t) =>
            t.channelId === channelId ? { ...t, visible: true } : t,
          ),
        }));
        return;
      }

      const lane = defaultLaneForChannel(ch);
      const { yScale, yOffset } = defaultYScaleAndOffset(ch);
      const colorRgba = TRACE_COLORS[scope.tracks.length % TRACE_COLORS.length];

      get().updateScope(scopeId, (prev) => ({
        ...prev,
        activeAnalogChannelId:
          lane === "analog" && prev.activeAnalogChannelId === null
            ? channelId
            : prev.activeAnalogChannelId,
        trigger:
          prev.trigger ??
          (lane === "analog"
            ? {
                kind: "edge",
                channel_id: channelId,
                level: yOffset,
                hysteresis: 0.5,
                slope: "rising",
              }
            : null),
        tracks: [
          ...prev.tracks,
          {
            channelId,
            lane,
            visible: true,
            yScale,
            yOffset,
            colorRgba,
          },
        ],
      }));
    },

    broadcastSyncGroup: (groupId, originScopeId, patch) => {
      const state = get();
      const origin = state.scopes[originScopeId];
      const resolvedSpanNs = patch.timeSpanNs ?? origin?.timeSpanNs ?? 20_000_000;
      const resolvedPanNs = patch.panOffsetNs ?? origin?.panOffsetNs ?? 0;
      const resolvedCursorA = patch.cursorARatio ?? origin?.cursorARatio ?? null;
      const resolvedCursorB = patch.cursorBRatio ?? origin?.cursorBRatio ?? null;

      const nextScopes = { ...state.scopes };
      let changed = false;

      for (const idStr of Object.keys(nextScopes)) {
        const id = Number(idStr);
        const sc = nextScopes[id];
        if (sc.syncGroupId === groupId && id !== originScopeId) {
          changed = true;
          nextScopes[id] = {
            ...sc,
            timeSpanNs: patch.timeSpanNs ?? sc.timeSpanNs,
            panOffsetNs: patch.panOffsetNs ?? sc.panOffsetNs,
            cursorsEnabled:
              patch.cursorsEnabled !== undefined
                ? patch.cursorsEnabled
                : patch.cursorARatio !== undefined || patch.cursorBRatio !== undefined
                  ? true
                  : sc.cursorsEnabled,
            cursorARatio: patch.cursorARatio ?? sc.cursorARatio,
            cursorBRatio: patch.cursorBRatio ?? sc.cursorBRatio,
          };
        }
      }

      if (changed) {
        set({ scopes: nextScopes });
      }

      client
        .updateSyncGroup({
          group_id: groupId,
          origin_scope_id: originScopeId,
          time_span_ns: resolvedSpanNs,
          center_offset_ns: resolvedPanNs,
          cursor_a_ratio: resolvedCursorA,
          cursor_b_ratio: resolvedCursorB,
        })
        .catch(() => {});
    },

    sendDownlink: async (req, labelSummary, isSliderEvent = false) => {
      if (isSliderEvent) {
        set((s) => ({ sliderEventCount: s.sliderEventCount + 1 }));
      }
      const receipt = await client.sendDownlink(req);

      const now = new Date();
      const ts = `${String(now.getHours()).padStart(2, "0")}:${String(
        now.getMinutes(),
      ).padStart(2, "0")}:${String(now.getSeconds()).padStart(2, "0")}.${String(
        now.getMilliseconds(),
      ).padStart(3, "0")}`;

      const entry: DownlinkLogEntry = {
        id: nextTxLogId++,
        timestamp: ts,
        sourceId: req.source_id,
        summary: labelSummary ?? req.payload.kind,
        coalesced: receipt.coalesced_existing,
        resultKind: receipt.result_kind,
      };

      set((s) => ({
        txLog: [entry, ...s.txLog].slice(0, 24),
        ...(client.isMockMode() ? {} : { downlinkStats: client.getDownlinkStats() }),
      }));

      return receipt;
    },

    injectGlitch: async () => {
      const groups = get().channelGroups;
      const runningSimIds = groups
        .filter((g) => g.source.transport_kind === "sim" && g.source.state === "running")
        .map((g) => g.source.source_id);
      const allSimIds =
        runningSimIds.length > 0
          ? runningSimIds
          : groups
              .filter((g) => g.source.transport_kind === "sim")
              .map((g) => g.source.source_id);
      await client.injectGlitch(allSimIds.length > 0 ? allSimIds : undefined);
    },
  }));
}

export const useWorkspaceStore = createWorkspaceStore();
