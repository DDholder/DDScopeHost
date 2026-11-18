import {
  useEffect,
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
import {
  DDSCOPE_POINTER_DROP_EVENT,
  parseCommandDragTransfer,
  type PointerDropEventDetail,
} from "../workspace/pointer-dnd.ts";
import type { CommandDefinition } from "../workspace/command-system.ts";
import { CmdButtonWidget } from "./CmdButtonWidget.tsx";
import { CmdTableWidget } from "./CmdTableWidget.tsx";
import { DigitalMeterWidget } from "./DigitalMeterWidget.tsx";
import { ParamSliderWidget } from "./ParamSliderWidget.tsx";
import { RadialGaugeWidget } from "./RadialGaugeWidget.tsx";
import { StatusLedWidget } from "./StatusLedWidget.tsx";
import { WatchTableWidget } from "./WatchTableWidget.tsx";
import {
  DASHBOARD_GRID_COLS,
  DASHBOARD_GRID_ROW_STRIDE_PX,
  WIDGET_TYPES,
  clampGridRect,
  computeCanvasGridCell,
  type DashboardCanvasConfig,
  type DashboardWidgetItem,
  type WidgetType,
} from "./types.ts";

export interface DashboardCanvasProps {
  canvas: DashboardCanvasConfig;
  channelsById: Record<number, ChannelNodeDto_Serialize>;
  sourceIds: string[];
  commands: CommandDefinition[];
  scopes: Record<number, ScopeUiConfig>;
  focusedScopeId: number | null;
  runBoundariesNs?: ReadonlyArray<number>;
  scopeCallbacks: ScopeCallbacks;
  locale: UiLocale;
  onFocusScope: (scopeId: number) => void;
  onCanvasWidthChanged: (scopeId: number, widthPx: number) => void;
  onToggleLock: (canvasId: number) => void;
  onClearCanvas: (canvasId: number) => void;
  onRenameCanvas?: (canvasId: number, title: string) => void;
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
  onBindCommandToWidget?: (
    canvasId: number,
    widgetId: string,
    commandId: string | null,
    argIndex?: number,
  ) => void;
  onSetCommandArgValue?: (
    commandId: string,
    argIndex: number,
    value: number,
    sourceWidgetId?: string,
  ) => void;
  onPlaceCommandOnCanvas?: (
    canvasId: number,
    commandId: string,
    opts?: { col?: number; row?: number; argIndex?: number },
  ) => string;
  onSendDownlink: (
    req: SendDownlinkRequestDto_Deserialize,
    labelSummary?: string,
    isSliderEvent?: boolean,
  ) => Promise<DownlinkReceiptDto>;
}

const GRID_ROW_STRIDE_PX = DASHBOARD_GRID_ROW_STRIDE_PX;

function isCommandBindableWidget(type: WidgetType): boolean {
  return type === "param_slider" || type === "cmd_button" || type === "cmd_table";
}

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
  const plain = ev.dataTransfer.getData("text/plain").trim();
  if (plain.startsWith("channel:")) {
    const num = Number(plain.slice("channel:".length));
    if (Number.isFinite(num)) {
      return num;
    }
  } else if (/^\d+$/.test(plain)) {
    return Number(plain);
  }
  return null;
}

