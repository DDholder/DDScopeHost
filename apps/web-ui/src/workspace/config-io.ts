import type {
  ChannelNodeDto_Serialize,
  CreateSourceRequestDto_Deserialize,
  CreateSourceRequestDto_Serialize,
  ScopeLaneKindDto,
  SourceChannelGroupDto_Serialize,
  TriggerConditionDto,
  TriggerSweepModeDto,
} from "../bridge/bindings.ts";
import { normalizeSourceConfigToSerialize } from "../bridge/source-config-normalize.ts";
import type { AnalogLayoutMode } from "../scope-engine/layout.ts";
import type {
  ScopeTrackUiBinding,
  ScopeUiConfig,
  WheelZoomTarget,
} from "../scope-engine/types.ts";
import {
  WIDGET_TYPES,
  clampGridRect,
  type DashboardCanvasConfig,
  type DashboardWidgetItem,
  type WidgetButtonConfig,
  type WidgetChannelBinding,
  type WidgetGaugeConfig,
  type WidgetSliderConfig,
  type WidgetType,
} from "../widgets/types.ts";
import {
  normalizeCommandDefinition,
  type CommandDefinition,
} from "./command-system.ts";

export const SOURCES_CONFIG_SCHEMA_V1 = "ddscope.sources.v1" as const;
export const CANVAS_CONFIG_SCHEMA_V1 = "ddscope.canvas.v1" as const;
export const BOARD_CONFIG_SCHEMA_V1 = "ddscope.board.v1" as const;
export const PROJECT_CONFIG_SCHEMA_V1 = "ddscope.project.v1" as const;
export const COMMANDS_CONFIG_SCHEMA_V1 = "ddscope.commands.v1" as const;

export interface DownlinkUiConfig {
  targetSource: string;
  priority: "normal" | "high";
  manualMode: "ascii_text" | "hex_bytes" | "key_value";
  asciiText: string;
  appendNewline: boolean;
  hexText: string;
  kvKey: string;
  kvVal: string;
}

export const DEFAULT_DOWNLINK_UI_CONFIG: DownlinkUiConfig = {
  targetSource: "sim0",
  priority: "normal",
  manualMode: "ascii_text",
  asciiText: "SET_MODE=FOC_RUN",
  appendNewline: true,
  hexText: "AA 55 01 10",
  kvKey: "kp",
  kvVal: "1.75",
};

export interface BoardUiLayoutStateV1 {
  mockRateHz: 60 | 120;
  sidebarCollapsed: boolean;
  sidebarTab: "sources" | "toolbox" | "commands";
  downlinkBarCollapsed: boolean;
  focusedScopeId: number | null;
  activeCanvasId: number | null;
}

export interface CommandsConfigFileV1 {
  schema: typeof COMMANDS_CONFIG_SCHEMA_V1;
  exportedAt: string;
  commands: CommandDefinition[];
}

export interface SourcesConfigFileV1 {
  schema: typeof SOURCES_CONFIG_SCHEMA_V1;
  exportedAt: string;
  sources: CreateSourceRequestDto_Serialize[];
}

export interface PortableWidgetChannelBindingV1 {
  channelPath: string;
  fallbackChannelId?: number;
  scale: number;
  offset: number;
}

export interface PortableScopeTrackV1 {
  channelPath: string;
  fallbackChannelId?: number;
  lane: ScopeLaneKindDto;
  visible: boolean;
  yScale: number;
  yOffset: number;
  colorRgba: number;
}

export interface PortableTriggerV1 {
  condition: TriggerConditionDto;
  channelPath: string | null;
}

export interface PortableScopeConfigV1 {
  title: string;
  timeSpanNs: number;
  preTriggerRatio: number;
  panOffsetNs: number;
  yAxisScale: number;
  yAxisOffset: number;
  wheelZoomTarget: WheelZoomTarget;
  sweepMode: TriggerSweepModeDto;
  autoTimeoutMs: number;
  trigger: PortableTriggerV1 | null;
  tracks: PortableScopeTrackV1[];
  syncGroupId: number | null;
  paused: boolean;
  analogLayout: AnalogLayoutMode;
  cursorsEnabled: boolean;
  cursorARatio: number;
  cursorBRatio: number;
  activeAnalogChannelPath: string | null;
}

export type PortableEmbeddedScopeV1 = PortableScopeConfigV1;

export interface PortableStandaloneScopeV1 {
  scopeId: number;
  config: PortableScopeConfigV1;
}

export interface PortableCanvasWidgetV1 {
  id?: string;
  type: WidgetType;
  title: string;
  col: number;
  row: number;
  colSpan: number;
  rowSpan: number;
  primaryBinding: PortableWidgetChannelBindingV1 | null;
  bindings: PortableWidgetChannelBindingV1[];
  embeddedScope?: PortableScopeConfigV1 | null;
  gaugeConfig?: WidgetGaugeConfig;
  sliderConfig?: WidgetSliderConfig;
  buttonConfig?: WidgetButtonConfig;
}

export interface PortableDashboardCanvasV1 {
  canvasId: number;
  title: string;
  locked: boolean;
  widgets: PortableCanvasWidgetV1[];
}

export interface ProjectBoardSectionsV1 {
  ui: BoardUiLayoutStateV1;
  downlink: DownlinkUiConfig;
  dockviewLayout: Record<string, unknown> | null;
  standaloneScopes: PortableStandaloneScopeV1[];
  canvases: PortableDashboardCanvasV1[];
  commands?: CommandDefinition[];
}

export interface BoardConfigFileV1 extends ProjectBoardSectionsV1 {
  schema: typeof BOARD_CONFIG_SCHEMA_V1;
  exportedAt: string;
}

export interface ProjectConfigFileV1 {
  schema: typeof PROJECT_CONFIG_SCHEMA_V1;
  exportedAt: string;
  sources: CreateSourceRequestDto_Serialize[];
  board: ProjectBoardSectionsV1;
}

export interface CanvasConfigFileV1 {
  schema: typeof CANVAS_CONFIG_SCHEMA_V1;
  exportedAt: string;
  canvas: {
    title: string;
    locked: boolean;
    widgets: PortableCanvasWidgetV1[];
  };
}

const VALID_TRANSPORT_KINDS = new Set([
  "sim",
  "serial",
  "udp",
  "tcp_client",
  "tcp_server",
  "raw_file_replay",
]);

const VALID_CLOCK_MODES = new Set([
  "tier1_hardware_counter",
  "tier2_software_pll",
  "tier3_host_arrival",
]);

const VALID_PROTOCOL_CODECS = new Set([
  "sim_preset",
  "just_float",
  "fixed_schema_binary",
  "fire_water",
]);

const VALID_SWEEP_MODES = new Set<TriggerSweepModeDto>([
  "roll",
  "auto",
  "normal",
  "single",
]);

const VALID_ANALOG_LAYOUTS = new Set<AnalogLayoutMode>(["overlay", "stacked"]);
const VALID_WHEEL_ZOOM_TARGETS = new Set<WheelZoomTarget>(["x", "y", "xy"]);
const VALID_LANES = new Set<ScopeLaneKindDto>(["analog", "digital", "state_ribbon"]);

