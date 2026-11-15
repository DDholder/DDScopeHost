import { create, type StoreApi, type UseBoundStore } from "zustand";
import type {
  ChannelNodeDto_Serialize,
  CreateSourceRequestDto_Deserialize,
  CreateSourceRequestDto_Serialize,
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
  getDefaultCanvasTitle,
  getDefaultScopeTitle,
  persistLocale,
  translate,
  translateDefaultCanvasTitle,
  translateDefaultScopeTitle,
  type MessageKey,
  type UiLocale,
} from "../i18n/index.ts";
import type {
  ScopeTrackUiBinding,
  ScopeUiConfig,
} from "../scope-engine/types.ts";
import {
  clampGridRect,
  defaultSizeForWidgetType,
  findFreeGridSlot,
  type DashboardCanvasConfig,
  type DashboardWidgetItem,
  type WidgetType,
} from "../widgets/types.ts";
import {
  DEFAULT_DOWNLINK_UI_CONFIG,
  normalizeSourceConfigForExport,
  parseBoardConfigJson,
  parseCommandsConfigJson,
  parseProjectConfigJson,
  parseSourcesConfigJson,
  projectToBoardFile,
  rehydrateBoardFromConfig,
  serializeBoardConfig,
  serializeCommandsConfig,
  serializeProjectConfig,
  serializeSourcesConfig,
  synthesizeFallbackSourceConfig,
  type BoardConfigFileV1,
  type DownlinkUiConfig,
  type ProjectConfigFileV1,
} from "./config-io.ts";
import {
  commandUsesValue,
  compileCommandPayload,
  type CommandDefinition,
} from "./command-system.ts";

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
  sourceConfigsById: Record<string, CreateSourceRequestDto_Serialize>;
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

  canvases: Record<number, DashboardCanvasConfig>;
  canvasOrder: number[];
  activeCanvasId: number | null;
  nextCanvasId: number;
  nextWidgetSeq: number;

  commands: CommandDefinition[];
  nextCommandSeq: number;

  locale: UiLocale;
  mockRateHz: 60 | 120;
  sidebarCollapsed: boolean;
  sidebarTab: "sources" | "toolbox" | "commands";
  downlinkBarCollapsed: boolean;
  sourceWizardOpen: boolean;
  downlinkConfig: DownlinkUiConfig;
  pendingDockviewLayout: Record<string, unknown> | null;
  layoutRestoreSeq: number;

  setLocale: (locale: UiLocale) => void;
  initWorkspace: () => Promise<void>;
  refreshChannelTree: () => Promise<void>;
  setMockRateHz: (rate: 60 | 120) => void;
  setSidebarCollapsed: (collapsed: boolean) => void;
  setSidebarTab: (tab: "sources" | "toolbox" | "commands") => void;
  setDownlinkBarCollapsed: (collapsed: boolean) => void;
  setSourceWizardOpen: (open: boolean) => void;
  setFocusedScopeId: (scopeId: number | null) => void;
  updateDownlinkConfig: (patch: Partial<DownlinkUiConfig>) => void;
  registerDockviewLayoutAccessor: (
    accessor: (() => Record<string, unknown> | null) | null,
  ) => void;

  createSource: (req: CreateSourceRequestDto_Deserialize) => Promise<void>;
  startSource: (sourceId: string) => Promise<void>;
  stopSource: (sourceId: string) => Promise<void>;
  removeSource: (sourceId: string) => Promise<void>;
  toggleRawRecorder: (sourceId: string, enabled: boolean) => Promise<void>;
  seekReplay: (req: SeekReplayRequestDto_Deserialize) => Promise<void>;
  exportSourcesConfig: (sourceIds?: string[]) => string;
  importSourcesConfig: (
    jsonText: string,
    options?: { replaceExisting?: boolean },
  ) => Promise<{ importedIds: string[]; skippedIds: string[] }>;

  addScope: (title?: string, options?: { dockInStage?: boolean }) => number;
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

  addCanvas: (title?: string) => number;
  removeCanvas: (canvasId: number) => void;
  setCanvasOrder: (order: number[]) => void;
  setActiveCanvasId: (canvasId: number | null) => void;
  toggleCanvasLock: (canvasId: number) => void;
  clearCanvasWidgets: (canvasId: number) => void;
  addWidgetToCanvas: (
    canvasId: number,
    widgetType: WidgetType,
    opts?: { col?: number; row?: number; channelId?: number },
  ) => string;
  updateCanvasWidget: (
    canvasId: number,
    widgetId: string,
    updater: (prev: DashboardWidgetItem) => DashboardWidgetItem,
  ) => void;
  removeCanvasWidget: (canvasId: number, widgetId: string) => void;
  bindChannelToWidget: (canvasId: number, widgetId: string, channelId: number) => void;
  exportBoardConfig: () => string;
  importBoardConfig: (jsonText: string) => Promise<{
    scopeCount: number;
    canvasCount: number;
    widgetCount: number;
    missingChannelPaths: string[];
  }>;

  exportProjectConfig: () => string;
  importProjectConfig: (jsonText: string) => Promise<{
    sourceCount: number;
    scopeCount: number;
    canvasCount: number;
    widgetCount: number;
    missingChannelPaths: string[];
  }>;
  saveDefaultProjectConfig: () => Promise<{
    location: string;
    mode: "exe-dir" | "browser-storage";
  }>;
  clearDefaultProjectConfig: () => Promise<void>;

  addCommand: (partial?: Partial<Omit<CommandDefinition, "id">>) => string;
  updateCommand: (commandId: string, patch: Partial<Omit<CommandDefinition, "id">>) => void;
  removeCommand: (commandId: string) => void;
  bindCommandToWidget: (canvasId: number, widgetId: string, commandId: string) => void;
  placeCommandOnCanvas: (
    canvasId: number,
    commandId: string,
    opts?: { col?: number; row?: number },
  ) => string;
  executeCommand: (
    commandId: string,
    opts?: {
      value?: number;
      fallbackSourceId?: string;
      coalesceKey?: string | null;
      isSliderEvent?: boolean;
    },
  ) => Promise<DownlinkReceiptDto>;
  exportCommandsConfig: () => string;
  importCommandsConfig: (
    jsonText: string,
    options?: { replaceExisting?: boolean },
  ) => { importedCount: number };

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