export function DashboardCanvas({
  canvas,
  channelsById,
  sourceIds,
  commands,
  scopes,
  focusedScopeId,
  runBoundariesNs,
  scopeCallbacks,
  locale,
  onFocusScope,
  onCanvasWidthChanged,
  onToggleLock,
  onClearCanvas,
  onRenameCanvas,
  onAddWidget,
  onUpdateWidget,
  onRemoveWidget,
  onBindChannelToWidget,
  onBindCommandToWidget,
  onSetCommandArgValue,
  onPlaceCommandOnCanvas,
  onSendDownlink,
}: DashboardCanvasProps) {
  const t = (key: Parameters<typeof translate>[1], params?: Record<string, string | number>) =>
    translate(locale, key, params);

  const gridRef = useRef<HTMLDivElement | null>(null);
  const [gridDragOver, setGridDragOver] = useState(false);
  const [cardDragOverId, setCardDragOverId] = useState<string | null>(null);
  const [renamingCanvas, setRenamingCanvas] = useState(false);
  const [canvasTitleDraft, setCanvasTitleDraft] = useState("");
  const [renamingWidgetId, setRenamingWidgetId] = useState<string | null>(null);
  const [widgetTitleDraft, setWidgetTitleDraft] = useState("");
  const skipBlurCommitRef = useRef(false);

  const startCanvasRename = () => {
    if (!onRenameCanvas) return;
    skipBlurCommitRef.current = false;
    setCanvasTitleDraft(canvas.title);
    setRenamingCanvas(true);
  };

  const commitCanvasRename = () => {
    if (skipBlurCommitRef.current) {
      skipBlurCommitRef.current = false;
      return;
    }
    setRenamingCanvas(false);
    const trimmed = canvasTitleDraft.trim();
    if (trimmed && trimmed !== canvas.title) {
      onRenameCanvas?.(canvas.canvasId, trimmed);
    }
  };

  const startWidgetRename = (widget: DashboardWidgetItem) => {
    if (canvas.locked) return;
    skipBlurCommitRef.current = false;
    setWidgetTitleDraft(widget.title);
    setRenamingWidgetId(widget.id);
  };

  const commitWidgetRename = (widget: DashboardWidgetItem) => {
    if (skipBlurCommitRef.current) {
      skipBlurCommitRef.current = false;
      return;
    }
    setRenamingWidgetId(null);
    const trimmed = widgetTitleDraft.trim();
    if (!trimmed || trimmed === widget.title) return;
    onUpdateWidget(canvas.canvasId, widget.id, (prev) => ({
      ...prev,
      title: trimmed,
    }));
    if (widget.type === "mso_scope" && widget.scopeId !== null) {
      scopeCallbacks.onUpdateScope(widget.scopeId, (prev) => ({
        ...prev,
        title: trimmed,
      }));
    }
  };

  const computeDropCell = (clientX: number, clientY: number): { col: number; row: number } => {
    const el = gridRef.current;
    if (!el) return { col: 0, row: 0 };
    return computeCanvasGridCell(clientX, clientY, el.getBoundingClientRect());
  };

  const dropChannelAtCell = (chId: number, clientX: number, clientY: number) => {
    const cell = computeDropCell(clientX, clientY);
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
  };

  const dropChannelOnWidget = (widget: DashboardWidgetItem, chId: number) => {
    if (widget.type === "mso_scope" && widget.scopeId !== null) {
      scopeCallbacks.onDropChannel(widget.scopeId, chId);
    } else {
      onBindChannelToWidget(canvas.canvasId, widget.id, chId);
    }
  };

  const dropHandlersRef = useRef({
    canvasId: canvas.canvasId,
    widgets: canvas.widgets,
    onAddWidget,
    onBindCommandToWidget,
    onPlaceCommandOnCanvas,
    dropChannelAtCell,
    dropChannelOnWidget,
  });
  dropHandlersRef.current = {
    canvasId: canvas.canvasId,
    widgets: canvas.widgets,
    onAddWidget,
    onBindCommandToWidget,
    onPlaceCommandOnCanvas,
    dropChannelAtCell,
    dropChannelOnWidget,
  };

  useEffect(() => {
    const gridEl = gridRef.current;
    if (!gridEl) return;

    const onPointerDrop = (ev: Event) => {
      const detail = (ev as CustomEvent<PointerDropEventDetail>).detail;
      if (!detail) return;
      ev.stopPropagation();
      setGridDragOver(false);
      setCardDragOverId(null);

      const latest = dropHandlersRef.current;
      if (detail.dropKind === "widget-card" && detail.payload.kind === "channel") {
        const targetWidget = latest.widgets.find((w) => w.id === detail.widgetId);
        if (targetWidget) {
          latest.dropChannelOnWidget(targetWidget, detail.payload.channelId);
        }
        return;
      }

      if (detail.dropKind === "widget-card" && detail.payload.kind === "command") {
        const targetWidget = latest.widgets.find((w) => w.id === detail.widgetId);
        if (targetWidget) {
          latest.onBindCommandToWidget?.(
            latest.canvasId,
            targetWidget.id,
            detail.payload.commandId,
            detail.payload.argIndex,
          );
        }
        return;
      }

      if (detail.dropKind === "canvas-grid") {
        if (detail.payload.kind === "widget") {
          const cell = computeCanvasGridCell(
            detail.clientX,
            detail.clientY,
            gridEl.getBoundingClientRect(),
          );
          latest.onAddWidget(latest.canvasId, detail.payload.widgetType, {
            col: cell.col,
            row: cell.row,
          });
          return;
        }
        if (detail.payload.kind === "command") {
          const cell = computeCanvasGridCell(
            detail.clientX,
            detail.clientY,
            gridEl.getBoundingClientRect(),
          );
          latest.onPlaceCommandOnCanvas?.(
            latest.canvasId,
            detail.payload.commandId,
            {
              col: cell.col,
              row: cell.row,
              ...(detail.payload.argIndex !== undefined
                ? { argIndex: detail.payload.argIndex }
                : {}),
            },
          );
          return;
        }
        if (detail.payload.kind === "channel") {
          latest.dropChannelAtCell(
            detail.payload.channelId,
            detail.clientX,
            detail.clientY,
          );
        }
      }
    };

    gridEl.addEventListener(DDSCOPE_POINTER_DROP_EVENT, onPointerDrop);
    return () => gridEl.removeEventListener(DDSCOPE_POINTER_DROP_EVENT, onPointerDrop);
  }, []);

  const handleGridDragOver = (ev: DragEvent<HTMLDivElement>) => {
    const types = Array.from(ev.dataTransfer.types);
    if (
      types.includes("application/x-ddscope-widget") ||
      types.includes("application/x-ddscope-channel") ||
      types.includes("application/x-ddscope-command") ||
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

    const cmdDrop = parseCommandDragTransfer(ev);
    if (cmdDrop) {
      ev.preventDefault();
      ev.stopPropagation();
      onPlaceCommandOnCanvas?.(canvas.canvasId, cmdDrop.commandId, {
        col: cell.col,
        row: cell.row,
        ...(cmdDrop.argIndex !== undefined ? { argIndex: cmdDrop.argIndex } : {}),
      });
      return;
    }

    const chId = parseDroppedChannelId(ev);
    if (chId !== null) {
      ev.preventDefault();
      ev.stopPropagation();
      dropChannelAtCell(chId, ev.clientX, ev.clientY);
    }
  };

  const handleCardDrop = (ev: DragEvent<HTMLDivElement>, widget: DashboardWidgetItem) => {
    // Allow toolbox widget drops to bubble to grid drop placement
    if (Array.from(ev.dataTransfer.types).includes("application/x-ddscope-widget")) {
      return;
    }
    const cmdDrop = parseCommandDragTransfer(ev);
    if (cmdDrop && isCommandBindableWidget(widget.type)) {
      ev.preventDefault();
      ev.stopPropagation();
      setCardDragOverId(null);
      setGridDragOver(false);
      onBindCommandToWidget?.(
        canvas.canvasId,
        widget.id,
        cmdDrop.commandId,
        cmdDrop.argIndex,
      );
      return;
    }

    const chId = parseDroppedChannelId(ev);
    if (chId === null) return;

    ev.preventDefault();
    ev.stopPropagation();
    setCardDragOverId(null);
    setGridDragOver(false);

    dropChannelOnWidget(widget, chId);
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

    const bindChannel = (channelId: number) =>
      onBindChannelToWidget(canvas.canvasId, widget.id, channelId);

    switch (widget.type) {
      case "digital_meter":
        return (
          <DigitalMeterWidget
            widget={widget}
            channelsById={channelsById}
            locale={locale}
            onUpdateWidget={handleUpdate}
            onBindChannel={bindChannel}
          />
        );
      case "radial_gauge":
        return (
          <RadialGaugeWidget
            widget={widget}
            channelsById={channelsById}
            locale={locale}
            onUpdateWidget={handleUpdate}
            onBindChannel={bindChannel}
          />
        );
      case "status_led":
        return (
          <StatusLedWidget
            widget={widget}
            channelsById={channelsById}
            locale={locale}
            onBindChannel={bindChannel}
          />
        );
      case "watch_table":
        return (
          <WatchTableWidget
            widget={widget}
            channelsById={channelsById}
            locale={locale}
            onUpdateWidget={handleUpdate}
            onBindChannel={bindChannel}
          />
        );
      case "param_slider":
        return (
          <ParamSliderWidget
            widget={widget}
            sourceIds={sourceIds}
            commands={commands}
            locale={locale}
            onUpdateWidget={handleUpdate}
            onBindCommand={(cmdId, argIndex) =>
              onBindCommandToWidget?.(canvas.canvasId, widget.id, cmdId, argIndex)
            }
            onSetCommandArgValue={onSetCommandArgValue}
            onSendDownlink={onSendDownlink}
          />
        );
      case "cmd_button":
        return (
          <CmdButtonWidget
            widget={widget}
            sourceIds={sourceIds}
            commands={commands}
            locale={locale}
            onUpdateWidget={handleUpdate}
            onBindCommand={(cmdId, argIndex) =>
              onBindCommandToWidget?.(canvas.canvasId, widget.id, cmdId, argIndex)
            }
            onSetCommandArgValue={onSetCommandArgValue}
            onSendDownlink={onSendDownlink}
          />
        );
      case "cmd_table":
        return (
          <CmdTableWidget
            widget={widget}
            sourceIds={sourceIds}
            commands={commands}
            locale={locale}
            onUpdateWidget={handleUpdate}
            onBindCommand={(cmdId) =>
              onBindCommandToWidget?.(canvas.canvasId, widget.id, cmdId)
            }
            onSetCommandArgValue={onSetCommandArgValue}
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
              runBoundariesNs={runBoundariesNs}
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
        {renamingCanvas ? (
          <input
            type="text"
            className="inline-rename-input"
            data-testid={`rename-canvas-input-${canvas.canvasId}`}
            value={canvasTitleDraft}
            autoFocus
            onFocus={(ev) => ev.currentTarget.select()}
            onChange={(ev) => setCanvasTitleDraft(ev.target.value)}
            onBlur={commitCanvasRename}
            onPointerDown={(ev) => ev.stopPropagation()}
            onMouseDown={(ev) => ev.stopPropagation()}
            onClick={(ev) => ev.stopPropagation()}
            onKeyDown={(ev) => {
              ev.stopPropagation();
              if (ev.key === "Enter") {
                ev.preventDefault();
                commitCanvasRename();
              } else if (ev.key === "Escape") {
                ev.preventDefault();
                skipBlurCommitRef.current = true;
                setRenamingCanvas(false);
              }
            }}
          />
        ) : (
          <div className="dashboard-canvas-title-wrap">
            <span
              className="dashboard-canvas-title"
              title={onRenameCanvas ? t("canvas.renameTitle") : undefined}
              onDoubleClick={startCanvasRename}
            >
              {canvas.title}
            </span>
            {onRenameCanvas && (
              <button
                type="button"
                className="inline-rename-btn"
                title={t("canvas.renameTitle")}
                data-testid={`rename-canvas-btn-${canvas.canvasId}`}
                onClick={startCanvasRename}
              >
                ✎
              </button>
            )}
          </div>
        )}
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
        data-drop-canvas-grid="true"
        data-drop-canvas-id={canvas.canvasId}
        data-testid={`dashboard-grid-${canvas.canvasId}`}
        onDragOver={handleGridDragOver}
        onDragLeave={(ev) => {
          if (!ev.currentTarget.contains(ev.relatedTarget as Node | null)) {
            setGridDragOver(false);
          }
        }}
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
                data-drop-widget-id={widget.id}
                {...(isCommandBindableWidget(widget.type)
                  ? { "data-drop-command-widget-id": widget.id }
                  : {})}
                data-drop-canvas-id={canvas.canvasId}
                data-testid={`widget-card-${widget.id}`}
                style={{
                  gridColumn: `${widget.col + 1} / span ${widget.colSpan}`,
                  gridRow: `${widget.row + 1} / span ${widget.rowSpan}`,
                }}
                onDragOver={(ev) => {
                  const types = Array.from(ev.dataTransfer.types);
                  const isWidgetDrag = types.includes("application/x-ddscope-widget");
                  const isCmdWidget = isCommandBindableWidget(widget.type);
                  if (
                    !isWidgetDrag &&
                    (types.includes("application/x-ddscope-channel") ||
                      (isCmdWidget && types.includes("application/x-ddscope-command")) ||
                      types.includes("text/plain"))
                  ) {
                    ev.preventDefault();
                    ev.stopPropagation();
                    ev.dataTransfer.dropEffect = "copy";
                    if (cardDragOverId !== widget.id) {
                      setCardDragOverId(widget.id);
                    }
                  }
                }}
                onDragLeave={(ev) => {
                  if (
                    !ev.currentTarget.contains(ev.relatedTarget as Node | null) &&
                    cardDragOverId === widget.id
                  ) {
                    setCardDragOverId(null);
                  }
                }}
                onDrop={(ev) => handleCardDrop(ev, widget)}
              >
                <div
                  className="widget-card-header"
                  onPointerDown={(ev) => startMoveDrag(ev, widget)}
                >
                  {renamingWidgetId === widget.id ? (
                    <input
                      type="text"
                      className="inline-rename-input"
                      data-testid={`rename-widget-input-${widget.id}`}
                      value={widgetTitleDraft}
                      autoFocus
                      onFocus={(ev) => ev.currentTarget.select()}
                      onChange={(ev) => setWidgetTitleDraft(ev.target.value)}
                      onBlur={() => commitWidgetRename(widget)}
                      onPointerDown={(ev) => ev.stopPropagation()}
                      onMouseDown={(ev) => ev.stopPropagation()}
                      onClick={(ev) => ev.stopPropagation()}
                      onKeyDown={(ev) => {
                        ev.stopPropagation();
                        if (ev.key === "Enter") {
                          ev.preventDefault();
                          commitWidgetRename(widget);
                        } else if (ev.key === "Escape") {
                          ev.preventDefault();
                          skipBlurCommitRef.current = true;
                          setRenamingWidgetId(null);
                        }
                      }}
                    />
                  ) : (
                    <div className="widget-card-title-wrap">
                      <span
                        className="widget-card-title"
                        title={!canvas.locked ? t("widget.renameTitle") : widget.title}
                        onDoubleClick={(ev) => {
                          ev.stopPropagation();
                          startWidgetRename(widget);
                        }}
                      >
                        {widget.title}
                      </span>
                      {!canvas.locked && (
                        <button
                          type="button"
                          className="inline-rename-btn"
                          title={t("widget.renameTitle")}
                          data-testid={`rename-widget-btn-${widget.id}`}
                          onClick={(ev) => {
                            ev.stopPropagation();
                            startWidgetRename(widget);
                          }}
                        >
                          ✎
                        </button>
                      )}
                    </div>
                  )}
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