function isRecord(val: unknown): val is Record<string, unknown> {
  return typeof val === "object" && val !== null && !Array.isArray(val);
}

function finiteOr(val: unknown, fallback: number): number {
  return typeof val === "number" && Number.isFinite(val) ? val : fallback;
}

export const normalizeSourceConfigForExport = normalizeSourceConfigToSerialize;

export function synthesizeFallbackSourceConfig(
  group: SourceChannelGroupDto_Serialize,
): CreateSourceRequestDto_Serialize {
  const sourceId = group.source.source_id;
  const displayName = group.source.display_name ?? null;
  const autoStart = group.source.state === "running";
  const channelNames = group.channels
    .map((ch) => ch.local_path || ch.display_name)
    .filter((name) => name.length > 0);
  const safeChannelNames = channelNames.length > 0 ? channelNames : ["ch0", "ch1"];
  const analogCount = Math.max(
    1,
    group.channels.filter((ch) => ch.modality === "scalar").length || safeChannelNames.length,
  );

  switch (group.source.transport_kind) {
    case "serial":
      return {
        source_id: sourceId,
        display_name: displayName,
        transport: {
          kind: "serial",
          port_name: "COM1",
          baud_rate: 921_600,
          data_bits: 8,
          parity: "none",
          stop_bits: "one",
          flow_control: "none",
          auto_reconnect: true,
        },
        clock: {
          mode: "tier2_software_pll",
          nominal_sample_rate_hz: 100_000,
        },
        protocol: {
          codec: "just_float",
          channel_names: safeChannelNames,
        },
        auto_start: autoStart,
      };
    case "udp":
      return {
        source_id: sourceId,
        display_name: displayName,
        transport: {
          kind: "udp",
          bind_addr: "0.0.0.0:9001",
          multicast_group: null,
          recv_buffer_bytes: 1_048_576,
          remote_tx_addr: null,
        },
        clock: {
          mode: "tier2_software_pll",
          nominal_sample_rate_hz: 100_000,
        },
        protocol: {
          codec: "just_float",
          channel_names: safeChannelNames,
        },
        auto_start: autoStart,
      };
    case "tcp_client":
      return {
        source_id: sourceId,
        display_name: displayName,
        transport: {
          kind: "tcp_client",
          remote_addr: "127.0.0.1:9000",
          auto_reconnect: true,
          tcp_nodelay: true,
        },
        clock: {
          mode: "tier2_software_pll",
          nominal_sample_rate_hz: 100_000,
        },
        protocol: {
          codec: "just_float",
          channel_names: safeChannelNames,
        },
        auto_start: autoStart,
      };
    case "tcp_server":
      return {
        source_id: sourceId,
        display_name: displayName,
        transport: {
          kind: "tcp_server",
          listen_addr: "0.0.0.0:9000",
          tcp_nodelay: true,
        },
        clock: {
          mode: "tier2_software_pll",
          nominal_sample_rate_hz: 100_000,
        },
        protocol: {
          codec: "just_float",
          channel_names: safeChannelNames,
        },
        auto_start: autoStart,
      };
    case "raw_file_replay":
      return {
        source_id: sourceId,
        display_name: displayName,
        transport: {
          kind: "raw_file_replay",
          file_path: "capture.ddraw",
          full_speed_offline: false,
          playback_speed: 1,
          loop_playback: true,
          nominal_byte_rate: null,
        },
        clock: {
          mode: "tier2_software_pll",
          nominal_sample_rate_hz: 100_000,
        },
        protocol: {
          codec: "just_float",
          channel_names: safeChannelNames,
        },
        auto_start: autoStart,
      };
    case "sim":
    default:
      return {
        source_id: sourceId,
        display_name: displayName,
        transport: {
          kind: "sim",
          sample_rate_hz: 2_000_000,
          analog_channels: analogCount,
          enable_glitch: true,
          glitch_period_samples: 4_000_000,
        },
        clock: {
          mode: "tier1_hardware_counter",
          tick_hz: 2_000_000,
          counter_bits: 32,
        },
        protocol: {
          codec: "sim_preset",
        },
        auto_start: autoStart,
      };
  }
}

function validateAndNormalizeSourcesList(
  rawSources: unknown,
  fieldLabel: string,
): CreateSourceRequestDto_Serialize[] {
  if (!Array.isArray(rawSources)) {
    throw new Error(`'${fieldLabel}' field must be an array.`);
  }

  return rawSources.map((item, idx) => {
    if (!isRecord(item)) {
      throw new Error(`${fieldLabel}[${idx}] must be an object.`);
    }
    if (typeof item.source_id !== "string" || item.source_id.trim().length === 0) {
      throw new Error(`${fieldLabel}[${idx}].source_id must be a non-empty string.`);
    }
    if (!isRecord(item.transport) || !VALID_TRANSPORT_KINDS.has(String(item.transport.kind))) {
      throw new Error(
        `${fieldLabel}[${idx}].transport.kind '${String(
          isRecord(item.transport) ? item.transport.kind : undefined,
        )}' is invalid.`,
      );
    }
    if (!isRecord(item.clock) || !VALID_CLOCK_MODES.has(String(item.clock.mode))) {
      throw new Error(
        `${fieldLabel}[${idx}].clock.mode '${String(
          isRecord(item.clock) ? item.clock.mode : undefined,
        )}' is invalid.`,
      );
    }
    if (!isRecord(item.protocol) || !VALID_PROTOCOL_CODECS.has(String(item.protocol.codec))) {
      throw new Error(
        `${fieldLabel}[${idx}].protocol.codec '${String(
          isRecord(item.protocol) ? item.protocol.codec : undefined,
        )}' is invalid.`,
      );
    }
    if (typeof item.auto_start !== "boolean") {
      throw new Error(`${fieldLabel}[${idx}].auto_start must be a boolean.`);
    }
    return normalizeSourceConfigForExport(
      item as unknown as CreateSourceRequestDto_Deserialize,
    );
  });
}

export function serializeSourcesConfig(
  sources: ReadonlyArray<CreateSourceRequestDto_Deserialize>,
): string {
  const payload: SourcesConfigFileV1 = {
    schema: SOURCES_CONFIG_SCHEMA_V1,
    exportedAt: new Date().toISOString(),
    sources: sources.map((src) => normalizeSourceConfigForExport(src)),
  };
  return JSON.stringify(payload, null, 2);
}

