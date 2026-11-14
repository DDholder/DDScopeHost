import {
  useRef,
  useState,
  type DragEvent,
  type PointerEvent as ReactPointerEvent,
} from "react";
import type {
  ChannelNodeDto_Serialize,
  DownlinkReceiptDto,
  SendDownlinkRequestDto_Deserialize,
} from "../bridge/bindings.ts";
import type { UiLocale } from "../i18n/index.ts";
import { translate } from "../i18n/index.ts";
import { ScopeViewportWidget } from "../scope-engine/ScopeViewportWidget.tsx";
import type { ScopeCallbacks, ScopeUiConfig } from "../scope-engine/types.ts";
import { CmdButtonWidget } from "./CmdButtonWidget.tsx";
import { DigitalMeterWidget } from "./DigitalMeterWidget.tsx";
import { ParamSliderWidget } from "./ParamSliderWidget.tsx";
import { RadialGaugeWidget } from "./RadialGaugeWidget.tsx";
import { StatusLedWidget } from "./StatusLedWidget.tsx";
import { WatchTableWidget } from "./WatchTableWidget.tsx";
import {
  DASHBOARD_GRID_COLS,
  WIDGET_TYPES,
  clampGridRect,
  type DashboardCanvasConfig,
  type DashboardWidgetItem,
  type WidgetType,
} from "./types.ts";

export interface DashboardCanvasProps {
  canvas: DashboardCanvasConfig;
  channelsById: Record<number, ChannelNodeDto_Serialize>;
  sourceIds: string[];
  scopes: Record<number, ScopeUiConfig>;
  focusedScopeId: number | null;
  scopeCallbacks: ScopeCallbacks;
  locale: UiLocale;
  onFocusScope: (scopeId: number) => void;
  onCanvasWidthChanged: (scopeId: number, widthPx: number) => void;
  onToggleLock: (canvasId: number) => void;
  onClearCanvas: (canvasId: number) => void;
  onAddWidget: (
    canvasId: number,
    widgetType: WidgetType,
    opts?: { col?: number; row?: number; channelId?: number },
  ) => string;
  onUpdateWidget: (
    canvasId: number,
    widgetId: string,
    updater: (prev: DashboardWidgetItem) => DashboardWidgetItem,
  ) => void;
  onRemoveWidget: (canvasId: number, widgetId: string) => void;
  onBindChannelToWidget: (canvasId: number, widgetId: string, channelId: number) => void;
  onSendDownlink: (
    req: SendDownlinkRequestDto_Deserialize,
    labelSummary?: string,
    isSliderEvent?: boolean,
  ) => Promise<DownlinkReceiptDto>;
}

const GRID_ROW_HEIGHT_PX = 42;
const GRID_GAP_PX = 8;
const GRID_ROW_STRIDE_PX = GRID_ROW_HEIGHT_PX + GRID_GAP_PX;

function parseDroppedWidgetType(ev: DragEvent): WidgetType | null {
  const rawJson = ev.dataTransfer.getData("application/x-ddscope-widget");
  if (rawJson) {
    try {
      const parsed = JSON.parse(rawJson) as { widgetType?: string };
      if (parsed.widgetType && (WIDGET_TYPES as readonly string[]).includes(parsed.widgetType)) {
        return parsed.widgetType as WidgetType;
      }
    } catch {
      // Ignore malformed widget payload
    }
  }
  const plain = ev.dataTransfer.getData("text/plain");
  if (plain.startsWith("widget:")) {
    const candidate = plain.slice("widget:".length);
    if ((WIDGET_TYPES as readonly string[]).includes(candidate)) {
      return candidate as WidgetType;
    }
  }
  return null;
}

function parseDroppedChannelId(ev: DragEvent): number | null {
  const rawJson = ev.dataTransfer.getData("application/x-ddscope-channel");
  if (rawJson) {
    try {
      const parsed = JSON.parse(rawJson) as { channelId?: number };
      if (typeof parsed.channelId === "number" && Number.isFinite(parsed.channelId)) {
        return parsed.channelId;
      }
    } catch {
      // Ignore malformed JSON
    }
  }
  const plain = ev.dataTransfer.getData("text/plain");
  if (/^\d+$/.test(plain.trim())) {
    return Number(plain.trim());
  }
  return null;
}

