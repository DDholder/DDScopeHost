import { create } from "zustand";
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
  if (ch.local_path === "v_bus") {
    return { yScale: 25, yOffset: 50 };
  }
  if (ch.local_path === "ia" || ch.local_path === "ib" || ch.local_path === "ic") {
    return { yScale: 5, yOffset: 0 };
  }
  return { yScale: 2.5, yOffset: 0 };
}

function buildDefaultScopeConfigs(
  channelsById: Record<number, ChannelNodeDto_Serialize>,
  locale: UiLocale,
): Record<number, ScopeUiConfig> {
  const byPath = new Map<string, ChannelNodeDto_Serialize>();
  Object.values(channelsById).forEach((c) => {
    byPath.set(c.full_path, c);
  });

  const chIa = byPath.get("sim0/ia")?.channel_id ?? 101;
  const chIb = byPath.get("sim0/ib")?.channel_id ?? 102;
  const chVbus = byPath.get("sim0/v_bus")?.channel_id ?? 104;
  const chPwmA = byPath.get("sim0/pwm_a")?.channel_id ?? 105;
  const chPwmB = byPath.get("sim0/pwm_b")?.channel_id ?? 106;
  const chFault = byPath.get("sim0/fault_pin")?.channel_id ?? 107;
  const chState = byPath.get("sim0/ctrl_state")?.channel_id ?? 108;

  const scope1: ScopeUiConfig = {
    scopeId: 1,
    title: getDefaultScopeTitle("scope1", locale),
    timeSpanNs: 20_000_000, // 20 ms (2 ms/div)
    preTriggerRatio: 0.25,
    panOffsetNs: 0,
    sweepMode: "roll",
    autoTimeoutMs: 120,
    trigger: {
      kind: "edge",
      channel_id: chIa,
      level: 0,
      hysteresis: 0.5,
      slope: "rising",
    },
    tracks: [
      { channelId: chIa, lane: "analog", visible: true, yScale: 5, yOffset: 0, colorRgba: 0x38bdf8ff },
      { channelId: chIb, lane: "analog", visible: true, yScale: 5, yOffset: 0, colorRgba: 0x34d399ff },
      { channelId: chVbus, lane: "analog", visible: true, yScale: 25, yOffset: 50, colorRgba: 0xf59e0bff },
      { channelId: chPwmA, lane: "digital", visible: true, yScale: 1, yOffset: 0, colorRgba: 0x22d3eeff },
      { channelId: chPwmB, lane: "digital", visible: true, yScale: 1, yOffset: 0, colorRgba: 0xa78bfaff },
      { channelId: chFault, lane: "digital", visible: true, yScale: 1, yOffset: 0, colorRgba: 0xfb7185ff },
      { channelId: chState, lane: "state_ribbon", visible: true, yScale: 1, yOffset: 0, colorRgba: 0x10b981ff },
    ],
    syncGroupId: null,
    paused: false,
    analogLayout: "overlay",
    cursorsEnabled: false,
    cursorARatio: 0.28,
    cursorBRatio: 0.68,
    activeAnalogChannelId: chIa,
  };

  const scope2: ScopeUiConfig = {
    scopeId: 2,
    title: getDefaultScopeTitle("scope2", locale),
    timeSpanNs: 10_000_000, // 10 ms (1 ms/div)
    preTriggerRatio: 0.25,
    panOffsetNs: 0,
    sweepMode: "single",
    autoTimeoutMs: 120,
    trigger: {
      kind: "edge",
      channel_id: chVbus,
      level: 85,
      hysteresis: 2.0,
      slope: "rising",
    },
    tracks: [
      { channelId: chVbus, lane: "analog", visible: true, yScale: 25, yOffset: 50, colorRgba: 0xf59e0bff },
      { channelId: chFault, lane: "digital", visible: true, yScale: 1, yOffset: 0, colorRgba: 0xfb7185ff },
      { channelId: chState, lane: "state_ribbon", visible: true, yScale: 1, yOffset: 0, colorRgba: 0x10b981ff },
    ],
    syncGroupId: null,
    paused: false,
    analogLayout: "overlay",
    cursorsEnabled: false,
    cursorARatio: 0.25,
    cursorBRatio: 0.55,
    activeAnalogChannelId: chVbus,
  };

  return { 1: scope1, 2: scope2 };
}

let nextTxLogId = 1;
let telemetryUnsub: (() => void) | null = null;
let metricsUnsub: (() => void) | null = null;