export function parseSourcesConfigJson(jsonText: string): SourcesConfigFileV1 {
  let parsed: unknown;
  try {
    parsed = JSON.parse(jsonText);
  } catch (err: unknown) {
    const msg = err instanceof Error ? err.message : String(err);
    throw new Error(`Invalid JSON syntax in sources config: ${msg}`);
  }

  if (!isRecord(parsed)) {
    throw new Error("Sources config root must be a JSON object.");
  }

  if (parsed.schema === BOARD_CONFIG_SCHEMA_V1 || parsed.schema === CANVAS_CONFIG_SCHEMA_V1) {
    throw new Error(
      `Expected schema '${SOURCES_CONFIG_SCHEMA_V1}', got board config ('${parsed.schema}'). Please load it from the Stage toolbar.`,
    );
  }

  if (parsed.schema === PROJECT_CONFIG_SCHEMA_V1) {
    throw new Error(
      `Expected schema '${SOURCES_CONFIG_SCHEMA_V1}', got project config ('${parsed.schema}'). Please load it from the Project toolbar.`,
    );
  }

  if (parsed.schema === COMMANDS_CONFIG_SCHEMA_V1) {
    throw new Error(
      `Expected schema '${SOURCES_CONFIG_SCHEMA_V1}', got commands config ('${parsed.schema}'). Please load it from the Commands panel.`,
    );
  }

  if (parsed.schema !== SOURCES_CONFIG_SCHEMA_V1) {
    throw new Error(
      `Unsupported sources config schema '${String(parsed.schema ?? "missing")}'. Expected '${SOURCES_CONFIG_SCHEMA_V1}'.`,
    );
  }

  const normalizedSources = validateAndNormalizeSourcesList(parsed.sources, "sources");

  return {
    schema: SOURCES_CONFIG_SCHEMA_V1,
    exportedAt: typeof parsed.exportedAt === "string" ? parsed.exportedAt : new Date().toISOString(),
    sources: normalizedSources,
  };
}

function serializeWidgetBinding(
  binding: WidgetChannelBinding,
  channelsById: Record<number, ChannelNodeDto_Serialize>,
): PortableWidgetChannelBindingV1 {
  return {
    channelPath: channelsById[binding.channelId]?.full_path ?? "",
    fallbackChannelId: binding.channelId,
    scale: binding.scale,
    offset: binding.offset,
  };
}

function serializePortableScope(
  scope: ScopeUiConfig,
  channelsById: Record<number, ChannelNodeDto_Serialize>,
): PortableScopeConfigV1 {
  const triggerChannelPath = scope.trigger
    ? channelsById[scope.trigger.channel_id]?.full_path ?? null
    : null;
  const activeAnalogChannelPath =
    scope.activeAnalogChannelId !== null
      ? channelsById[scope.activeAnalogChannelId]?.full_path ?? null
      : null;

  return {
    title: scope.title,
    timeSpanNs: scope.timeSpanNs,
    preTriggerRatio: scope.preTriggerRatio,
    panOffsetNs: scope.panOffsetNs,
    yAxisScale: scope.yAxisScale,
    yAxisOffset: scope.yAxisOffset,
    wheelZoomTarget: scope.wheelZoomTarget ?? "xy",
    sweepMode: scope.sweepMode,
    autoTimeoutMs: scope.autoTimeoutMs,
    trigger: scope.trigger
      ? {
          condition: { ...scope.trigger },
          channelPath: triggerChannelPath,
        }
      : null,
    tracks: scope.tracks.map((tr) => ({
      channelPath: channelsById[tr.channelId]?.full_path ?? "",
      fallbackChannelId: tr.channelId,
      lane: tr.lane,
      visible: tr.visible,
      yScale: tr.yScale,
      yOffset: tr.yOffset,
      colorRgba: tr.colorRgba >>> 0,
    })),
    syncGroupId: scope.syncGroupId,
    paused: scope.paused,
    analogLayout: scope.analogLayout,
    cursorsEnabled: scope.cursorsEnabled,
    cursorARatio: scope.cursorARatio,
    cursorBRatio: scope.cursorBRatio,
    activeAnalogChannelPath,
  };
}

function serializeWidgetList(
  canvas: DashboardCanvasConfig,
  scopes: Record<number, ScopeUiConfig>,
  channelsById: Record<number, ChannelNodeDto_Serialize>,
): PortableCanvasWidgetV1[] {
  return canvas.widgets.map((w) => {
    const scope =
      w.type === "mso_scope" && w.scopeId !== null ? scopes[w.scopeId] ?? null : null;

    return {
      id: w.id,
      type: w.type,
      title: w.title,
      col: w.col,
      row: w.row,
      colSpan: w.colSpan,
      rowSpan: w.rowSpan,
      primaryBinding: w.primaryBinding
        ? serializeWidgetBinding(w.primaryBinding, channelsById)
        : null,
      bindings: w.bindings.map((b) => serializeWidgetBinding(b, channelsById)),
      ...(w.type === "mso_scope"
        ? {
            embeddedScope: scope ? serializePortableScope(scope, channelsById) : null,
          }
        : {}),
      ...(w.gaugeConfig ? { gaugeConfig: { ...w.gaugeConfig } } : {}),
      ...(w.sliderConfig ? { sliderConfig: { ...w.sliderConfig } } : {}),
      ...(w.buttonConfig ? { buttonConfig: { ...w.buttonConfig } } : {}),
    };
  });
}

export interface BoardExportInput {
  scopes: Record<number, ScopeUiConfig>;
  scopeOrder: number[];
  canvases: Record<number, DashboardCanvasConfig>;
  canvasOrder: number[];
  channelsById: Record<number, ChannelNodeDto_Serialize>;
  ui: BoardUiLayoutStateV1;
  downlink: DownlinkUiConfig;
  dockviewLayout: Record<string, unknown> | null;
  commands?: CommandDefinition[];
}

function buildBoardConfigSections(input: BoardExportInput): ProjectBoardSectionsV1 {
  const standaloneScopes: PortableStandaloneScopeV1[] = [];
  for (const scopeId of input.scopeOrder) {
    const sc = input.scopes[scopeId];
    if (!sc) continue;
    standaloneScopes.push({
      scopeId,
      config: serializePortableScope(sc, input.channelsById),
    });
  }

  const canvases: PortableDashboardCanvasV1[] = [];
  for (const canvasId of input.canvasOrder) {
    const cv = input.canvases[canvasId];
    if (!cv) continue;
    canvases.push({
      canvasId,
      title: cv.title,
      locked: cv.locked,
      widgets: serializeWidgetList(cv, input.scopes, input.channelsById),
    });
  }

  return {
    ui: { ...input.ui },
    downlink: { ...input.downlink },
    dockviewLayout: input.dockviewLayout,
    standaloneScopes,
    canvases,
    commands: (input.commands ?? []).map((cmd) => ({ ...cmd })),
  };
}

/**
 * Serialize the entire workspace board arrangement and all control configurations
 * (Dockview split tree, all standalone MSO scopes, all canvases/widgets, UI chrome,
 * and downlink bar settings) into portable JSON (`ddscope.board.v1`).
 */
export function serializeBoardConfig(input: BoardExportInput): string {
  const payload: BoardConfigFileV1 = {
    schema: BOARD_CONFIG_SCHEMA_V1,
    exportedAt: new Date().toISOString(),
    ...buildBoardConfigSections(input),
  };

  return JSON.stringify(payload, null, 2);
}