function defaultYScaleAndOffset(_ch: ChannelNodeDto_Serialize): {
  yScale: number;
  yOffset: number;
} {
  return { yScale: 1, yOffset: 0 };
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
      yAxisScale: 15,
      yAxisOffset: 0,
      wheelZoomTarget: "x",
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
      yAxisScale: 25,
      yAxisOffset: 50,
      wheelZoomTarget: "x",
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
      yScale: 1,
      yOffset: 0,
      colorRgba: 0x38bdf8ff,
    });
  }
  if (chIb) {
    scope1Tracks.push({
      channelId: chIb.channel_id,
      lane: "analog",
      visible: true,
      yScale: 1,
      yOffset: 0,
      colorRgba: 0x34d399ff,
    });
  }
  if (chVbus) {
    scope1Tracks.push({
      channelId: chVbus.channel_id,
      lane: "analog",
      visible: true,
      yScale: 0.5,
      yOffset: 0,
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
    yAxisScale: 15,
    yAxisOffset: 0,
    wheelZoomTarget: "x",
  };

  const scope2TriggerCh = chVbus ?? chIa;
  const scope2Tracks: ScopeTrackUiBinding[] = [];
  if (scope2TriggerCh) {
    scope2Tracks.push({
      channelId: scope2TriggerCh.channel_id,
      lane: "analog",
      visible: true,
      yScale: 1,
      yOffset: 0,
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
    yAxisScale: 25,
    yAxisOffset: 50,
    wheelZoomTarget: "x",
  };

  return { 1: scope1, 2: scope2 };
}

function buildDefaultDashboardCanvas(
  channelsById: Record<number, ChannelNodeDto_Serialize>,
  locale: UiLocale,
): { canvas: DashboardCanvasConfig; nextWidgetSeq: number } {
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
  const chFault =
    findFirst("sim0/fault_pin") ??
    allChannels.find(
      (c) => c.modality === "digital_bool" && c.channel_id !== chPwmA?.channel_id,
    );
  const chState =
    findFirst("sim0/ctrl_state") ??
    allChannels.find((c) => c.modality === "state_machine");

  const primarySourceId = chIa?.source_id ?? chVbus?.source_id ?? allChannels[0]?.source_id ?? "sim0";

  const watchChannels = [chIa, chIb, chVbus, chPwmA, chFault, chState].filter(
    (c): c is ChannelNodeDto_Serialize => Boolean(c),
  );

  const widgets: DashboardWidgetItem[] = [
    {
      id: "w_1",
      type: "digital_meter",
      title: chIa ? chIa.full_path : translate(locale, "toolbox.widget.digital_meter.name"),
      col: 0,
      row: 0,
      colSpan: 8,
      rowSpan: 4,
      primaryBinding: chIa ? { channelId: chIa.channel_id, scale: 1, offset: 0 } : null,
      bindings: [],
      scopeId: null,
    },
    {
      id: "w_2",
      type: "radial_gauge",
      title: chVbus ? chVbus.full_path : translate(locale, "toolbox.widget.radial_gauge.name"),
      col: 8,
      row: 0,
      colSpan: 8,
      rowSpan: 4,
      primaryBinding: chVbus ? { channelId: chVbus.channel_id, scale: 1, offset: 0 } : null,
      bindings: [],
      scopeId: null,
      gaugeConfig: { min: 0, max: 140, warnHigh: 95 },
    },
    {
      id: "w_3",
      type: "status_led",
      title: (chState ?? chFault)?.full_path ?? translate(locale, "toolbox.widget.status_led.name"),
      col: 16,
      row: 0,
      colSpan: 8,
      rowSpan: 4,
      primaryBinding:
        (chState ?? chFault)
          ? { channelId: (chState ?? chFault)!.channel_id, scale: 1, offset: 0 }
          : null,
      bindings: [],
      scopeId: null,
    },
    {
      id: "w_4",
      type: "watch_table",
      title: translate(locale, "toolbox.widget.watch_table.name"),
      col: 0,
      row: 4,
      colSpan: 24,
      rowSpan: 5,
      primaryBinding: null,
      bindings: watchChannels.map((c) => ({ channelId: c.channel_id, scale: 1, offset: 0 })),
      scopeId: null,
    },
    {
      id: "w_5",
      type: "param_slider",
      title: translate(locale, "toolbox.widget.param_slider.name"),
      col: 0,
      row: 9,
      colSpan: 12,
      rowSpan: 3,
      primaryBinding: null,
      bindings: [],
      scopeId: null,
      sliderConfig: {
        sourceId: primarySourceId,
        key: "kp",
        min: 0.2,
        max: 6.0,
        step: 0.05,
        value: 1.2,
      },
    },
    {
      id: "w_6",
      type: "cmd_button",
      title: translate(locale, "toolbox.widget.cmd_button.name"),
      col: 12,
      row: 9,
      colSpan: 12,
      rowSpan: 3,
      primaryBinding: null,
      bindings: [],
      scopeId: null,
      buttonConfig: {
        sourceId: primarySourceId,
        mode: "ascii",
        command: "SET_MODE=FOC_RUN",
        kvKey: "kp",
        kvValue: 1.5,
        priority: "high",
      },
    },
  ];

  return {
    canvas: {
      canvasId: 1,
      title: getDefaultCanvasTitle(locale, 1),
      locked: false,
      widgets,
    },
    nextWidgetSeq: 7,
  };
}

export function createWorkspaceStore(
  client: BridgeClient = bridgeClient,
  fetchStatus: () => Promise<RuntimeStatus> = fetchEngineStatus,
): UseBoundStore<StoreApi<WorkspaceStoreState>> {
  let nextTxLogId = 1;
  let telemetryUnsub: (() => void) | null = null;
  let metricsUnsub: (() => void) | null = null;
  let dockviewLayoutAccessor: (() => Record<string, unknown> | null) | null = null;

  return create<WorkspaceStoreState>((set, get) => {
    const fetchSourceConfigMap = async (
      groups: SourceChannelGroupDto_Serialize[],
      overrides?: Record<string, CreateSourceRequestDto_Serialize>,
    ): Promise<Record<string, CreateSourceRequestDto_Serialize>> => {
      const rawList = await client.getSourceConfigs().catch(() => []);
      const byIdFromBridge: Record<string, CreateSourceRequestDto_Serialize> = {};
      for (const cfg of rawList) {
        byIdFromBridge[cfg.source_id] = normalizeSourceConfigForExport(cfg);
      }

      const prevConfigs = get().sourceConfigsById;
      const merged: Record<string, CreateSourceRequestDto_Serialize> = {};
      for (const grp of groups) {
        const id = grp.source.source_id;
        merged[id] =
          byIdFromBridge[id] ??
          overrides?.[id] ??
          prevConfigs[id] ??
          synthesizeFallbackSourceConfig(grp);
      }
      return merged;
    };

    const collectExportableSources = (
      state: WorkspaceStoreState,
      sourceIds?: ReadonlyArray<string>,
    ): CreateSourceRequestDto_Serialize[] => {
      const targetIds = sourceIds
        ? new Set(sourceIds)
        : new Set(state.channelGroups.map((g) => g.source.source_id));
      const list: CreateSourceRequestDto_Serialize[] = [];
      for (const grp of state.channelGroups) {
        if (!targetIds.has(grp.source.source_id)) continue;
        list.push(
          state.sourceConfigsById[grp.source.source_id] ??
            synthesizeFallbackSourceConfig(grp),
        );
      }
      return list;
    };

    const collectBoardExportInput = (state: WorkspaceStoreState) => {
      const liveLayout = dockviewLayoutAccessor
        ? dockviewLayoutAccessor()
        : state.pendingDockviewLayout;
      const effectiveTargetSource =
        state.channelGroups.length === 0
          ? state.downlinkConfig.targetSource
          : state.channelGroups.some((g) => g.source.source_id === state.downlinkConfig.targetSource)
            ? state.downlinkConfig.targetSource
            : state.channelGroups[0].source.source_id;

      return {
        scopes: state.scopes,
        scopeOrder: state.scopeOrder,
        canvases: state.canvases,
        canvasOrder: state.canvasOrder,
        channelsById: state.channelsById,
        ui: {
          mockRateHz: state.mockRateHz,
          sidebarCollapsed: state.sidebarCollapsed,
          sidebarTab: state.sidebarTab,
          downlinkBarCollapsed: state.downlinkBarCollapsed,
          focusedScopeId: state.focusedScopeId,
          activeCanvasId: state.activeCanvasId,
        },
        downlink: {
          ...state.downlinkConfig,
          targetSource: effectiveTargetSource,
        },
        dockviewLayout: liveLayout,
        commands: state.commands,
      };
    };

    const applyRehydratedBoard = async (file: BoardConfigFileV1) => {
      const stateBefore = get();
      const rehydrated = rehydrateBoardFromConfig({
        file,
        channelsById: stateBefore.channelsById,
        availableSourceIds: stateBefore.channelGroups.map((g) => g.source.source_id),
      });

      const oldScopeIds = Object.keys(stateBefore.scopes).map((k) => Number(k));
      for (const oldId of oldScopeIds) {
        client.topicBus.deleteScopeCache(oldId);
      }
      await Promise.all(
        oldScopeIds.map((oldId) => client.removeScopeViewport(oldId).catch(() => {})),
      );

      if (rehydrated.ui.mockRateHz !== stateBefore.mockRateHz) {
        client.setMockRate(rehydrated.ui.mockRateHz);
      }

      const nextWidths: Record<number, number> = {};
      for (const idStr of Object.keys(rehydrated.scopes)) {
        nextWidths[Number(idStr)] = 760;
      }

      set((s) => ({
        scopes: rehydrated.scopes,
        scopeOrder: rehydrated.scopeOrder,
        focusedScopeId: rehydrated.ui.focusedScopeId,
        nextScopeId: rehydrated.nextScopeId,
        canvasWidthsByScope: nextWidths,
        canvases: rehydrated.canvases,
        canvasOrder: rehydrated.canvasOrder,
        activeCanvasId: rehydrated.ui.activeCanvasId,
        nextCanvasId: rehydrated.nextCanvasId,
        nextWidgetSeq: rehydrated.nextWidgetSeq,
        mockRateHz: rehydrated.ui.mockRateHz,
        sidebarCollapsed: rehydrated.ui.sidebarCollapsed,
        sidebarTab: rehydrated.ui.sidebarTab,
        downlinkBarCollapsed: rehydrated.ui.downlinkBarCollapsed,
        downlinkConfig: rehydrated.downlink,
        commands: rehydrated.commands,
        nextCommandSeq: rehydrated.nextCommandSeq,
        pendingDockviewLayout: rehydrated.dockviewLayout,
        layoutRestoreSeq: s.layoutRestoreSeq + 1,
      }));

      await Promise.all(
        Object.values(rehydrated.scopes).map((sc) =>
          client.upsertScopeViewport(toViewportDto(sc, 760)).catch(() => {}),
        ),
      );

      return {
        scopeCount: rehydrated.scopeOrder.length,
        canvasCount: rehydrated.canvasOrder.length,
        widgetCount: Object.values(rehydrated.canvases).reduce(
          (sum, cv) => sum + cv.widgets.length,
          0,
        ),
        missingChannelPaths: rehydrated.missingChannelPaths,
      };
    };

    const applyProjectFile = async (projectFile: ProjectConfigFileV1) => {
      const existingTree = await client.getChannelTree();
      for (const grp of existingTree.sources) {
        await client.removeSource(grp.source.source_id);
      }
      for (const srcCfg of projectFile.sources) {
        await client.createSource(srcCfg as unknown as CreateSourceRequestDto_Deserialize);
      }

      const tree = await client.getChannelTree();
      const chMap: Record<number, ChannelNodeDto_Serialize> = {};
      for (const grp of tree.sources) {
        for (const ch of grp.channels) {
          chMap[ch.channel_id] = ch;
        }
      }
      const sourceConfigs = await fetchSourceConfigMap(tree.sources);
      set({
        channelGroups: tree.sources,
        channelsById: chMap,
        sourceConfigsById: sourceConfigs,
        quota: {
          usedBytes: tree.quota_used_bytes ?? 0,
          limitBytes: tree.quota_limit_bytes ?? 2 * 1024 * 1024 * 1024,
          pinnedBytes: tree.pinned_bytes ?? 0,
        },
      });

      const boardResult = await applyRehydratedBoard(projectToBoardFile(projectFile));
      return {
        sourceCount: projectFile.sources.length,
        ...boardResult,
      };
    };

    return {
    initialized: false,
    initError: null,
    runtimeStatus: null,
    channelGroups: [],
    channelsById: {},
    sourceConfigsById: {},
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

    canvases: {},
    canvasOrder: [],
    activeCanvasId: 1,
    nextCanvasId: 2,
    nextWidgetSeq: 1,

    commands: [],
    nextCommandSeq: 1,

    locale: detectInitialLocale(),
    mockRateHz: 120,
    sidebarCollapsed: false,
    sidebarTab: "sources",
    downlinkBarCollapsed: false,
    sourceWizardOpen: false,
    downlinkConfig: { ...DEFAULT_DOWNLINK_UI_CONFIG },
    pendingDockviewLayout: null,
    layoutRestoreSeq: 0,

    setLocale: (nextLocale) => {
      persistLocale(nextLocale);
      const state = get();
      const nextScopes: Record<number, ScopeUiConfig> = {};
      let changedScopes = false;
      for (const [idStr, sc] of Object.entries(state.scopes)) {
        const nextTitle = translateDefaultScopeTitle(sc.title, nextLocale);
        if (nextTitle !== sc.title) {
          changedScopes = true;
          nextScopes[Number(idStr)] = { ...sc, title: nextTitle };
        } else {
          nextScopes[Number(idStr)] = sc;
        }
      }
      const nextCanvases: Record<number, DashboardCanvasConfig> = {};
      let changedCanvases = false;
      for (const [idStr, cv] of Object.entries(state.canvases)) {
        const nextTitle = translateDefaultCanvasTitle(cv.title, nextLocale);
        if (nextTitle !== cv.title) {
          changedCanvases = true;
          nextCanvases[Number(idStr)] = { ...cv, title: nextTitle };
        } else {
          nextCanvases[Number(idStr)] = cv;
        }
      }
      set({
        locale: nextLocale,
        ...(changedScopes ? { scopes: nextScopes } : {}),
        ...(changedCanvases ? { canvases: nextCanvases } : {}),
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

        let appliedDefaultProject = false;
        const defaultProjectJson = await client.loadDefaultProjectConfig().catch(() => null);
        if (defaultProjectJson !== null) {
          try {
            const projectFile = parseProjectConfigJson(defaultProjectJson);
            await applyProjectFile(projectFile);
            appliedDefaultProject = true;
          } catch (err) {
            console.warn(
              "Failed to apply portable default project config, falling back to built-in defaults:",
              err,
            );
          }
        }

        if (appliedDefaultProject) {
          set({
            initialized: true,
            initError: null,
            runtimeStatus: status,
          });
        } else {
          const defaultScopes = buildDefaultScopeConfigs(chMap, get().locale);
          await Promise.all(
            Object.values(defaultScopes).map((sc) =>
              client.upsertScopeViewport(toViewportDto(sc, 760)),
            ),
          );

          const sourceConfigs = await fetchSourceConfigMap(tree.sources);
          const defaultCanvas = buildDefaultDashboardCanvas(chMap, get().locale);

          set({
            initialized: true,
            initError: null,
            runtimeStatus: status,
            channelGroups: tree.sources,
            channelsById: chMap,
            sourceConfigsById: sourceConfigs,
            quota: {
              usedBytes: tree.quota_used_bytes ?? 0,
              limitBytes: tree.quota_limit_bytes ?? 2 * 1024 * 1024 * 1024,
              pinnedBytes: tree.pinned_bytes ?? 0,
            },
            scopes: defaultScopes,
            scopeOrder: [1, 2],
            focusedScopeId: 1,
            nextScopeId: 3,
            canvases: { 1: defaultCanvas.canvas },
            canvasOrder: [1],
            activeCanvasId: 1,
            nextCanvasId: 2,
            nextWidgetSeq: defaultCanvas.nextWidgetSeq,
          });
        }
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
      const sourceConfigs = await fetchSourceConfigMap(tree.sources);
      set({
        channelGroups: tree.sources,
        channelsById: chMap,
        sourceConfigsById: sourceConfigs,
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
    setSidebarTab: (tab) => set({ sidebarTab: tab }),
    setDownlinkBarCollapsed: (collapsed) => set({ downlinkBarCollapsed: collapsed }),
    setSourceWizardOpen: (open) => set({ sourceWizardOpen: open }),
    setFocusedScopeId: (scopeId) => set({ focusedScopeId: scopeId }),
    updateDownlinkConfig: (patch) =>
      set((s) => ({
        downlinkConfig: {
          ...s.downlinkConfig,
          ...patch,
        },
      })),
    registerDockviewLayoutAccessor: (accessor) => {
      dockviewLayoutAccessor = accessor;
    },

    createSource: async (req) => {
      await client.createSource(req);
      const norm = normalizeSourceConfigForExport(req);
      set((s) => ({
        sourceConfigsById: {
          ...s.sourceConfigsById,
          [norm.source_id]: norm,
        },
      }));
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
      const tree = await client.getChannelTree();
      const chMap: Record<number, ChannelNodeDto_Serialize> = {};
      for (const grp of tree.sources) {
        for (const ch of grp.channels) {
          chMap[ch.channel_id] = ch;
        }
      }

      const state = get();
      const nextTelemetry = { ...state.telemetryBySource };
      delete nextTelemetry[sourceId];

      const fallbackSourceId = tree.sources[0]?.source.source_id ?? "";
      const changedScopesToSync: Array<{ scope: ScopeUiConfig; canvasW: number }> = [];
      const nextScopes: Record<number, ScopeUiConfig> = {};
      for (const [idStr, sc] of Object.entries(state.scopes)) {
        const scopeId = Number(idStr);
        const remainingTracks = sc.tracks.filter((t) => Boolean(chMap[t.channelId]));
        const nextTrigger =
          sc.trigger && chMap[sc.trigger.channel_id] ? sc.trigger : null;
        const tracksChanged = remainingTracks.length !== sc.tracks.length;
        const triggerChanged = nextTrigger !== sc.trigger;

        if (tracksChanged || triggerChanged) {
          const activeStillPresent =
            sc.activeAnalogChannelId !== null &&
            remainingTracks.some((t) => t.channelId === sc.activeAnalogChannelId);
          const nextActiveAnalog = activeStillPresent
            ? sc.activeAnalogChannelId
            : (remainingTracks.find((t) => t.lane === "analog")?.channelId ?? null);
          const updatedScope: ScopeUiConfig = {
            ...sc,
            tracks: remainingTracks,
            trigger: nextTrigger,
            activeAnalogChannelId: nextActiveAnalog,
          };
          nextScopes[scopeId] = updatedScope;
          if (remainingTracks.length === 0) {
            client.topicBus.deleteScopeCache(scopeId);
          }
          changedScopesToSync.push({
            scope: updatedScope,
            canvasW: state.canvasWidthsByScope[scopeId] ?? 760,
          });
        } else {
          nextScopes[scopeId] = sc;
        }
      }

      const nextCanvases: Record<number, DashboardCanvasConfig> = {};
      for (const [idStr, cv] of Object.entries(state.canvases)) {
        const canvasId = Number(idStr);
        let canvasChanged = false;
        const nextWidgets = cv.widgets.map((w) => {
          let updated = w;
          if (w.primaryBinding && !chMap[w.primaryBinding.channelId]) {
            updated = { ...updated, primaryBinding: null };
            canvasChanged = true;
          }
          if (w.bindings.some((b) => !chMap[b.channelId])) {
            updated = {
              ...updated,
              bindings: w.bindings.filter((b) => Boolean(chMap[b.channelId])),
            };
            canvasChanged = true;
          }
          if (w.sliderConfig && w.sliderConfig.sourceId === sourceId) {
            updated = {
              ...updated,
              sliderConfig: { ...w.sliderConfig, sourceId: fallbackSourceId },
            };
            canvasChanged = true;
          }
          if (w.buttonConfig && w.buttonConfig.sourceId === sourceId) {
            updated = {
              ...updated,
              buttonConfig: { ...w.buttonConfig, sourceId: fallbackSourceId },
            };
            canvasChanged = true;
          }
          return updated;
        });
        nextCanvases[canvasId] = canvasChanged ? { ...cv, widgets: nextWidgets } : cv;
      }

      const sourceConfigs = await fetchSourceConfigMap(tree.sources);
      set({
        channelGroups: tree.sources,
        channelsById: chMap,
        sourceConfigsById: sourceConfigs,
        telemetryBySource: nextTelemetry,
        scopes: nextScopes,
        canvases: nextCanvases,
        ...(state.downlinkConfig.targetSource === sourceId
          ? {
              downlinkConfig: {
                ...state.downlinkConfig,
                targetSource: fallbackSourceId,
              },
            }
          : {}),
        quota: {
          usedBytes: tree.quota_used_bytes ?? 0,
          limitBytes: tree.quota_limit_bytes ?? 2 * 1024 * 1024 * 1024,
          pinnedBytes: tree.pinned_bytes ?? 0,
        },
      });

      await Promise.all(
        changedScopesToSync.map(({ scope, canvasW }) =>
          client.upsertScopeViewport(toViewportDto(scope, canvasW)).catch(() => {}),
        ),
      );
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

    exportSourcesConfig: (sourceIds) => {
      const state = get();
      const list = collectExportableSources(state, sourceIds);
      if (sourceIds) {
        for (const id of sourceIds) {
          if (list.some((item) => item.source_id === id)) continue;
          const cached = state.sourceConfigsById[id];
          if (cached) list.push(cached);
        }
      }
      return serializeSourcesConfig(list);
    },

    importSourcesConfig: async (jsonText, options) => {
      const file = parseSourcesConfigJson(jsonText);
      const replaceExisting = options?.replaceExisting ?? true;
      const snapshotState = get();
      const channelsBefore = snapshotState.channelsById;
      const existingIds = new Set(snapshotState.channelGroups.map((g) => g.source.source_id));
      const replacedSourceIds = new Set<string>();
      const importedOverrides: Record<string, CreateSourceRequestDto_Serialize> = {};
      const importedIds: string[] = [];
      const skippedIds: string[] = [];

      for (const srcCfg of file.sources) {
        if (existingIds.has(srcCfg.source_id)) {
          if (!replaceExisting) {
            skippedIds.push(srcCfg.source_id);
            continue;
          }
          replacedSourceIds.add(srcCfg.source_id);
          await client.removeSource(srcCfg.source_id);
        }
        await client.createSource(srcCfg);
        existingIds.add(srcCfg.source_id);
        importedIds.push(srcCfg.source_id);
        importedOverrides[srcCfg.source_id] = srcCfg;
      }

      const tree = await client.getChannelTree();
      const chMap: Record<number, ChannelNodeDto_Serialize> = {};
      const channelByFullPath = new Map<string, ChannelNodeDto_Serialize>();
      for (const grp of tree.sources) {
        for (const ch of grp.channels) {
          chMap[ch.channel_id] = ch;
          channelByFullPath.set(ch.full_path, ch);
        }
      }
      const nextSourceConfigs = await fetchSourceConfigMap(tree.sources, importedOverrides);

      const rebindChannelId = (oldChannelId: number): number | null => {
        const oldCh = channelsBefore[oldChannelId];
        if (oldCh) {
          if (!replacedSourceIds.has(oldCh.source_id) && chMap[oldChannelId]) {
            return oldChannelId;
          }
          const matched = channelByFullPath.get(oldCh.full_path);
          return matched ? matched.channel_id : null;
        }
        return chMap[oldChannelId] ? oldChannelId : null;
      };

      const latest = get();
      const changedScopesToSync: Array<{ scope: ScopeUiConfig; canvasW: number }> = [];
      const nextScopes: Record<number, ScopeUiConfig> = {};

      for (const [idStr, sc] of Object.entries(latest.scopes)) {
        const scopeId = Number(idStr);
        const seenTrackIds = new Set<number>();
        const nextTracks: ScopeTrackUiBinding[] = [];
        let tracksChanged = false;

        for (const tr of sc.tracks) {
          const reboundId = rebindChannelId(tr.channelId);
          if (reboundId === null) {
            tracksChanged = true;
            continue;
          }
          if (reboundId !== tr.channelId) {
            tracksChanged = true;
          }
          if (seenTrackIds.has(reboundId)) {
            tracksChanged = true;
            continue;
          }
          seenTrackIds.add(reboundId);
          nextTracks.push({ ...tr, channelId: reboundId });
        }

        let nextTrigger = sc.trigger;
        let triggerChanged = false;
        if (sc.trigger) {
          const reboundTrigId = rebindChannelId(sc.trigger.channel_id);
          if (reboundTrigId === null) {
            nextTrigger = null;
            triggerChanged = true;
          } else if (reboundTrigId !== sc.trigger.channel_id) {
            nextTrigger = { ...sc.trigger, channel_id: reboundTrigId };
            triggerChanged = true;
          }
        }

        const reboundActiveId =
          sc.activeAnalogChannelId !== null ? rebindChannelId(sc.activeAnalogChannelId) : null;
        const activeStillValid =
          reboundActiveId !== null &&
          nextTracks.some((tr) => tr.channelId === reboundActiveId && tr.lane === "analog");
        const nextActiveAnalog = activeStillValid
          ? reboundActiveId
          : (nextTracks.find((tr) => tr.lane === "analog")?.channelId ?? null);
        const activeChanged = nextActiveAnalog !== sc.activeAnalogChannelId;

        if (tracksChanged || triggerChanged || activeChanged) {
          const updatedScope: ScopeUiConfig = {
            ...sc,
            tracks: nextTracks,
            trigger: nextTrigger,
            activeAnalogChannelId: nextActiveAnalog,
          };
          nextScopes[scopeId] = updatedScope;
          if (nextTracks.length === 0) {
            client.topicBus.deleteScopeCache(scopeId);
          }
          changedScopesToSync.push({
            scope: updatedScope,
            canvasW: latest.canvasWidthsByScope[scopeId] ?? 760,
          });
        } else {
          nextScopes[scopeId] = sc;
        }
      }

      const nextCanvases: Record<number, DashboardCanvasConfig> = {};
      for (const [idStr, cv] of Object.entries(latest.canvases)) {
        const canvasId = Number(idStr);
        let canvasChanged = false;
        const nextWidgets = cv.widgets.map((w) => {
          let updated = w;
          if (w.primaryBinding) {
            const reboundPrimary = rebindChannelId(w.primaryBinding.channelId);
            if (reboundPrimary === null) {
              updated = { ...updated, primaryBinding: null };
              canvasChanged = true;
            } else if (reboundPrimary !== w.primaryBinding.channelId) {
              updated = {
                ...updated,
                primaryBinding: { ...w.primaryBinding, channelId: reboundPrimary },
              };
              canvasChanged = true;
            }
          }
          if (w.bindings.length > 0) {
            const seenBindingIds = new Set<number>();
            const nextBindings: typeof w.bindings = [];
            let bindingsChanged = false;
            for (const b of w.bindings) {
              const reboundId = rebindChannelId(b.channelId);
              if (reboundId === null) {
                bindingsChanged = true;
                continue;
              }
              if (reboundId !== b.channelId) {
                bindingsChanged = true;
              }
              if (seenBindingIds.has(reboundId)) {
                bindingsChanged = true;
                continue;
              }
              seenBindingIds.add(reboundId);
              nextBindings.push({ ...b, channelId: reboundId });
            }
            if (bindingsChanged) {
              updated = { ...updated, bindings: nextBindings };
              canvasChanged = true;
            }
          }
          return updated;
        });
        nextCanvases[canvasId] = canvasChanged ? { ...cv, widgets: nextWidgets } : cv;
      }

      const activeSourceIds = new Set(tree.sources.map((g) => g.source.source_id));
      const nextTelemetry: Record<string, SourceTelemetryDto> = {};
      for (const [srcId, tel] of Object.entries(latest.telemetryBySource)) {
        if (activeSourceIds.has(srcId)) {
          nextTelemetry[srcId] = tel;
        }
      }

      set({
        channelGroups: tree.sources,
        channelsById: chMap,
        sourceConfigsById: nextSourceConfigs,
        telemetryBySource: nextTelemetry,
        scopes: nextScopes,
        canvases: nextCanvases,
        quota: {
          usedBytes: tree.quota_used_bytes ?? 0,
          limitBytes: tree.quota_limit_bytes ?? 2 * 1024 * 1024 * 1024,
          pinnedBytes: tree.pinned_bytes ?? 0,
        },
      });

      await Promise.all(
        changedScopesToSync.map(({ scope, canvasW }) =>
          client.upsertScopeViewport(toViewportDto(scope, canvasW)).catch(() => {}),
        ),
      );

      return { importedIds, skippedIds };
    },

    addScope: (title, options) => {
      const state = get();
      const scopeId = state.nextScopeId;
      const dockInStage = options?.dockInStage !== false;
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
        yAxisScale: 15,
        yAxisOffset: 0,
        wheelZoomTarget: "x",
      };

      set({
        scopes: { ...state.scopes, [scopeId]: newScope },
        ...(dockInStage
          ? {
              scopeOrder: [...state.scopeOrder, scopeId],
              focusedScopeId: scopeId,
            }
          : {}),
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

    addCanvas: (title) => {
      const state = get();
      const canvasId = state.nextCanvasId;
      const newCanvas: DashboardCanvasConfig = {
        canvasId,
        title: title ?? getDefaultCanvasTitle(state.locale, canvasId),
        locked: false,
        widgets: [],
      };
      set({
        canvases: { ...state.canvases, [canvasId]: newCanvas },
        canvasOrder: [...state.canvasOrder, canvasId],
        activeCanvasId: canvasId,
        nextCanvasId: canvasId + 1,
      });
      return canvasId;
    },

    removeCanvas: (canvasId) => {
      const state = get();
      const target = state.canvases[canvasId];
      if (!target) return;
      for (const w of target.widgets) {
        if (w.type === "mso_scope" && w.scopeId !== null && !state.scopeOrder.includes(w.scopeId)) {
          get().removeScope(w.scopeId);
        }
      }
      const nextCanvases = { ...get().canvases };
      delete nextCanvases[canvasId];
      const nextOrder = state.canvasOrder.filter((id) => id !== canvasId);
      const nextActive =
        state.activeCanvasId === canvasId ? (nextOrder[0] ?? null) : state.activeCanvasId;
      set({
        canvases: nextCanvases,
        canvasOrder: nextOrder,
        activeCanvasId: nextActive,
      });
    },

    setCanvasOrder: (order) => set({ canvasOrder: order }),

    setActiveCanvasId: (canvasId) => set({ activeCanvasId: canvasId }),

    toggleCanvasLock: (canvasId) => {
      const state = get();
      const prev = state.canvases[canvasId];
      if (!prev) return;
      set({
        canvases: {
          ...state.canvases,
          [canvasId]: { ...prev, locked: !prev.locked },
        },
      });
    },

    clearCanvasWidgets: (canvasId) => {
      const state = get();
      const prev = state.canvases[canvasId];
      if (!prev) return;
      for (const w of prev.widgets) {
        if (w.type === "mso_scope" && w.scopeId !== null && !state.scopeOrder.includes(w.scopeId)) {
          get().removeScope(w.scopeId);
        }
      }
      const latest = get();
      const latestPrev = latest.canvases[canvasId];
      if (!latestPrev) return;
      set({
        canvases: {
          ...latest.canvases,
          [canvasId]: { ...latestPrev, widgets: [] },
        },
      });
    },

    addWidgetToCanvas: (canvasId, widgetType, opts) => {
      const state = get();
      const canvas = state.canvases[canvasId];
      if (!canvas) return "";

      const seq = state.nextWidgetSeq;
      const widgetId = `w_${seq}`;
      const size = defaultSizeForWidgetType(widgetType);
      const slot = findFreeGridSlot(
        canvas.widgets,
        size.colSpan,
        size.rowSpan,
        opts?.col !== undefined && opts?.row !== undefined
          ? { col: opts.col, row: opts.row }
          : undefined,
      );
      const clamped = clampGridRect(slot.col, slot.row, size.colSpan, size.rowSpan);

      const ch = opts?.channelId !== undefined ? state.channelsById[opts.channelId] : undefined;
      const primarySourceId =
        ch?.source_id ?? state.channelGroups[0]?.source.source_id ?? "sim0";

      let scopeId: number | null = null;
      if (widgetType === "mso_scope") {
        scopeId = get().addScope(undefined, { dockInStage: false });
      }

      const defaultTitle =
        ch && widgetType !== "watch_table"
          ? ch.full_path
          : translate(state.locale, `toolbox.widget.${widgetType}.name` as MessageKey);

      const binding =
        ch && widgetType !== "watch_table" && widgetType !== "param_slider" && widgetType !== "cmd_button"
          ? { channelId: ch.channel_id, scale: 1, offset: 0 }
          : null;

      const item: DashboardWidgetItem = {
        id: widgetId,
        type: widgetType,
        title: defaultTitle,
        col: clamped.col,
        row: clamped.row,
        colSpan: clamped.colSpan,
        rowSpan: clamped.rowSpan,
        primaryBinding: widgetType === "mso_scope" ? null : binding,
        bindings:
          ch && widgetType === "watch_table"
            ? [{ channelId: ch.channel_id, scale: 1, offset: 0 }]
            : [],
        scopeId,
        ...(widgetType === "radial_gauge"
          ? {
              gaugeConfig:
                ch?.local_path === "v_bus"
                  ? { min: 0, max: 140, warnHigh: 95 }
                  : { min: -25, max: 25, warnHigh: 18 },
            }
          : {}),
        ...(widgetType === "param_slider"
          ? {
              sliderConfig: {
                sourceId: primarySourceId,
                key: "kp",
                min: 0.2,
                max: 6.0,
                step: 0.05,
                value: 1.2,
              },
            }
          : {}),
        ...(widgetType === "cmd_button"
          ? {
              buttonConfig: {
                sourceId: primarySourceId,
                mode: "ascii",
                command: "SET_MODE=FOC_RUN",
                kvKey: "kp",
                kvValue: 1.5,
                priority: "high",
              },
            }
          : {}),
      };

      const latest = get();
      const latestCanvas = latest.canvases[canvasId];
      if (!latestCanvas) return widgetId;

      set({
        canvases: {
          ...latest.canvases,
          [canvasId]: {
            ...latestCanvas,
            widgets: [...latestCanvas.widgets, item],
          },
        },
        nextWidgetSeq: seq + 1,
      });

      if (scopeId !== null && opts?.channelId !== undefined) {
        get().bindChannelToScope(scopeId, opts.channelId);
      }

      return widgetId;
    },

    updateCanvasWidget: (canvasId, widgetId, updater) => {
      const state = get();
      const canvas = state.canvases[canvasId];
      if (!canvas) return;
      let changed = false;
      const nextWidgets = canvas.widgets.map((w) => {
        if (w.id !== widgetId) return w;
        const next = updater(w);
        if (next !== w) changed = true;
        return next;
      });
      if (!changed) return;
      set({
        canvases: {
          ...state.canvases,
          [canvasId]: {
            ...canvas,
            widgets: nextWidgets,
          },
        },
      });
    },

    removeCanvasWidget: (canvasId, widgetId) => {
      const state = get();
      const canvas = state.canvases[canvasId];
      if (!canvas) return;
      const target = canvas.widgets.find((w) => w.id === widgetId);
      if (!target) return;
      if (
        target.type === "mso_scope" &&
        target.scopeId !== null &&
        !state.scopeOrder.includes(target.scopeId)
      ) {
        get().removeScope(target.scopeId);
      }
      const latest = get();
      const latestCanvas = latest.canvases[canvasId];
      if (!latestCanvas) return;
      set({
        canvases: {
          ...latest.canvases,
          [canvasId]: {
            ...latestCanvas,
            widgets: latestCanvas.widgets.filter((w) => w.id !== widgetId),
          },
        },
      });
    },

    bindChannelToWidget: (canvasId, widgetId, channelId) => {
      const state = get();
      const canvas = state.canvases[canvasId];
      const ch = state.channelsById[channelId];
      if (!canvas || !ch) return;

      const targetWidget = canvas.widgets.find((w) => w.id === widgetId);
      if (targetWidget?.type === "mso_scope" && targetWidget.scopeId !== null) {
        get().bindChannelToScope(targetWidget.scopeId, channelId);
        return;
      }

      get().updateCanvasWidget(canvasId, widgetId, (prev) => {
        if (prev.type === "watch_table") {
          if (prev.bindings.some((b) => b.channelId === channelId)) {
            return prev;
          }
          return {
            ...prev,
            bindings: [...prev.bindings, { channelId, scale: 1, offset: 0 }],
          };
        }
        if (
          prev.type === "digital_meter" ||
          prev.type === "radial_gauge" ||
          prev.type === "status_led"
        ) {
          return {
            ...prev,
            title: ch.full_path,
            primaryBinding: { channelId, scale: 1, offset: 0 },
            ...(prev.type === "radial_gauge"
              ? {
                  gaugeConfig:
                    ch.local_path === "v_bus"
                      ? { min: 0, max: 140, warnHigh: 95 }
                      : prev.gaugeConfig ?? { min: -25, max: 25, warnHigh: 18 },
                }
              : {}),
          };
        }
        if (prev.type === "param_slider") {
          const prevCfg = prev.sliderConfig ?? {
            sourceId: ch.source_id,
            key: ch.local_path || "kp",
            min: 0.2,
            max: 6.0,
            step: 0.05,
            value: 1.2,
          };
          return {
            ...prev,
            primaryBinding: { channelId, scale: 1, offset: 0 },
            sliderConfig: {
              ...prevCfg,
              sourceId: ch.source_id,
              key: ch.local_path || prevCfg.key,
            },
          };
        }
        if (prev.type === "cmd_button") {
          const prevCfg = prev.buttonConfig ?? {
            sourceId: ch.source_id,
            mode: "ascii" as const,
            command: "SET_MODE=FOC_RUN",
            kvKey: ch.local_path || "kp",
            kvValue: 1.5,
            priority: "high" as const,
          };
          return {
            ...prev,
            primaryBinding: { channelId, scale: 1, offset: 0 },
            buttonConfig: {
              ...prevCfg,
              sourceId: ch.source_id,
              kvKey: ch.local_path || prevCfg.kvKey,
            },
          };
        }
        return prev;
      });
    },

    exportBoardConfig: () => {
      return serializeBoardConfig(collectBoardExportInput(get()));
    },

    importBoardConfig: async (jsonText) => {
      const file = parseBoardConfigJson(jsonText);
      return applyRehydratedBoard(file);
    },

    exportProjectConfig: () => {
      const state = get();
      return serializeProjectConfig({
        ...collectBoardExportInput(state),
        sources: collectExportableSources(state),
      });
    },

    importProjectConfig: async (jsonText) => {
      const projectFile = parseProjectConfigJson(jsonText);
      return applyProjectFile(projectFile);
    },

    saveDefaultProjectConfig: async () => {
      const state = get();
      const jsonText = serializeProjectConfig({
        ...collectBoardExportInput(state),
        sources: collectExportableSources(state),
      });
      return client.saveDefaultProjectConfig(jsonText);
    },

    clearDefaultProjectConfig: async () => {
      await client.clearDefaultProjectConfig();
    },

    addCommand: (partial) => {
      const state = get();
      const usedIds = new Set(state.commands.map((c) => c.id));
      let seq = state.nextCommandSeq;
      while (usedIds.has(`cmd_${seq}`)) {
        seq += 1;
      }
      const id = `cmd_${seq}`;
      const format = partial?.format === "hex" ? "hex" : "string";
      const defaultName =
        state.locale === "zh-CN" ? `命令 ${seq}` : `Command ${seq}`;
      const item: CommandDefinition = {
        id,
        name: partial?.name ?? defaultName,
        format,
        template: partial?.template ?? (format === "hex" ? "00 01 02" : "set_num %f"),
        appendNewline: partial?.appendNewline ?? false,
        endian: partial?.endian === "big" ? "big" : "little",
        targetSourceId: partial?.targetSourceId?.trim() ?? "",
        priority: partial?.priority === "high" ? "high" : "normal",
      };
      set({
        commands: [...state.commands, item],
        nextCommandSeq: seq + 1,
      });
      return id;
    },

    updateCommand: (commandId, patch) => {
      set((state) => ({
        commands: state.commands.map((cmd) =>
          cmd.id === commandId ? { ...cmd, ...patch, id: cmd.id } : cmd,
        ),
      }));
    },

    removeCommand: (commandId) => {
      const state = get();
      const nextCanvases: Record<number, DashboardCanvasConfig> = {};
      let canvasChanged = false;
      for (const [idStr, canvas] of Object.entries(state.canvases)) {
        const cid = Number(idStr);
        let widgetChanged = false;
        const nextWidgets = canvas.widgets.map((w) => {
          let nextW = w;
          if (w.sliderConfig?.commandId === commandId) {
            widgetChanged = true;
            nextW = {
              ...nextW,
              sliderConfig: { ...w.sliderConfig, commandId: null },
            };
          }
          if (w.buttonConfig?.commandId === commandId) {
            widgetChanged = true;
            nextW = {
              ...nextW,
              buttonConfig: { ...w.buttonConfig, commandId: null },
            };
          }
          return nextW;
        });
        if (widgetChanged) {
          canvasChanged = true;
          nextCanvases[cid] = { ...canvas, widgets: nextWidgets };
        } else {
          nextCanvases[cid] = canvas;
        }
      }

      set({
        commands: state.commands.filter((c) => c.id !== commandId),
        ...(canvasChanged ? { canvases: nextCanvases } : {}),
      });
    },

    bindCommandToWidget: (canvasId, widgetId, commandId) => {
      const state = get();
      const canvas = state.canvases[canvasId];
      const cmd = state.commands.find((c) => c.id === commandId);
      if (!canvas || !cmd) return;

      const fallbackSource = state.channelGroups[0]?.source.source_id ?? "sim0";

      get().updateCanvasWidget(canvasId, widgetId, (prev) => {
        if (prev.type === "param_slider") {
          const prevCfg = prev.sliderConfig ?? {
            sourceId: fallbackSource,
            key: "kp",
            min: 0.2,
            max: 6.0,
            step: 0.05,
            value: 1.2,
          };
          return {
            ...prev,
            title: cmd.name || prev.title,
            sliderConfig: {
              ...prevCfg,
              sourceId: cmd.targetSourceId || prevCfg.sourceId || fallbackSource,
              commandId: cmd.id,
            },
          };
        }

        if (prev.type === "cmd_button") {
          const prevCfg = prev.buttonConfig ?? {
            sourceId: fallbackSource,
            mode: "ascii" as const,
            command: "SET_MODE=FOC_RUN",
            kvKey: "kp",
            kvValue: 1.5,
            priority: cmd.priority,
          };
          return {
            ...prev,
            title: cmd.name || prev.title,
            buttonConfig: {
              ...prevCfg,
              sourceId: cmd.targetSourceId || prevCfg.sourceId || fallbackSource,
              priority: cmd.priority,
              commandId: cmd.id,
              commandValue: prevCfg.commandValue ?? 0,
            },
          };
        }

        return prev;
      });
    },

    placeCommandOnCanvas: (canvasId, commandId, opts) => {
      const state = get();
      const cmd = state.commands.find((c) => c.id === commandId);
      if (!cmd) return "";

      const widgetType: WidgetType = commandUsesValue(cmd)
        ? "param_slider"
        : "cmd_button";
      const widgetId = get().addWidgetToCanvas(canvasId, widgetType, opts);
      if (widgetId) {
        get().bindCommandToWidget(canvasId, widgetId, commandId);
      }
      return widgetId;
    },

    executeCommand: async (commandId, opts) => {
      const state = get();
      const cmd = state.commands.find((c) => c.id === commandId);
      if (!cmd) {
        throw new Error(`Command '${commandId}' not found.`);
      }
      const compiled = compileCommandPayload(cmd, opts?.value ?? 0);
      if (!compiled.ok) {
        throw new Error(compiled.error);
      }
      const targetSource =
        cmd.targetSourceId.trim() ||
        opts?.fallbackSourceId?.trim() ||
        state.channelGroups[0]?.source.source_id ||
        "sim0";
      return get().sendDownlink(
        {
          source_id: targetSource,
          priority: cmd.priority,
          coalesce_key: opts?.coalesceKey ?? null,
          payload: compiled.payload,
        },
        `${cmd.name}: ${compiled.preview}`,
        opts?.isSliderEvent ?? false,
      );
    },

    exportCommandsConfig: () => {
      return serializeCommandsConfig(get().commands);
    },

    importCommandsConfig: (jsonText, options) => {
      const file = parseCommandsConfigJson(jsonText);
      const state = get();

      if (options?.replaceExisting) {
        const nextIds = new Set(file.commands.map((c) => c.id));
        const nextCanvases: Record<number, DashboardCanvasConfig> = {};
        let canvasChanged = false;
        for (const [idStr, canvas] of Object.entries(state.canvases)) {
          const cid = Number(idStr);
          let widgetChanged = false;
          const nextWidgets = canvas.widgets.map((w) => {
            let nextW = w;
            if (w.sliderConfig?.commandId && !nextIds.has(w.sliderConfig.commandId)) {
              widgetChanged = true;
              nextW = { ...nextW, sliderConfig: { ...w.sliderConfig, commandId: null } };
            }
            if (w.buttonConfig?.commandId && !nextIds.has(w.buttonConfig.commandId)) {
              widgetChanged = true;
              nextW = { ...nextW, buttonConfig: { ...w.buttonConfig, commandId: null } };
            }
            return nextW;
          });
          if (widgetChanged) {
            canvasChanged = true;
            nextCanvases[cid] = { ...canvas, widgets: nextWidgets };
          } else {
            nextCanvases[cid] = canvas;
          }
        }

        let maxSeq = 0;
        for (const c of file.commands) {
          const m = /^cmd_(\d+)$/.exec(c.id);
          if (m) maxSeq = Math.max(maxSeq, Number(m[1]));
        }

        set({
          commands: file.commands,
          nextCommandSeq: Math.max(maxSeq + 1, file.commands.length + 1),
          ...(canvasChanged ? { canvases: nextCanvases } : {}),
        });
        return { importedCount: file.commands.length };
      }

      const usedIds = new Set(state.commands.map((c) => c.id));
      let nextSeq = state.nextCommandSeq;
      for (const c of state.commands) {
        const m = /^cmd_(\d+)$/.exec(c.id);
        if (m) nextSeq = Math.max(nextSeq, Number(m[1]) + 1);
      }
      const appended: CommandDefinition[] = [];
      for (const rawCmd of file.commands) {
        let finalId = rawCmd.id;
        if (usedIds.has(finalId)) {
          while (usedIds.has(`cmd_${nextSeq}`)) {
            nextSeq += 1;
          }
          finalId = `cmd_${nextSeq}`;
          nextSeq += 1;
        }
        usedIds.add(finalId);
        const m = /^cmd_(\d+)$/.exec(finalId);
        if (m) nextSeq = Math.max(nextSeq, Number(m[1]) + 1);
        appended.push({ ...rawCmd, id: finalId });
      }

      set({
        commands: [...state.commands, ...appended],
        nextCommandSeq: nextSeq,
      });
      return { importedCount: appended.length };
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
  };
  });
}

export const useWorkspaceStore = createWorkspaceStore();