export const useWorkspaceStore = create<WorkspaceStoreState>((set, get) => ({
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
      const status = await fetchEngineStatus();
      bridgeClient.startDataPlane(get().mockRateHz);

      if (!telemetryUnsub) {
        telemetryUnsub = bridgeClient.subscribeTelemetry((telemetry, downlinkStats) => {
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
        metricsUnsub = bridgeClient.topicBus.subscribeMetrics((metrics) => {
          set({ dataPlaneMetrics: metrics });
        });
      }

      const tree = await bridgeClient.getChannelTree();
      const chMap: Record<number, ChannelNodeDto_Serialize> = {};
      for (const grp of tree.sources) {
        for (const ch of grp.channels) {
          chMap[ch.channel_id] = ch;
        }
      }

      const defaultScopes = buildDefaultScopeConfigs(chMap, get().locale);
      await Promise.all(
        Object.values(defaultScopes).map((sc) =>
          bridgeClient.upsertScopeViewport(toViewportDto(sc, 760)),
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
    const tree = await bridgeClient.getChannelTree();
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
    bridgeClient.setMockRate(rate);
    set({ mockRateHz: rate });
  },

  setSidebarCollapsed: (collapsed) => set({ sidebarCollapsed: collapsed }),
  setDownlinkBarCollapsed: (collapsed) => set({ downlinkBarCollapsed: collapsed }),
  setSourceWizardOpen: (open) => set({ sourceWizardOpen: open }),
  setFocusedScopeId: (scopeId) => set({ focusedScopeId: scopeId }),

  createSource: async (req) => {
    await bridgeClient.createSource(req);
    await get().refreshChannelTree();
  },

  startSource: async (sourceId) => {
    await bridgeClient.startSource(sourceId);
    await get().refreshChannelTree();
  },

  stopSource: async (sourceId) => {
    await bridgeClient.stopSource(sourceId);
    await get().refreshChannelTree();
  },

  removeSource: async (sourceId) => {
    await bridgeClient.removeSource(sourceId);
    await get().refreshChannelTree();
  },

  toggleRawRecorder: async (sourceId, enabled) => {
    await bridgeClient.setRawRecorder({
      source_id: sourceId,
      enabled,
      output_path: null,
    });
    await get().refreshChannelTree();
  },

  seekReplay: async (req) => {
    await bridgeClient.seekReplaySource(req);
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

    bridgeClient
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
    bridgeClient.topicBus.deleteScopeCache(scopeId);
    bridgeClient.removeScopeViewport(scopeId).catch(() => {});
  },

  updateScope: (scopeId, updater) => {
    const state = get();
    const prev = state.scopes[scopeId];
    if (!prev) return;
    const next = updater(prev);
    const canvasW = state.canvasWidthsByScope[scopeId] ?? 760;

    set({
      scopes: {
        ...state.scopes,
        [scopeId]: next,
      },
    });

    bridgeClient
      .upsertScopeViewport(toViewportDto(next, canvasW))
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
    bridgeClient
      .upsertScopeViewport(toViewportDto(scope, widthPx))
      .catch(() => {});
  },

  rearmScope: (scopeId) => {
    get().updateScope(scopeId, (prev) => ({
      ...prev,
      panOffsetNs: 0,
    }));
    bridgeClient.rearmScope(scopeId).catch(() => {});
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
          cursorARatio: patch.cursorARatio ?? sc.cursorARatio,
          cursorBRatio: patch.cursorBRatio ?? sc.cursorBRatio,
        };
      }
    }

    if (changed) {
      set({ scopes: nextScopes });
    }

    bridgeClient
      .updateSyncGroup({
        group_id: groupId,
        origin_scope_id: originScopeId,
        time_span_ns: patch.timeSpanNs ?? null,
        center_offset_ns: patch.panOffsetNs ?? null,
        cursor_a_ratio: patch.cursorARatio ?? null,
        cursor_b_ratio: patch.cursorBRatio ?? null,
      })
      .catch(() => {});
  },

  sendDownlink: async (req, labelSummary, isSliderEvent = false) => {
    if (isSliderEvent) {
      set((s) => ({ sliderEventCount: s.sliderEventCount + 1 }));
    }
    const receipt = await bridgeClient.sendDownlink(req);

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
    }));

    return receipt;
  },

  injectGlitch: async () => {
    await bridgeClient.injectMockGlitch();
  },
}));