/**
 * Serialize a full project configuration (`ddscope.project.v1`), combining
 * data source configurations with the whole-page board layout & controls.
 */
export function serializeProjectConfig(
  input: BoardExportInput & {
    sources: ReadonlyArray<CreateSourceRequestDto_Deserialize>;
  },
): string {
  const payload: ProjectConfigFileV1 = {
    schema: PROJECT_CONFIG_SCHEMA_V1,
    exportedAt: new Date().toISOString(),
    sources: input.sources.map((src) => normalizeSourceConfigForExport(src)),
    board: buildBoardConfigSections(input),
  };

  return JSON.stringify(payload, null, 2);
}

function parsePortableWidgets(
  rawWidgets: unknown[],
  pathLabel: string,
): PortableCanvasWidgetV1[] {
  return rawWidgets.map((wRaw, idx) => {
    if (!isRecord(wRaw)) {
      throw new Error(`${pathLabel}[${idx}] must be an object.`);
    }
    const widgetType = wRaw.type;
    if (
      typeof widgetType !== "string" ||
      !(WIDGET_TYPES as readonly string[]).includes(widgetType)
    ) {
      throw new Error(`${pathLabel}[${idx}].type '${String(widgetType)}' is invalid.`);
    }
    const id =
      typeof wRaw.id === "string" && wRaw.id.trim().length > 0
        ? wRaw.id.trim()
        : undefined;
    return {
      ...(id ? { id } : {}),
      type: widgetType as WidgetType,
      title: typeof wRaw.title === "string" ? wRaw.title : widgetType,
      col: finiteOr(wRaw.col, 0),
      row: finiteOr(wRaw.row, 0),
      colSpan: finiteOr(wRaw.colSpan, 8),
      rowSpan: finiteOr(wRaw.rowSpan, 4),
      primaryBinding: isRecord(wRaw.primaryBinding)
        ? (wRaw.primaryBinding as unknown as PortableWidgetChannelBindingV1)
        : null,
      bindings: Array.isArray(wRaw.bindings)
        ? (wRaw.bindings.filter(isRecord) as unknown as PortableWidgetChannelBindingV1[])
        : [],
      ...(wRaw.embeddedScope !== undefined
        ? {
            embeddedScope: isRecord(wRaw.embeddedScope)
              ? (wRaw.embeddedScope as unknown as PortableScopeConfigV1)
              : null,
          }
        : {}),
      ...(isRecord(wRaw.gaugeConfig)
        ? { gaugeConfig: wRaw.gaugeConfig as unknown as WidgetGaugeConfig }
        : {}),
      ...(isRecord(wRaw.sliderConfig)
        ? { sliderConfig: wRaw.sliderConfig as unknown as WidgetSliderConfig }
        : {}),
      ...(isRecord(wRaw.buttonConfig)
        ? { buttonConfig: wRaw.buttonConfig as unknown as WidgetButtonConfig }
        : {}),
    };
  });
}

function normalizeUiLayoutState(
  rawUi: unknown,
  fallbackActiveCanvasId: number | null,
  fallbackFocusedScopeId: number | null,
): BoardUiLayoutStateV1 {
  const uiRec = isRecord(rawUi) ? rawUi : {};
  const sidebarTab =
    uiRec.sidebarTab === "toolbox"
      ? "toolbox"
      : uiRec.sidebarTab === "commands"
        ? "commands"
        : "sources";
  return {
    mockRateHz: uiRec.mockRateHz === 60 ? 60 : 120,
    sidebarCollapsed: Boolean(uiRec.sidebarCollapsed),
    sidebarTab,
    downlinkBarCollapsed: Boolean(uiRec.downlinkBarCollapsed),
    focusedScopeId:
      typeof uiRec.focusedScopeId === "number" && Number.isFinite(uiRec.focusedScopeId)
        ? uiRec.focusedScopeId
        : fallbackFocusedScopeId,
    activeCanvasId:
      typeof uiRec.activeCanvasId === "number" && Number.isFinite(uiRec.activeCanvasId)
        ? uiRec.activeCanvasId
        : fallbackActiveCanvasId,
  };
}

function normalizeDownlinkUiConfig(rawDownlink: unknown): DownlinkUiConfig {
  const dl = isRecord(rawDownlink) ? rawDownlink : {};
  const manualMode =
    dl.manualMode === "hex_bytes" || dl.manualMode === "key_value"
      ? dl.manualMode
      : "ascii_text";
  return {
    targetSource:
      typeof dl.targetSource === "string"
        ? dl.targetSource
        : DEFAULT_DOWNLINK_UI_CONFIG.targetSource,
    priority: dl.priority === "high" ? "high" : "normal",
    manualMode,
    asciiText:
      typeof dl.asciiText === "string" ? dl.asciiText : DEFAULT_DOWNLINK_UI_CONFIG.asciiText,
    appendNewline:
      typeof dl.appendNewline === "boolean"
        ? dl.appendNewline
        : DEFAULT_DOWNLINK_UI_CONFIG.appendNewline,
    hexText: typeof dl.hexText === "string" ? dl.hexText : DEFAULT_DOWNLINK_UI_CONFIG.hexText,
    kvKey: typeof dl.kvKey === "string" ? dl.kvKey : DEFAULT_DOWNLINK_UI_CONFIG.kvKey,
    kvVal: typeof dl.kvVal === "string" ? dl.kvVal : DEFAULT_DOWNLINK_UI_CONFIG.kvVal,
  };
}