export function DashboardCanvas({
  canvas,
  channelsById,
  sourceIds,
  scopes,
  focusedScopeId,
  scopeCallbacks,
  locale,
  onFocusScope,
  onCanvasWidthChanged,
  onToggleLock,
  onClearCanvas,
  onAddWidget,
  onUpdateWidget,
  onRemoveWidget,
  onBindChannelToWidget,
  onSendDownlink,
}: DashboardCanvasProps) {
  const t = (key: Parameters<typeof translate>[1], params?: Record<string, string | number>) =>
    translate(locale, key, params);

  const gridRef = useRef<HTMLDivElement | null>(null);
  const [gridDragOver, setGridDragOver] = useState(false);
  const [cardDragOverId, setCardDragOverId] = useState<string | null>(null);

  const computeDropCell = (clientX: number, clientY: number): { col: number; row: number } => {
    const el = gridRef.current;
    if (!el) return { col: 0, row: 0 };
    const rect = el.getBoundingClientRect();
    const colWidth = Math.max(12, rect.width / DASHBOARD_GRID_COLS);
    const col = Math.max(0, Math.floor((clientX - rect.left) / colWidth));
    const row = Math.max(0, Math.floor((clientY - rect.top) / GRID_ROW_STRIDE_PX));
    return { col, row };
  };

  const handleGridDragOver = (ev: DragEvent<HTMLDivElement>) => {
    const types = Array.from(ev.dataTransfer.types);
    if (
      types.includes("application/x-ddscope-widget") ||
      types.includes("application/x-ddscope-channel") ||
      types.includes("text/plain")
    ) {
      ev.preventDefault();
      ev.dataTransfer.dropEffect = "copy";
      if (!gridDragOver) setGridDragOver(true);
    }
  };

  const handleGridDrop = (ev: DragEvent<HTMLDivElement>) => {
    setGridDragOver(false);
    setCardDragOverId(null);
    const widgetType = parseDroppedWidgetType(ev);
    const cell = computeDropCell(ev.clientX, ev.clientY);

    if (widgetType) {
      ev.preventDefault();
      ev.stopPropagation();
      onAddWidget(canvas.canvasId, widgetType, { col: cell.col, row: cell.row });
      return;
    }

    const chId = parseDroppedChannelId(ev);
    if (chId !== null) {
      ev.preventDefault();
      ev.stopPropagation();
      const ch = channelsById[chId];
      const autoType: WidgetType =
        ch && (ch.modality === "digital_bool" || ch.modality === "state_machine")
          ? "status_led"
          : "digital_meter";
      onAddWidget(canvas.canvasId, autoType, {
        col: cell.col,
        row: cell.row,
        channelId: chId,
      });
    }
  };

  const handleCardDrop = (ev: DragEvent<HTMLDivElement>, widget: DashboardWidgetItem) => {
    // Allow toolbox widget drops to bubble to grid drop placement
    if (Array.from(ev.dataTransfer.types).includes("application/x-ddscope-widget")) {
      return;
    }
    const chId = parseDroppedChannelId(ev);
    if (chId === null) return;

    ev.preventDefault();
    ev.stopPropagation();
    setCardDragOverId(null);
    setGridDragOver(false);

    if (widget.type === "mso_scope" && widget.scopeId !== null) {
      scopeCallbacks.onDropChannel(widget.scopeId, chId);
    } else {
      onBindChannelToWidget(canvas.canvasId, widget.id, chId);
    }
  };

  const startMoveDrag = (
    ev: ReactPointerEvent<HTMLDivElement>,
    widget: DashboardWidgetItem,
  ) => {
    if (canvas.locked) return;
    if ((ev.target as HTMLElement).closest("button, input, select")) return;
    const gridEl = gridRef.current;
    if (!gridEl) return;

    ev.preventDefault();
    const startX = ev.clientX;
    const startY = ev.clientY;
    const origCol = widget.col;
    const origRow = widget.row;
    const colWidth = Math.max(12, gridEl.getBoundingClientRect().width / DASHBOARD_GRID_COLS);

    const onMove = (moveEv: PointerEvent) => {
      const dCols = Math.round((moveEv.clientX - startX) / colWidth);
      const dRows = Math.round((moveEv.clientY - startY) / GRID_ROW_STRIDE_PX);
      const next = clampGridRect(origCol + dCols, origRow + dRows, widget.colSpan, widget.rowSpan);
      onUpdateWidget(canvas.canvasId, widget.id, (prev) =>
        prev.col === next.col && prev.row === next.row
          ? prev
          : { ...prev, col: next.col, row: next.row },
      );
    };

    const onUp = () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
    };

    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp, { once: true });
  };

  const startResizeDrag = (
    ev: ReactPointerEvent<HTMLDivElement>,
    widget: DashboardWidgetItem,
  ) => {
    if (canvas.locked) return;
    const gridEl = gridRef.current;
    if (!gridEl) return;

    ev.preventDefault();
    ev.stopPropagation();
    const startX = ev.clientX;
    const startY = ev.clientY;
    const origW = widget.colSpan;
    const origH = widget.rowSpan;
    const colWidth = Math.max(12, gridEl.getBoundingClientRect().width / DASHBOARD_GRID_COLS);

    const onMove = (moveEv: PointerEvent) => {
      const dCols = Math.round((moveEv.clientX - startX) / colWidth);
      const dRows = Math.round((moveEv.clientY - startY) / GRID_ROW_STRIDE_PX);
      const next = clampGridRect(widget.col, widget.row, origW + dCols, origH + dRows);
      onUpdateWidget(canvas.canvasId, widget.id, (prev) =>
        prev.colSpan === next.colSpan && prev.rowSpan === next.rowSpan
          ? prev
          : { ...prev, colSpan: next.colSpan, rowSpan: next.rowSpan },
      );
    };

    const onUp = () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
    };

    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp, { once: true });
  };

  const renderWidgetBody = (widget: DashboardWidgetItem) => {
    const handleUpdate = (updater: (prev: DashboardWidgetItem) => DashboardWidgetItem) =>
      onUpdateWidget(canvas.canvasId, widget.id, updater);

    switch (widget.type) {
      case "digital_meter":
        return (
          <DigitalMeterWidget
            widget={widget}
            channelsById={channelsById}
            locale={locale}
            onUpdateWidget={handleUpdate}
          />
        );
      case "radial_gauge":
        return (
          <RadialGaugeWidget
            widget={widget}
            channelsById={channelsById}
            locale={locale}
            onUpdateWidget={handleUpdate}
          />
        );
      case "status_led":
        return (
          <StatusLedWidget
            widget={widget}
            channelsById={channelsById}
            locale={locale}
          />
        );
      case "watch_table":
        return (
          <WatchTableWidget
            widget={widget}
            channelsById={channelsById}
            locale={locale}
            onUpdateWidget={handleUpdate}
          />
        );
      case "param_slider":
        return (
          <ParamSliderWidget
            widget={widget}
            sourceIds={sourceIds}
            locale={locale}
            onUpdateWidget={handleUpdate}
            onSendDownlink={onSendDownlink}
          />
        );
      case "cmd_button":
        return (
          <CmdButtonWidget
            widget={widget}
            sourceIds={sourceIds}
            locale={locale}
            onUpdateWidget={handleUpdate}
            onSendDownlink={onSendDownlink}
          />
        );
      case "mso_scope": {
        const scopeCfg = widget.scopeId !== null ? scopes[widget.scopeId] : undefined;
        if (!scopeCfg) {
          return (
            <div className="widget-unbound-dropzone">
              {t("stage.unavailable", { id: widget.scopeId ?? "?" })}
            </div>
          );
        }
        return (
          <div className="widget-embedded-scope">
            <ScopeViewportWidget
              config={scopeCfg}
              channelsById={channelsById}
              callbacks={scopeCallbacks}
              locale={locale}
              isFocused={focusedScopeId === scopeCfg.scopeId}
              onFocusScope={onFocusScope}
              onCanvasWidthChanged={onCanvasWidthChanged}
            />
          </div>
        );
      }
      default:
        return null;
    }
  };

  return (
    <div
      className={`dashboard-canvas-root ${canvas.locked ? "is-locked" : "is-editing"}`}
      data-testid={`dashboard-canvas-${canvas.canvasId}`}
    >
      <div className="dashboard-canvas-toolbar">
        <span className="dashboard-canvas-title">{canvas.title}</span>
        <span className="dashboard-canvas-hint">{t("canvas.dropHint")}</span>
        <div className="dashboard-canvas-actions">
          <button
            type="button"
            className={`scope-btn ${!canvas.locked ? "scope-btn-accent" : ""}`}
            data-testid={`canvas-lock-toggle-${canvas.canvasId}`}
            onClick={() => onToggleLock(canvas.canvasId)}
          >
            {canvas.locked ? t("canvas.lockedMode") : t("canvas.designMode")}
          </button>
          <button
            type="button"
            className="scope-btn scope-btn-danger"
            onClick={() => onClearCanvas(canvas.canvasId)}
          >
            {t("canvas.clearAll")}
          </button>
        </div>
      </div>

      <div
        ref={gridRef}
        className={`dashboard-grid ${gridDragOver ? "is-drag-over" : ""}`}
        data-testid={`dashboard-grid-${canvas.canvasId}`}
        onDragOver={handleGridDragOver}
        onDragLeave={() => setGridDragOver(false)}
        onDrop={handleGridDrop}
      >
        {canvas.widgets.length === 0 ? (
          <div className="dashboard-empty-state">
            <h4>{t("canvas.emptyTitle")}</h4>
            <p>{t("canvas.emptyDesc")}</p>
          </div>
        ) : (
          canvas.widgets.map((widget) => {
            const isCardDropTarget = cardDragOverId === widget.id;
            return (
              <div
                key={widget.id}
                className={`widget-card ${isCardDropTarget ? "is-channel-drop-over" : ""} ${
                  !canvas.locked ? "is-editable" : ""
                }`}
                data-testid={`widget-card-${widget.id}`}
                style={{
                  gridColumn: `${widget.col + 1} / span ${widget.colSpan}`,
                  gridRow: `${widget.row + 1} / span ${widget.rowSpan}`,
                }}
                onDragOver={(ev) => {
                  const types = Array.from(ev.dataTransfer.types);
                  if (types.includes("application/x-ddscope-channel")) {
                    ev.preventDefault();
                    ev.stopPropagation();
                    setCardDragOverId(widget.id);
                  }
                }}
                onDragLeave={() => {
                  if (cardDragOverId === widget.id) setCardDragOverId(null);
                }}
                onDrop={(ev) => handleCardDrop(ev, widget)}
              >
                <div
                  className="widget-card-header"
                  onPointerDown={(ev) => startMoveDrag(ev, widget)}
                >
                  <span className="widget-card-title">{widget.title}</span>
                  {!canvas.locked && (
                    <button
                      type="button"
                      className="widget-remove-btn"
                      title={t("widget.remove")}
                      data-testid={`remove-widget-${widget.id}`}
                      onClick={() => onRemoveWidget(canvas.canvasId, widget.id)}
                    >
                      ✕
                    </button>
                  )}
                </div>

                <div className="widget-card-content">{renderWidgetBody(widget)}</div>

                {!canvas.locked && (
                  <div
                    className="widget-resize-handle"
                    data-testid={`resize-widget-${widget.id}`}
                    onPointerDown={(ev) => startResizeDrag(ev, widget)}
                  />
                )}
              </div>
            );
          })
        )}
      </div>
    </div>
  );
}
