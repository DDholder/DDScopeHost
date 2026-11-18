export const DASHBOARD_GRID_COLS = 24;

export const WIDGET_TYPES = [
  "mso_scope",
  "digital_meter",
  "radial_gauge",
  "status_led",
  "bit_parser",
  "watch_table",
  "param_slider",
  "cmd_button",
  "cmd_table",
] as const;

export type WidgetType = (typeof WIDGET_TYPES)[number];

export interface WidgetChannelBinding {
  channelId: number;
  scale: number;
  offset: number;
}

export interface WidgetGaugeConfig {
  min: number;
  max: number;
  warnHigh: number;
}

export interface WidgetSliderConfig {
  sourceId: string;
  key: string;
  min: number;
  max: number;
  step: number;
  value: number;
  commandId?: string | null;
  /** 0-based placeholder slot index (`0` -> `#1`, `1` -> `#2`, ...). */
  commandArgIndex?: number;
  /** Whether moving the slider automatically transmits the compiled command. */
  autoSend?: boolean;
}

export interface WidgetButtonConfig {
  sourceId: string;
  mode: "ascii" | "hex" | "kv";
  command: string;
  kvKey: string;
  kvValue: number;
  priority: "normal" | "high";
  commandId?: string | null;
  commandValue?: number;
  /** 0-based placeholder slot index, or `-1` to trigger-send all current placeholder values. */
  commandArgIndex?: number;
}

export interface WidgetCmdTableRow {
  id: string;
  label: string;
  values: number[];
}

export interface WidgetCmdTableConfig {
  sourceId: string;
  commandId: string | null;
  rows: WidgetCmdTableRow[];
}

export type BitParserWidth = 8 | 16 | 32;
export type BitParserTone = "info" | "ok" | "warn" | "fault";

export interface WidgetBitItem {
  /** 0-based bit index (0 = LSB, up to 31 = MSB). */
  bit: number;
  /** Message displayed when this bit is 1. */
  label: string;
  /** Badge color tone when active. */
  tone: BitParserTone;
}

export interface WidgetBitParserConfig {
  bitWidth: BitParserWidth;
  bits: WidgetBitItem[];
  /** When true, set bits without a configured label also display as 'Bit N'. */
  showUnlabeled?: boolean;
}

export interface DashboardWidgetItem {
  id: string;
  type: WidgetType;
  title: string;
  col: number;
  row: number;
  colSpan: number;
  rowSpan: number;
  primaryBinding: WidgetChannelBinding | null;
  bindings: WidgetChannelBinding[];
  scopeId: number | null;
  gaugeConfig?: WidgetGaugeConfig;
  sliderConfig?: WidgetSliderConfig;
  buttonConfig?: WidgetButtonConfig;
  cmdTableConfig?: WidgetCmdTableConfig;
  bitParserConfig?: WidgetBitParserConfig;
}

export interface DashboardCanvasConfig {
  canvasId: number;
  title: string;
  locked: boolean;
  widgets: DashboardWidgetItem[];
}

export interface WidgetCatalogEntry {
  type: WidgetType;
  badge: string;
  defaultColSpan: number;
  defaultRowSpan: number;
}

export const WIDGET_CATALOG: ReadonlyArray<WidgetCatalogEntry> = [
  { type: "mso_scope", badge: "MSO", defaultColSpan: 24, defaultRowSpan: 8 },
  { type: "digital_meter", badge: "NUM", defaultColSpan: 8, defaultRowSpan: 4 },
  { type: "radial_gauge", badge: "ARC", defaultColSpan: 8, defaultRowSpan: 5 },
  { type: "status_led", badge: "LED", defaultColSpan: 8, defaultRowSpan: 4 },
  { type: "bit_parser", badge: "BIT", defaultColSpan: 12, defaultRowSpan: 5 },
  { type: "watch_table", badge: "TBL", defaultColSpan: 16, defaultRowSpan: 5 },
  { type: "param_slider", badge: "SLD", defaultColSpan: 12, defaultRowSpan: 3 },
  { type: "cmd_button", badge: "CMD", defaultColSpan: 8, defaultRowSpan: 3 },
  { type: "cmd_table", badge: "CTB", defaultColSpan: 16, defaultRowSpan: 6 },
];

export const DASHBOARD_GRID_ROW_HEIGHT_PX = 42;
export const DASHBOARD_GRID_GAP_PX = 8;
export const DASHBOARD_GRID_ROW_STRIDE_PX =
  DASHBOARD_GRID_ROW_HEIGHT_PX + DASHBOARD_GRID_GAP_PX;

export function computeCanvasGridCell(
  clientX: number,
  clientY: number,
  rect: { left: number; top: number; width: number },
): { col: number; row: number } {
  const colWidth = Math.max(12, rect.width / DASHBOARD_GRID_COLS);
  const col = Math.max(0, Math.floor((clientX - rect.left) / colWidth));
  const row = Math.max(
    0,
    Math.floor((clientY - rect.top) / DASHBOARD_GRID_ROW_STRIDE_PX),
  );
  return { col, row };
}

export function defaultSizeForWidgetType(type: WidgetType): {
  colSpan: number;
  rowSpan: number;
} {
  const found = WIDGET_CATALOG.find((e) => e.type === type);
  return found
    ? { colSpan: found.defaultColSpan, rowSpan: found.defaultRowSpan }
    : { colSpan: 8, rowSpan: 4 };
}

export function clampGridRect(
  col: number,
  row: number,
  colSpan: number,
  rowSpan: number,
  gridCols = DASHBOARD_GRID_COLS,
): { col: number; row: number; colSpan: number; rowSpan: number } {
  const safeColSpan = Math.max(4, Math.min(gridCols, Math.round(colSpan)));
  const safeRowSpan = Math.max(2, Math.min(24, Math.round(rowSpan)));
  const safeCol = Math.max(0, Math.min(gridCols - safeColSpan, Math.round(col)));
  const safeRow = Math.max(0, Math.round(row));
  return {
    col: safeCol,
    row: safeRow,
    colSpan: safeColSpan,
    rowSpan: safeRowSpan,
  };
}

export interface GridBox {
  col: number;
  row: number;
  colSpan: number;
  rowSpan: number;
}

export function widgetsOverlap(a: GridBox, b: GridBox): boolean {
  return (
    a.col < b.col + b.colSpan &&
    a.col + a.colSpan > b.col &&
    a.row < b.row + b.rowSpan &&
    a.row + a.rowSpan > b.row
  );
}

export function findFreeGridSlot(
  existing: ReadonlyArray<GridBox>,
  colSpan: number,
  rowSpan: number,
  preferred?: { col: number; row: number },
  gridCols = DASHBOARD_GRID_COLS,
): { col: number; row: number } {
  if (preferred) {
    const clamped = clampGridRect(preferred.col, preferred.row, colSpan, rowSpan, gridCols);
    return { col: clamped.col, row: clamped.row };
  }

  let maxRow = 0;
  for (let i = 0; i < existing.length; i += 1) {
    maxRow = Math.max(maxRow, existing[i].row + existing[i].rowSpan);
  }

  const safeW = Math.max(4, Math.min(gridCols, Math.round(colSpan)));
  const safeH = Math.max(2, Math.round(rowSpan));

  for (let r = 0; r <= maxRow + safeH; r += 1) {
    for (let c = 0; c <= gridCols - safeW; c += 2) {
      const candidate: GridBox = { col: c, row: r, colSpan: safeW, rowSpan: safeH };
      if (!existing.some((item) => widgetsOverlap(candidate, item))) {
        return { col: c, row: r };
      }
    }
  }

  return { col: 0, row: maxRow };
}