function validateAndNormalizeBoardSections(
  rawBoard: Record<string, unknown>,
  fieldPrefix = "",
): ProjectBoardSectionsV1 {
  const prefixLabel = fieldPrefix ? `${fieldPrefix}.` : "";

  if (!Array.isArray(rawBoard.standaloneScopes)) {
    throw new Error(`'${prefixLabel}standaloneScopes' field must be an array.`);
  }

  const standaloneScopes: PortableStandaloneScopeV1[] = rawBoard.standaloneScopes.map(
    (item, idx) => {
      if (!isRecord(item)) {
        throw new Error(`${prefixLabel}standaloneScopes[${idx}] must be an object.`);
      }
      if (
        typeof item.scopeId !== "number" ||
        !Number.isInteger(item.scopeId) ||
        item.scopeId <= 0
      ) {
        throw new Error(`${prefixLabel}standaloneScopes[${idx}].scopeId must be a positive integer.`);
      }
      if (!isRecord(item.config)) {
        throw new Error(`${prefixLabel}standaloneScopes[${idx}].config must be an object.`);
      }
      return {
        scopeId: item.scopeId,
        config: item.config as unknown as PortableScopeConfigV1,
      };
    },
  );

  if (!Array.isArray(rawBoard.canvases)) {
    throw new Error(`'${prefixLabel}canvases' field must be an array.`);
  }

  const canvases: PortableDashboardCanvasV1[] = rawBoard.canvases.map((item, idx) => {
    if (!isRecord(item)) {
      throw new Error(`${prefixLabel}canvases[${idx}] must be an object.`);
    }
    if (
      typeof item.canvasId !== "number" ||
      !Number.isInteger(item.canvasId) ||
      item.canvasId <= 0
    ) {
      throw new Error(`${prefixLabel}canvases[${idx}].canvasId must be a positive integer.`);
    }
    if (typeof item.title !== "string") {
      throw new Error(`${prefixLabel}canvases[${idx}].title must be a string.`);
    }
    if (!Array.isArray(item.widgets)) {
      throw new Error(`${prefixLabel}canvases[${idx}].widgets must be an array.`);
    }
    return {
      canvasId: item.canvasId,
      title: item.title,
      locked: Boolean(item.locked),
      widgets: parsePortableWidgets(item.widgets, `${prefixLabel}canvases[${idx}].widgets`),
    };
  });

  const fallbackFocusedScopeId = standaloneScopes[0]?.scopeId ?? null;
  const fallbackActiveCanvasId = canvases[0]?.canvasId ?? null;

  let commands: CommandDefinition[] = [];
  if (rawBoard.commands !== undefined) {
    if (!Array.isArray(rawBoard.commands)) {
      throw new Error(`'${prefixLabel}commands' field must be an array.`);
    }
    commands = rawBoard.commands.map((item, idx) => {
      if (!isRecord(item)) {
        throw new Error(`${prefixLabel}commands[${idx}] must be an object.`);
      }
      return normalizeCommandDefinition(item, idx + 1);
    });
  }

  return {
    ui: normalizeUiLayoutState(rawBoard.ui, fallbackActiveCanvasId, fallbackFocusedScopeId),
    downlink: normalizeDownlinkUiConfig(rawBoard.downlink),
    dockviewLayout: isRecord(rawBoard.dockviewLayout) ? rawBoard.dockviewLayout : null,
    standaloneScopes,
    canvases,
    commands,
  };
}

export function serializeCommandsConfig(commands: ReadonlyArray<CommandDefinition>): string {
  const payload: CommandsConfigFileV1 = {
    schema: COMMANDS_CONFIG_SCHEMA_V1,
    exportedAt: new Date().toISOString(),
    commands: commands.map((cmd, idx) => normalizeCommandDefinition(cmd, idx + 1)),
  };
  return JSON.stringify(payload, null, 2);
}

export function parseCommandsConfigJson(jsonText: string): CommandsConfigFileV1 {
  let parsed: unknown;
  try {
    parsed = JSON.parse(jsonText);
  } catch (err: unknown) {
    const msg = err instanceof Error ? err.message : String(err);
    throw new Error(`Invalid JSON syntax in commands config: ${msg}`);
  }

  if (!isRecord(parsed)) {
    throw new Error("Commands config root must be a JSON object.");
  }

  if (parsed.schema === SOURCES_CONFIG_SCHEMA_V1) {
    throw new Error(
      `Expected schema '${COMMANDS_CONFIG_SCHEMA_V1}', got sources config ('${SOURCES_CONFIG_SCHEMA_V1}'). Please load it from the Sources sidebar.`,
    );
  }

  if (parsed.schema === BOARD_CONFIG_SCHEMA_V1 || parsed.schema === CANVAS_CONFIG_SCHEMA_V1) {
    throw new Error(
      `Expected schema '${COMMANDS_CONFIG_SCHEMA_V1}', got board config ('${parsed.schema}'). Please load it from the Stage toolbar.`,
    );
  }

  if (parsed.schema === PROJECT_CONFIG_SCHEMA_V1) {
    throw new Error(
      `Expected schema '${COMMANDS_CONFIG_SCHEMA_V1}', got project config ('${PROJECT_CONFIG_SCHEMA_V1}'). Please load it from the Project toolbar.`,
    );
  }

  if (parsed.schema !== COMMANDS_CONFIG_SCHEMA_V1) {
    throw new Error(
      `Unsupported commands config schema '${String(parsed.schema ?? "missing")}'. Expected '${COMMANDS_CONFIG_SCHEMA_V1}'.`,
    );
  }

  if (!Array.isArray(parsed.commands)) {
    throw new Error("Commands config 'commands' field must be an array.");
  }

  const commands = parsed.commands.map((item, idx) => {
    if (!isRecord(item)) {
      throw new Error(`commands[${idx}] must be an object.`);
    }
    return normalizeCommandDefinition(item, idx + 1);
  });

  return {
    schema: COMMANDS_CONFIG_SCHEMA_V1,
    exportedAt:
      typeof parsed.exportedAt === "string" ? parsed.exportedAt : new Date().toISOString(),
    commands,
  };
}

/**
 * Parse and validate a whole-page board config JSON (`ddscope.board.v1`),
 * automatically upgrading legacy single-canvas (`ddscope.canvas.v1`) files and
 * rejecting data-source (`ddscope.sources.v1`) or project (`ddscope.project.v1`)
 * configs with a descriptive error.
 */
export function parseBoardConfigJson(jsonText: string): BoardConfigFileV1 {
  let parsed: unknown;
  try {
    parsed = JSON.parse(jsonText);
  } catch (err: unknown) {
    const msg = err instanceof Error ? err.message : String(err);
    throw new Error(`Invalid JSON syntax in board config: ${msg}`);
  }

  if (!isRecord(parsed)) {
    throw new Error("Board config root must be a JSON object.");
  }

  if (parsed.schema === SOURCES_CONFIG_SCHEMA_V1) {
    throw new Error(
      `Expected schema '${BOARD_CONFIG_SCHEMA_V1}', got sources config ('${SOURCES_CONFIG_SCHEMA_V1}'). Please load it from the Sources sidebar.`,
    );
  }

  if (parsed.schema === PROJECT_CONFIG_SCHEMA_V1) {
    throw new Error(
      `Expected schema '${BOARD_CONFIG_SCHEMA_V1}', got project config ('${PROJECT_CONFIG_SCHEMA_V1}'). Please load it from the Project toolbar.`,
    );
  }

  if (parsed.schema === COMMANDS_CONFIG_SCHEMA_V1) {
    throw new Error(
      `Expected schema '${BOARD_CONFIG_SCHEMA_V1}', got commands config ('${COMMANDS_CONFIG_SCHEMA_V1}'). Please load it from the Commands panel.`,
    );
  }

  // Backward-compatible upgrade for legacy single-canvas files
  if (parsed.schema === CANVAS_CONFIG_SCHEMA_V1) {
    if (!isRecord(parsed.canvas)) {
      throw new Error("Legacy canvas config 'canvas' field must be an object.");
    }
    if (typeof parsed.canvas.title !== "string") {
      throw new Error("Legacy canvas config 'canvas.title' must be a string.");
    }
    if (!Array.isArray(parsed.canvas.widgets)) {
      throw new Error("Legacy canvas config 'canvas.widgets' must be an array.");
    }
    const widgets = parsePortableWidgets(parsed.canvas.widgets, "canvas.widgets");
    return {
      schema: BOARD_CONFIG_SCHEMA_V1,
      exportedAt:
        typeof parsed.exportedAt === "string" ? parsed.exportedAt : new Date().toISOString(),
      ui: normalizeUiLayoutState(undefined, 1, null),
      downlink: { ...DEFAULT_DOWNLINK_UI_CONFIG },
      dockviewLayout: null,
      standaloneScopes: [],
      canvases: [
        {
          canvasId: 1,
          title: parsed.canvas.title,
          locked: Boolean(parsed.canvas.locked),
          widgets,
        },
      ],
    };
  }

  if (parsed.schema !== BOARD_CONFIG_SCHEMA_V1) {
    throw new Error(
      `Unsupported board config schema '${String(parsed.schema ?? "missing")}'. Expected '${BOARD_CONFIG_SCHEMA_V1}'.`,
    );
  }

  return {
    schema: BOARD_CONFIG_SCHEMA_V1,
    exportedAt:
      typeof parsed.exportedAt === "string" ? parsed.exportedAt : new Date().toISOString(),
    ...validateAndNormalizeBoardSections(parsed),
  };
}

/**
 * Parse and validate a full project config JSON (`ddscope.project.v1`),
 * containing both `sources` and `board`.
 */
export function parseProjectConfigJson(jsonText: string): ProjectConfigFileV1 {
  let parsed: unknown;
  try {
    parsed = JSON.parse(jsonText);
  } catch (err: unknown) {
    const msg = err instanceof Error ? err.message : String(err);
    throw new Error(`Invalid JSON syntax in project config: ${msg}`);
  }

  if (!isRecord(parsed)) {
    throw new Error("Project config root must be a JSON object.");
  }

  if (parsed.schema === SOURCES_CONFIG_SCHEMA_V1) {
    throw new Error(
      `Expected schema '${PROJECT_CONFIG_SCHEMA_V1}', got sources config ('${SOURCES_CONFIG_SCHEMA_V1}'). Please load it from the Sources sidebar.`,
    );
  }

  if (parsed.schema === BOARD_CONFIG_SCHEMA_V1 || parsed.schema === CANVAS_CONFIG_SCHEMA_V1) {
    throw new Error(
      `Expected schema '${PROJECT_CONFIG_SCHEMA_V1}', got board config ('${parsed.schema}'). Please load it from the Stage toolbar.`,
    );
  }

  if (parsed.schema === COMMANDS_CONFIG_SCHEMA_V1) {
    throw new Error(
      `Expected schema '${PROJECT_CONFIG_SCHEMA_V1}', got commands config ('${COMMANDS_CONFIG_SCHEMA_V1}'). Please load it from the Commands panel.`,
    );
  }

  if (parsed.schema !== PROJECT_CONFIG_SCHEMA_V1) {
    throw new Error(
      `Unsupported project config schema '${String(parsed.schema ?? "missing")}'. Expected '${PROJECT_CONFIG_SCHEMA_V1}'.`,
    );
  }

  const sources = validateAndNormalizeSourcesList(parsed.sources, "sources");
  if (!isRecord(parsed.board)) {
    throw new Error("Project config 'board' field must be an object.");
  }

  return {
    schema: PROJECT_CONFIG_SCHEMA_V1,
    exportedAt:
      typeof parsed.exportedAt === "string" ? parsed.exportedAt : new Date().toISOString(),
    sources,
    board: validateAndNormalizeBoardSections(parsed.board, "board"),
  };
}

export function projectToBoardFile(project: ProjectConfigFileV1): BoardConfigFileV1 {
  return {
    schema: BOARD_CONFIG_SCHEMA_V1,
    exportedAt: project.exportedAt,
    ...project.board,
  };
}

function sanitizeDockviewLayout(
  rawLayout: Record<string, unknown> | null,
  scopes: Record<number, ScopeUiConfig>,
  scopeOrder: number[],
  canvases: Record<number, DashboardCanvasConfig>,
  canvasOrder: number[],
): Record<string, unknown> | null {
  if (!rawLayout) return null;
  if (scopeOrder.length + canvasOrder.length === 0) return null;
  if (
    !isRecord(rawLayout.grid) ||
    !isRecord(rawLayout.grid.root) ||
    rawLayout.grid.root.type !== "branch" ||
    !Array.isArray(rawLayout.grid.root.data) ||
    !isRecord(rawLayout.panels)
  ) {
    return null;
  }

  const rawPanels = rawLayout.panels;
  if (Object.keys(rawPanels).length === 0) {
    return null;
  }

  const expectedScopePanels = new Map<string, number>();
  for (const sid of scopeOrder) {
    expectedScopePanels.set(`scope-${sid}`, sid);
  }
  const expectedCanvasPanels = new Map<string, number>();
  for (const cid of canvasOrder) {
    expectedCanvasPanels.set(`canvas-${cid}`, cid);
  }

  const sanitizedPanels: Record<string, unknown> = {};
  for (const [panelKey, panelVal] of Object.entries(rawPanels)) {
    if (!isRecord(panelVal)) return null;

    if (expectedScopePanels.has(panelKey)) {
      if (
        panelVal.contentComponent !== undefined &&
        panelVal.contentComponent !== "msoScope"
      ) {
        return null;
      }
      const sid = expectedScopePanels.get(panelKey)!;
      sanitizedPanels[panelKey] = {
        ...panelVal,
        id: panelKey,
        contentComponent: "msoScope",
        title: scopes[sid]?.title ?? panelVal.title,
        params: {
          ...(isRecord(panelVal.params) ? panelVal.params : {}),
          scopeId: sid,
        },
      };
    } else if (expectedCanvasPanels.has(panelKey)) {
      if (
        panelVal.contentComponent !== undefined &&
        panelVal.contentComponent !== "dashboardCanvas"
      ) {
        return null;
      }
      const cid = expectedCanvasPanels.get(panelKey)!;
      sanitizedPanels[panelKey] = {
        ...panelVal,
        id: panelKey,
        contentComponent: "dashboardCanvas",
        title: canvases[cid]?.title ?? panelVal.title,
        params: {
          ...(isRecord(panelVal.params) ? panelVal.params : {}),
          canvasId: cid,
        },
      };
    } else {
      return null;
    }
  }

  return {
    ...rawLayout,
    panels: sanitizedPanels,
  };
}

/**
 * Rehydrate an entire workspace board (`scopes`, `canvases`, widgets, Dockview layout,
 * page chrome toggles, and downlink bar settings) from a validated `BoardConfigFileV1`,
 * resolving channels primarily by portable `full_path` without false ID collisions.
 */
export function rehydrateBoardFromConfig(args: {
  file: BoardConfigFileV1;
  channelsById: Record<number, ChannelNodeDto_Serialize>;
  availableSourceIds?: string[];
}): {
  scopes: Record<number, ScopeUiConfig>;
  scopeOrder: number[];
  canvases: Record<number, DashboardCanvasConfig>;
  canvasOrder: number[];
  nextScopeId: number;
  nextCanvasId: number;
  nextWidgetSeq: number;
  ui: BoardUiLayoutStateV1;
  downlink: DownlinkUiConfig;
  dockviewLayout: Record<string, unknown> | null;
  commands: CommandDefinition[];
  nextCommandSeq: number;
  missingChannelPaths: string[];
} {
  const { file, channelsById, availableSourceIds } = args;

  const channelByFullPath = new Map<string, ChannelNodeDto_Serialize>();
  for (const ch of Object.values(channelsById)) {
    channelByFullPath.set(ch.full_path, ch);
  }

  const missingSet = new Set<string>();
  const resolveChannelId = (
    channelPath: string | null | undefined,
    fallbackChannelId?: number,
  ): number | null => {
    if (typeof channelPath === "string" && channelPath.length > 0) {
      const matched = channelByFullPath.get(channelPath);
      if (matched) {
        return matched.channel_id;
      }
      missingSet.add(channelPath);
      return null;
    }

    if (
      typeof fallbackChannelId === "number" &&
      Number.isFinite(fallbackChannelId) &&
      channelsById[fallbackChannelId]
    ) {
      return fallbackChannelId;
    }
    return null;
  };

  const rehydrateScopeConfig = (
    scopeId: number,
    rawScope: PortableScopeConfigV1 | null | undefined,
    fallbackTitle: string,
  ): ScopeUiConfig => {
    const tracks: ScopeTrackUiBinding[] = [];

    if (rawScope && Array.isArray(rawScope.tracks)) {
      for (const rawTrack of rawScope.tracks) {
        if (!isRecord(rawTrack)) continue;
        const resolvedId = resolveChannelId(
          typeof rawTrack.channelPath === "string" ? rawTrack.channelPath : null,
          typeof rawTrack.fallbackChannelId === "number"
            ? rawTrack.fallbackChannelId
            : undefined,
        );
        if (resolvedId === null) continue;
        if (tracks.some((tr) => tr.channelId === resolvedId)) continue;
        const lane: ScopeLaneKindDto = VALID_LANES.has(rawTrack.lane)
          ? rawTrack.lane
          : "analog";
        const yScaleRaw = finiteOr(rawTrack.yScale, 1);
        tracks.push({
          channelId: resolvedId,
          lane,
          visible: typeof rawTrack.visible === "boolean" ? rawTrack.visible : true,
          yScale: yScaleRaw === 0 ? 1 : yScaleRaw,
          yOffset: finiteOr(rawTrack.yOffset, 0),
          colorRgba:
            (typeof rawTrack.colorRgba === "number" ? rawTrack.colorRgba : 0x38bdf8ff) >>> 0,
        });
      }
    }

    let trigger: TriggerConditionDto | null = null;
    if (rawScope?.trigger && isRecord(rawScope.trigger) && isRecord(rawScope.trigger.condition)) {
      const cond = rawScope.trigger.condition as TriggerConditionDto;
      const resolvedTrigId = resolveChannelId(
        typeof rawScope.trigger.channelPath === "string" ? rawScope.trigger.channelPath : null,
        cond.channel_id,
      );
      if (resolvedTrigId !== null) {
        trigger = { ...cond, channel_id: resolvedTrigId };
      }
    }

    let activeAnalogChannelId: number | null = null;
    if (rawScope?.activeAnalogChannelPath) {
      const matchedActive = channelByFullPath.get(rawScope.activeAnalogChannelPath);
      if (
        matchedActive &&
        tracks.some((tr) => tr.channelId === matchedActive.channel_id && tr.lane === "analog")
      ) {
        activeAnalogChannelId = matchedActive.channel_id;
      }
    }
    if (activeAnalogChannelId === null) {
      activeAnalogChannelId = tracks.find((tr) => tr.lane === "analog")?.channelId ?? null;
    }

    return {
      scopeId,
      title: typeof rawScope?.title === "string" && rawScope.title ? rawScope.title : fallbackTitle,
      timeSpanNs: Math.max(1_000, finiteOr(rawScope?.timeSpanNs, 2_000_000)),
      preTriggerRatio: Math.max(0.05, Math.min(0.95, finiteOr(rawScope?.preTriggerRatio, 0.2))),
      panOffsetNs: finiteOr(rawScope?.panOffsetNs, 0),
      yAxisScale: Math.max(1e-6, finiteOr(rawScope?.yAxisScale, 25)),
      yAxisOffset: finiteOr(rawScope?.yAxisOffset, 0),
      wheelZoomTarget:
        rawScope && VALID_WHEEL_ZOOM_TARGETS.has(rawScope.wheelZoomTarget)
          ? rawScope.wheelZoomTarget
          : "xy",
      sweepMode:
        rawScope && VALID_SWEEP_MODES.has(rawScope.sweepMode) ? rawScope.sweepMode : "auto",
      autoTimeoutMs: Math.max(50, finiteOr(rawScope?.autoTimeoutMs, 250)),
      trigger,
      tracks,
      syncGroupId:
        typeof rawScope?.syncGroupId === "number" && Number.isFinite(rawScope.syncGroupId)
          ? rawScope.syncGroupId
          : null,
      paused: Boolean(rawScope?.paused),
      analogLayout:
        rawScope && VALID_ANALOG_LAYOUTS.has(rawScope.analogLayout)
          ? rawScope.analogLayout
          : "overlay",
      cursorsEnabled: Boolean(rawScope?.cursorsEnabled),
      cursorARatio: finiteOr(rawScope?.cursorARatio, 0.25),
      cursorBRatio: finiteOr(rawScope?.cursorBRatio, 0.75),
      activeAnalogChannelId,
    };
  };

  const scopes: Record<number, ScopeUiConfig> = {};
  const scopeOrder: number[] = [];
  const usedScopeIds = new Set<number>();
  let fallbackScopeCursor = 1;

  for (const entry of file.standaloneScopes) {
    let scopeId = entry.scopeId;
    if (!Number.isInteger(scopeId) || scopeId <= 0 || usedScopeIds.has(scopeId)) {
      while (usedScopeIds.has(fallbackScopeCursor)) {
        fallbackScopeCursor += 1;
      }
      scopeId = fallbackScopeCursor;
    }
    usedScopeIds.add(scopeId);
    scopeOrder.push(scopeId);
    scopes[scopeId] = rehydrateScopeConfig(scopeId, entry.config, `MSO Scope ${scopeId}`);
  }

  let cursorScopeId = Math.max(0, ...scopeOrder) + 1;

  // Pre-scan explicit widget IDs across canvases so generated fallback IDs never collide
  const reservedWidgetIds = new Set<string>();
  let maxWidgetSeq = 0;
  for (const rawCanvas of file.canvases) {
    for (const rawWidget of rawCanvas.widgets) {
      if (typeof rawWidget.id === "string" && rawWidget.id.trim().length > 0) {
        const cleanId = rawWidget.id.trim();
        reservedWidgetIds.add(cleanId);
        const seqMatch = /^w_(\d+)$/.exec(cleanId);
        if (seqMatch) {
          maxWidgetSeq = Math.max(maxWidgetSeq, Number(seqMatch[1]));
        }
      }
    }
  }

  const usedWidgetIds = new Set<string>();
  const allocateNextWidgetId = (preferredId?: string): string => {
    if (preferredId && !usedWidgetIds.has(preferredId)) {
      usedWidgetIds.add(preferredId);
      return preferredId;
    }
    do {
      maxWidgetSeq += 1;
    } while (usedWidgetIds.has(`w_${maxWidgetSeq}`) || reservedWidgetIds.has(`w_${maxWidgetSeq}`));
    const fresh = `w_${maxWidgetSeq}`;
    usedWidgetIds.add(fresh);
    return fresh;
  };

  const canvases: Record<number, DashboardCanvasConfig> = {};
  const canvasOrder: number[] = [];
  const usedCanvasIds = new Set<number>();
  let fallbackCanvasCursor = 1;

  for (const rawCanvas of file.canvases) {
    let canvasId = rawCanvas.canvasId;
    if (!Number.isInteger(canvasId) || canvasId <= 0 || usedCanvasIds.has(canvasId)) {
      while (usedCanvasIds.has(fallbackCanvasCursor)) {
        fallbackCanvasCursor += 1;
      }
      canvasId = fallbackCanvasCursor;
    }
    usedCanvasIds.add(canvasId);
    canvasOrder.push(canvasId);

    const widgets: DashboardWidgetItem[] = [];
    for (const rawWidget of rawCanvas.widgets) {
      const widgetId = allocateNextWidgetId(rawWidget.id);
      const clamped = clampGridRect(
        rawWidget.col,
        rawWidget.row,
        rawWidget.colSpan,
        rawWidget.rowSpan,
      );

      const resolveWidgetBinding = (
        b: PortableWidgetChannelBindingV1 | null | undefined,
      ): WidgetChannelBinding | null => {
        if (!b) return null;
        const resolvedId = resolveChannelId(b.channelPath, b.fallbackChannelId);
        if (resolvedId === null) return null;
        return {
          channelId: resolvedId,
          scale: finiteOr(b.scale, 1),
          offset: finiteOr(b.offset, 0),
        };
      };

      const primaryBinding = resolveWidgetBinding(rawWidget.primaryBinding);
      const bindings: WidgetChannelBinding[] = [];
      for (const rawB of rawWidget.bindings) {
        const resolved = resolveWidgetBinding(rawB);
        if (resolved && !bindings.some((item) => item.channelId === resolved.channelId)) {
          bindings.push(resolved);
        }
      }

      let scopeId: number | null = null;
      if (rawWidget.type === "mso_scope") {
        while (usedScopeIds.has(cursorScopeId)) {
          cursorScopeId += 1;
        }
        scopeId = cursorScopeId;
        cursorScopeId += 1;
        usedScopeIds.add(scopeId);
        scopes[scopeId] = rehydrateScopeConfig(
          scopeId,
          rawWidget.embeddedScope,
          rawWidget.title,
        );
      }

      widgets.push({
        id: widgetId,
        type: rawWidget.type,
        title: rawWidget.title,
        col: clamped.col,
        row: clamped.row,
        colSpan: clamped.colSpan,
        rowSpan: clamped.rowSpan,
        primaryBinding,
        bindings,
        scopeId,
        ...(rawWidget.gaugeConfig ? { gaugeConfig: { ...rawWidget.gaugeConfig } } : {}),
        ...(rawWidget.sliderConfig ? { sliderConfig: { ...rawWidget.sliderConfig } } : {}),
        ...(rawWidget.buttonConfig ? { buttonConfig: { ...rawWidget.buttonConfig } } : {}),
      });
    }

    canvases[canvasId] = {
      canvasId,
      title: rawCanvas.title,
      locked: Boolean(rawCanvas.locked),
      widgets,
    };
  }

  const nextScopeId = Math.max(0, ...usedScopeIds) + 1;
  const nextCanvasId = Math.max(0, ...canvasOrder) + 1;
  const nextWidgetSeq = maxWidgetSeq + 1;

  const dockviewLayout = sanitizeDockviewLayout(
    file.dockviewLayout,
    scopes,
    scopeOrder,
    canvases,
    canvasOrder,
  );

  const focusedScopeId =
    typeof file.ui.focusedScopeId === "number" && scopes[file.ui.focusedScopeId]
      ? file.ui.focusedScopeId
      : (scopeOrder[0] ?? null);
  const activeCanvasId =
    typeof file.ui.activeCanvasId === "number" && canvases[file.ui.activeCanvasId]
      ? file.ui.activeCanvasId
      : (canvasOrder[0] ?? null);

  const ui: BoardUiLayoutStateV1 = {
    mockRateHz: file.ui.mockRateHz === 60 ? 60 : 120,
    sidebarCollapsed: Boolean(file.ui.sidebarCollapsed),
    sidebarTab:
      file.ui.sidebarTab === "toolbox"
        ? "toolbox"
        : file.ui.sidebarTab === "commands"
          ? "commands"
          : "sources",
    downlinkBarCollapsed: Boolean(file.ui.downlinkBarCollapsed),
    focusedScopeId,
    activeCanvasId,
  };

  let maxCmdSeq = 0;
  const seenCmdIds = new Set<string>();
  const commands: CommandDefinition[] = [];
  for (const rawCmd of file.commands ?? []) {
    const normalized = normalizeCommandDefinition(rawCmd, commands.length + 1);
    let finalId = normalized.id;
    if (seenCmdIds.has(finalId)) {
      maxCmdSeq += 1;
      while (seenCmdIds.has(`cmd_${maxCmdSeq}`)) {
        maxCmdSeq += 1;
      }
      finalId = `cmd_${maxCmdSeq}`;
    }
    seenCmdIds.add(finalId);
    const seqMatch = /^cmd_(\d+)$/.exec(finalId);
    if (seqMatch) {
      maxCmdSeq = Math.max(maxCmdSeq, Number(seqMatch[1]));
    }
    commands.push({ ...normalized, id: finalId });
  }
  const nextCommandSeq = Math.max(maxCmdSeq + 1, commands.length + 1);

  const targetSource =
    file.downlink.targetSource.trim().length > 0
      ? file.downlink.targetSource
      : (availableSourceIds?.[0] ?? "");

  const downlink: DownlinkUiConfig = {
    targetSource,
    priority: file.downlink.priority === "high" ? "high" : "normal",
    manualMode:
      file.downlink.manualMode === "hex_bytes" || file.downlink.manualMode === "key_value"
        ? file.downlink.manualMode
        : "ascii_text",
    asciiText: file.downlink.asciiText,
    appendNewline: Boolean(file.downlink.appendNewline),
    hexText: file.downlink.hexText,
    kvKey: file.downlink.kvKey,
    kvVal: file.downlink.kvVal,
  };

  return {
    scopes,
    scopeOrder,
    canvases,
    canvasOrder,
    nextScopeId,
    nextCanvasId,
    nextWidgetSeq,
    ui,
    downlink,
    dockviewLayout,
    commands,
    nextCommandSeq,
    missingChannelPaths: Array.from(missingSet),
  };
}
