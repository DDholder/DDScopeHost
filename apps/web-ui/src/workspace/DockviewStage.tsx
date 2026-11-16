import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ChangeEvent,
  type DragEvent,
} from "react";
import {
  DockviewReact,
  themeAbyss,
  type DockviewApi,
  type DockviewReadyEvent,
  type IDockviewPanelProps,
  type IWatermarkPanelProps,
  type SerializedDockview,
} from "dockview";
import { ScopeViewportWidget, type ScopeCallbacks } from "../scope-engine/index.ts";
import { DashboardCanvas, WIDGET_TYPES, type WidgetType } from "../widgets/index.ts";
import { useWorkspaceStore } from "./store.ts";
import {
  DDSCOPE_POINTER_DROP_EVENT,
  type PointerDropEventDetail,
} from "./pointer-dnd.ts";
import { readJsonFileFromInput, saveJsonFile } from "./file-dialog.ts";
import { useI18n } from "./use-i18n.ts";

interface ScopePanelParams {
  scopeId: number;
}

interface CanvasPanelParams {
  canvasId: number;
}

function MsoScopePanel(props: IDockviewPanelProps<ScopePanelParams>) {
  const { locale, t } = useI18n();
  const scopeId = props.params.scopeId;
  const config = useWorkspaceStore((s) => s.scopes[scopeId]);
  const channelsById = useWorkspaceStore((s) => s.channelsById);
  const runBoundariesNs = useWorkspaceStore((s) => s.runBoundariesNs);
  const focusedScopeId = useWorkspaceStore((s) => s.focusedScopeId);
  const setFocusedScopeId = useWorkspaceStore((s) => s.setFocusedScopeId);
  const updateScope = useWorkspaceStore((s) => s.updateScope);
  const rearmScope = useWorkspaceStore((s) => s.rearmScope);
  const broadcastSyncGroup = useWorkspaceStore((s) => s.broadcastSyncGroup);
  const bindChannelToScope = useWorkspaceStore((s) => s.bindChannelToScope);
  const setScopeCanvasWidth = useWorkspaceStore((s) => s.setScopeCanvasWidth);

  const callbacks = useMemo<ScopeCallbacks>(
    () => ({
      onUpdateScope: updateScope,
      onRearmScope: rearmScope,
      onSyncGroupBroadcast: broadcastSyncGroup,
      onDropChannel: bindChannelToScope,
    }),
    [updateScope, rearmScope, broadcastSyncGroup, bindChannelToScope],
  );

  useEffect(() => {
    if (config && props.api.title !== config.title) {
      props.api.setTitle(config.title);
    }
  }, [config, props.api]);

  if (!config) {
    return (
      <div className="scope-panel-empty">
        {t("stage.unavailable", { id: scopeId })}
      </div>
    );
  }

  return (
    <ScopeViewportWidget
      config={config}
      channelsById={channelsById}
      callbacks={callbacks}
      locale={locale}
      isFocused={focusedScopeId === scopeId}
      runBoundariesNs={runBoundariesNs}
      onFocusScope={setFocusedScopeId}
      onCanvasWidthChanged={setScopeCanvasWidth}
    />
  );
}

function DashboardCanvasPanel(props: IDockviewPanelProps<CanvasPanelParams>) {
  const { locale, t } = useI18n();
  const canvasId = props.params.canvasId;
  const canvas = useWorkspaceStore((s) => s.canvases[canvasId]);
  const channelsById = useWorkspaceStore((s) => s.channelsById);
  const channelGroups = useWorkspaceStore((s) => s.channelGroups);
  const commands = useWorkspaceStore((s) => s.commands);
  const scopes = useWorkspaceStore((s) => s.scopes);
  const runBoundariesNs = useWorkspaceStore((s) => s.runBoundariesNs);
  const focusedScopeId = useWorkspaceStore((s) => s.focusedScopeId);
  const setFocusedScopeId = useWorkspaceStore((s) => s.setFocusedScopeId);
  const updateScope = useWorkspaceStore((s) => s.updateScope);
  const rearmScope = useWorkspaceStore((s) => s.rearmScope);
  const broadcastSyncGroup = useWorkspaceStore((s) => s.broadcastSyncGroup);
  const bindChannelToScope = useWorkspaceStore((s) => s.bindChannelToScope);
  const setScopeCanvasWidth = useWorkspaceStore((s) => s.setScopeCanvasWidth);
  const toggleCanvasLock = useWorkspaceStore((s) => s.toggleCanvasLock);
  const clearCanvasWidgets = useWorkspaceStore((s) => s.clearCanvasWidgets);
  const addWidgetToCanvas = useWorkspaceStore((s) => s.addWidgetToCanvas);
  const updateCanvasWidget = useWorkspaceStore((s) => s.updateCanvasWidget);
  const removeCanvasWidget = useWorkspaceStore((s) => s.removeCanvasWidget);
  const bindChannelToWidget = useWorkspaceStore((s) => s.bindChannelToWidget);
  const bindCommandToWidget = useWorkspaceStore((s) => s.bindCommandToWidget);
  const placeCommandOnCanvas = useWorkspaceStore((s) => s.placeCommandOnCanvas);
  const sendDownlink = useWorkspaceStore((s) => s.sendDownlink);

  const scopeCallbacks = useMemo<ScopeCallbacks>(
    () => ({
      onUpdateScope: updateScope,
      onRearmScope: rearmScope,
      onSyncGroupBroadcast: broadcastSyncGroup,
      onDropChannel: bindChannelToScope,
    }),
    [updateScope, rearmScope, broadcastSyncGroup, bindChannelToScope],
  );

  const sourceIds = useMemo(
    () => channelGroups.map((g) => g.source.source_id),
    [channelGroups],
  );

  useEffect(() => {
    if (canvas && props.api.title !== canvas.title) {
      props.api.setTitle(canvas.title);
    }
  }, [canvas, props.api]);

  if (!canvas) {
    return (
      <div className="scope-panel-empty">
        {t("stage.canvasUnavailable", { id: canvasId })}
      </div>
    );
  }

  return (
    <DashboardCanvas
      canvas={canvas}
      channelsById={channelsById}
      sourceIds={sourceIds}
      commands={commands}
      scopes={scopes}
      focusedScopeId={focusedScopeId}
      runBoundariesNs={runBoundariesNs}
      scopeCallbacks={scopeCallbacks}
      locale={locale}
      onFocusScope={setFocusedScopeId}
      onCanvasWidthChanged={setScopeCanvasWidth}
      onToggleLock={toggleCanvasLock}
      onClearCanvas={clearCanvasWidgets}
      onAddWidget={addWidgetToCanvas}
      onUpdateWidget={updateCanvasWidget}
      onRemoveWidget={removeCanvasWidget}
      onBindChannelToWidget={bindChannelToWidget}
      onBindCommandToWidget={bindCommandToWidget}
      onPlaceCommandOnCanvas={placeCommandOnCanvas}
      onSendDownlink={sendDownlink}
    />
  );
}

function parseWatermarkWidgetType(ev: DragEvent): WidgetType | null {
  const raw = ev.dataTransfer.getData("application/x-ddscope-widget");
  if (raw) {
    try {
      const parsed = JSON.parse(raw) as { widgetType?: string };
      if (parsed.widgetType && (WIDGET_TYPES as readonly string[]).includes(parsed.widgetType)) {
        return parsed.widgetType as WidgetType;
      }
    } catch {
      // Ignore malformed payload
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

function parseWatermarkCommandId(ev: DragEvent): string | null {
  const raw = ev.dataTransfer.getData("application/x-ddscope-command");
  if (raw) {
    try {
      const parsed = JSON.parse(raw) as { commandId?: string };
      if (typeof parsed.commandId === "string" && parsed.commandId.trim().length > 0) {
        return parsed.commandId.trim();
      }
    } catch {
      // Ignore malformed payload
    }
  }
  const plain = ev.dataTransfer.getData("text/plain").trim();
  if (plain.startsWith("command:")) {
    const id = plain.slice("command:".length).trim();
    return id.length > 0 ? id : null;
  }
  return null;
}

function WorkspaceWatermark(_props: IWatermarkPanelProps) {
  const { t } = useI18n();
  const addScope = useWorkspaceStore((s) => s.addScope);
  const addCanvas = useWorkspaceStore((s) => s.addCanvas);
  const addWidgetToCanvas = useWorkspaceStore((s) => s.addWidgetToCanvas);
  const placeCommandOnCanvas = useWorkspaceStore((s) => s.placeCommandOnCanvas);
  const importBoardConfig = useWorkspaceStore((s) => s.importBoardConfig);
  const importInputRef = useRef<HTMLInputElement | null>(null);
  const [importError, setImportError] = useState<string | null>(null);
  const watermarkRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    const el = watermarkRef.current;
    if (!el) return;
    const onPointerDrop = (ev: Event) => {
      const detail = (ev as CustomEvent<PointerDropEventDetail>).detail;
      if (!detail) return;
      if (detail.payload.kind === "widget") {
        ev.stopPropagation();
        const cid = addCanvas();
        addWidgetToCanvas(cid, detail.payload.widgetType);
        return;
      }
      if (detail.payload.kind === "command") {
        ev.stopPropagation();
        const cid = addCanvas();
        placeCommandOnCanvas(cid, detail.payload.commandId);
      }
    };
    el.addEventListener(DDSCOPE_POINTER_DROP_EVENT, onPointerDrop);
    return () => el.removeEventListener(DDSCOPE_POINTER_DROP_EVENT, onPointerDrop);
  }, [addCanvas, addWidgetToCanvas, placeCommandOnCanvas]);

  return (
    <div
      ref={watermarkRef}
      className="dockview-watermark"
      data-drop-watermark="true"
      onDragOver={(ev) => {
        const types = Array.from(ev.dataTransfer.types);
        if (
          types.includes("application/x-ddscope-widget") ||
          types.includes("application/x-ddscope-command") ||
          types.includes("text/plain")
        ) {
          ev.preventDefault();
          ev.dataTransfer.dropEffect = "copy";
          ev.currentTarget.classList.add("is-drag-over");
        }
      }}
      onDragLeave={(ev) => {
        if (!ev.currentTarget.contains(ev.relatedTarget as Node | null)) {
          ev.currentTarget.classList.remove("is-drag-over");
        }
      }}
      onDrop={(ev) => {
        ev.currentTarget.classList.remove("is-drag-over");
        const wType = parseWatermarkWidgetType(ev);
        if (wType) {
          ev.preventDefault();
          const cid = addCanvas();
          addWidgetToCanvas(cid, wType);
          return;
        }
        const cmdId = parseWatermarkCommandId(ev);
        if (cmdId) {
          ev.preventDefault();
          const cid = addCanvas();
          placeCommandOnCanvas(cid, cmdId);
        }
      }}
    >
      <h3>{t("stage.emptyTitle")}</h3>
      <p>{t("stage.emptyDesc")}</p>
      <div className="stage-watermark-actions">
        <button
          type="button"
          className="scope-btn scope-btn-accent"
          onClick={() => addScope()}
        >
          {t("stage.addScope")}
        </button>
        <button
          type="button"
          className="scope-btn"
          data-testid="watermark-add-canvas-btn"
          onClick={() => addCanvas()}
        >
          {t("stage.addCanvas")}
        </button>
        <button
          type="button"
          className="scope-btn"
          data-testid="watermark-import-board-btn"
          title={t("stage.importBoardTitle")}
          onClick={() => importInputRef.current?.click()}
        >
          {t("stage.importBoard")}
        </button>
        <input
          ref={importInputRef}
          type="file"
          accept=".json,application/json"
          style={{ display: "none" }}
          data-testid="watermark-import-board-input"
          onChange={(ev) => {
            void (async () => {
              try {
                const text = await readJsonFileFromInput(ev.currentTarget);
                if (text === null) return;
                setImportError(null);
                await importBoardConfig(text);
              } catch (err: unknown) {
                setImportError(
                  t("stage.boardImportError", {
                    error: err instanceof Error ? err.message : String(err),
                  }),
                );
              }
            })();
          }}
        />
      </div>
      {importError && (
        <div
          className="config-io-notice err"
          data-testid="watermark-import-board-error"
        >
          <span>{importError}</span>
          <button
            type="button"
            className="scope-btn-xs"
            onClick={() => setImportError(null)}
          >
            ×
          </button>
        </div>
      )}
    </div>
  );
}

export function DockviewStage() {
  const { t } = useI18n();
  const scopeOrder = useWorkspaceStore((s) => s.scopeOrder);
  const scopes = useWorkspaceStore((s) => s.scopes);
  const canvasOrder = useWorkspaceStore((s) => s.canvasOrder);
  const canvases = useWorkspaceStore((s) => s.canvases);
  const layoutRestoreSeq = useWorkspaceStore((s) => s.layoutRestoreSeq);
  const addScope = useWorkspaceStore((s) => s.addScope);
  const removeScope = useWorkspaceStore((s) => s.removeScope);
  const addCanvas = useWorkspaceStore((s) => s.addCanvas);
  const removeCanvas = useWorkspaceStore((s) => s.removeCanvas);
  const setFocusedScopeId = useWorkspaceStore((s) => s.setFocusedScopeId);
  const setActiveCanvasId = useWorkspaceStore((s) => s.setActiveCanvasId);
  const registerDockviewLayoutAccessor = useWorkspaceStore(
    (s) => s.registerDockviewLayoutAccessor,
  );
  const exportBoardConfig = useWorkspaceStore((s) => s.exportBoardConfig);
  const importBoardConfig = useWorkspaceStore((s) => s.importBoardConfig);

  const apiRef = useRef<DockviewApi | null>(null);
  const isRestoringLayoutRef = useRef(false);
  const lastAppliedRestoreSeqRef = useRef(0);
  const boardImportInputRef = useRef<HTMLInputElement | null>(null);
  const [boardNotice, setBoardNotice] = useState<{
    kind: "ok" | "warn" | "err";
    text: string;
  } | null>(null);
  const scopesRef = useRef(scopes);
  scopesRef.current = scopes;
  const canvasesRef = useRef(canvases);
  canvasesRef.current = canvases;

  const components = useMemo(
    () => ({
      msoScope: MsoScopePanel,
      dashboardCanvas: DashboardCanvasPanel,
    }),
    [],
  );

  const syncPanelsToStore = useCallback(
    (api: DockviewApi, sOrder: number[], cOrder: number[]) => {
      let firstScopePanelId: string | undefined;
      for (let i = 0; i < sOrder.length; i += 1) {
        const scopeId = sOrder[i];
        const panelId = `scope-${scopeId}`;
        if (!api.getPanel(panelId)) {
          const cfg = scopesRef.current[scopeId];
          if (!cfg) continue;
          if (i === 0 || !firstScopePanelId || !api.getPanel(firstScopePanelId)) {
            api.addPanel({
              id: panelId,
              component: "msoScope",
              title: cfg.title,
              params: { scopeId },
            });
            firstScopePanelId = panelId;
          } else {
            api.addPanel({
              id: panelId,
              component: "msoScope",
              title: cfg.title,
              params: { scopeId },
              position: {
                referencePanel: firstScopePanelId,
                direction: i === 1 ? "below" : "within",
              },
            });
          }
        } else if (!firstScopePanelId) {
          firstScopePanelId = panelId;
        }
      }

      let firstCanvasPanelId: string | undefined;
      for (let i = 0; i < cOrder.length; i += 1) {
        const canvasId = cOrder[i];
        const panelId = `canvas-${canvasId}`;
        if (!api.getPanel(panelId)) {
          const cfg = canvasesRef.current[canvasId];
          if (!cfg) continue;
          if (firstCanvasPanelId && api.getPanel(firstCanvasPanelId)) {
            api.addPanel({
              id: panelId,
              component: "dashboardCanvas",
              title: cfg.title,
              params: { canvasId },
              position: {
                referencePanel: firstCanvasPanelId,
                direction: "within",
              },
            });
          } else if (firstScopePanelId && api.getPanel(firstScopePanelId)) {
            api.addPanel({
              id: panelId,
              component: "dashboardCanvas",
              title: cfg.title,
              params: { canvasId },
              position: {
                referencePanel: firstScopePanelId,
                direction: "right",
              },
            });
            firstCanvasPanelId = panelId;
          } else {
            api.addPanel({
              id: panelId,
              component: "dashboardCanvas",
              title: cfg.title,
              params: { canvasId },
            });
            firstCanvasPanelId = panelId;
          }
        } else if (!firstCanvasPanelId) {
          firstCanvasPanelId = panelId;
        }
      }
    },
    [],
  );

  const applyBoardLayoutRestore = useCallback(
    (api: DockviewApi) => {
      const st = useWorkspaceStore.getState();
      scopesRef.current = st.scopes;
      canvasesRef.current = st.canvases;
      isRestoringLayoutRef.current = true;
      try {
        if (st.scopeOrder.length === 0 && st.canvasOrder.length === 0) {
          api.clear();
        } else if (st.pendingDockviewLayout) {
          try {
            api.fromJSON(st.pendingDockviewLayout as unknown as SerializedDockview);
          } catch {
            api.clear();
          }
          syncPanelsToStore(api, st.scopeOrder, st.canvasOrder);
        } else {
          api.clear();
          syncPanelsToStore(api, st.scopeOrder, st.canvasOrder);
        }
      } finally {
        isRestoringLayoutRef.current = false;
      }
    },
    [syncPanelsToStore],
  );

  const handleReady = useCallback(
    (event: DockviewReadyEvent) => {
      const api = event.api;
      apiRef.current = api;
      registerDockviewLayoutAccessor(
        () => api.toJSON() as unknown as Record<string, unknown>,
      );

      api.onDidRemovePanel((panel) => {
        if (isRestoringLayoutRef.current) return;
        if (panel.id.startsWith("scope-")) {
          const idNum = Number(panel.id.slice("scope-".length));
          if (Number.isFinite(idNum) && useWorkspaceStore.getState().scopes[idNum]) {
            removeScope(idNum);
          }
        } else if (panel.id.startsWith("canvas-")) {
          const idNum = Number(panel.id.slice("canvas-".length));
          if (Number.isFinite(idNum) && useWorkspaceStore.getState().canvases[idNum]) {
            removeCanvas(idNum);
          }
        }
      });

      api.onDidActivePanelChange((panel) => {
        if (isRestoringLayoutRef.current) return;
        if (!panel) return;
        if (panel.id.startsWith("scope-")) {
          const idNum = Number(panel.id.slice("scope-".length));
          if (Number.isFinite(idNum)) {
            setFocusedScopeId(idNum);
          }
        } else if (panel.id.startsWith("canvas-")) {
          const idNum = Number(panel.id.slice("canvas-".length));
          if (Number.isFinite(idNum)) {
            setActiveCanvasId(idNum);
          }
        }
      });

      const st = useWorkspaceStore.getState();
      if (st.layoutRestoreSeq !== lastAppliedRestoreSeqRef.current) {
        lastAppliedRestoreSeqRef.current = st.layoutRestoreSeq;
        applyBoardLayoutRestore(api);
      } else {
        syncPanelsToStore(api, st.scopeOrder, st.canvasOrder);
      }
    },
    [
      applyBoardLayoutRestore,
      registerDockviewLayoutAccessor,
      removeCanvas,
      removeScope,
      setActiveCanvasId,
      setFocusedScopeId,
      syncPanelsToStore,
    ],
  );

  useEffect(() => {
    return () => {
      registerDockviewLayoutAccessor(null);
    };
  }, [registerDockviewLayoutAccessor]);

  useEffect(() => {
    const api = apiRef.current;
    if (!api) return;
    if (layoutRestoreSeq !== lastAppliedRestoreSeqRef.current) {
      lastAppliedRestoreSeqRef.current = layoutRestoreSeq;
      applyBoardLayoutRestore(api);
      return;
    }
    syncPanelsToStore(api, scopeOrder, canvasOrder);
  }, [scopeOrder, canvasOrder, layoutRestoreSeq, applyBoardLayoutRestore, syncPanelsToStore]);

  const handleSplitLayout = (direction: "below" | "right") => {
    const api = apiRef.current;
    if (!api) return;
    const p1 = api.getPanel("scope-1");
    const p2 = api.getPanel("scope-2");
    if (p1 && p2) {
      p2.api.moveTo({
        group: p1.group,
        position: direction === "below" ? "bottom" : "right",
      });
    } else {
      addScope();
    }
  };

  const handleExportBoardClick = async () => {
    try {
      const json = exportBoardConfig();
      const saved = await saveJsonFile(json, "ddscope-board.json");
      if (saved) {
        setBoardNotice({
          kind: "ok",
          text: t("stage.boardExported", {
            scopes: scopeOrder.length,
            canvases: canvasOrder.length,
          }),
        });
      }
    } catch (err: unknown) {
      setBoardNotice({
        kind: "err",
        text: t("stage.boardImportError", {
          error: err instanceof Error ? err.message : String(err),
        }),
      });
    }
  };

  const handleImportBoardFile = async (ev: ChangeEvent<HTMLInputElement>) => {
    try {
      const text = await readJsonFileFromInput(ev.currentTarget);
      if (text === null) return;
      const res = await importBoardConfig(text);
      if (res.missingChannelPaths.length > 0) {
        setBoardNotice({
          kind: "warn",
          text: t("stage.boardImportedMissing", {
            scopes: res.scopeCount,
            canvases: res.canvasCount,
            missing: res.missingChannelPaths.length,
          }),
        });
      } else {
        setBoardNotice({
          kind: "ok",
          text: t("stage.boardImported", {
            scopes: res.scopeCount,
            canvases: res.canvasCount,
            widgets: res.widgetCount,
          }),
        });
      }
    } catch (err: unknown) {
      setBoardNotice({
        kind: "err",
        text: t("stage.boardImportError", {
          error: err instanceof Error ? err.message : String(err),
        }),
      });
    }
  };

  return (
    <div className="dockview-stage" data-testid="dockview-stage">
      <div className="stage-mini-bar">
        <div className="stage-mini-left">
          <button
            type="button"
            className="scope-btn scope-btn-accent"
            data-testid="add-scope-btn"
            onClick={() => addScope()}
          >
            {t("stage.addScope")}
          </button>
          <button
            type="button"
            className="scope-btn"
            data-testid="add-canvas-btn"
            onClick={() => addCanvas()}
          >
            {t("stage.addCanvas")}
          </button>
          <button
            type="button"
            className="scope-btn"
            onClick={() => handleSplitLayout("below")}
          >
            {t("stage.splitStacked")}
          </button>
          <button
            type="button"
            className="scope-btn"
            onClick={() => handleSplitLayout("right")}
          >
            {t("stage.splitSideBySide")}
          </button>
        </div>
        <span className="stage-hint">{t("stage.hint")}</span>
        <div className="stage-mini-right">
          <button
            type="button"
            className="scope-btn"
            data-testid="export-board-btn"
            title={t("stage.exportBoardTitle")}
            onClick={() => {
              void handleExportBoardClick();
            }}
          >
            {t("stage.exportBoard")}
          </button>
          <button
            type="button"
            className="scope-btn"
            data-testid="import-board-btn"
            title={t("stage.importBoardTitle")}
            onClick={() => boardImportInputRef.current?.click()}
          >
            {t("stage.importBoard")}
          </button>
          <input
            ref={boardImportInputRef}
            type="file"
            accept=".json,application/json"
            style={{ display: "none" }}
            data-testid="import-board-input"
            onChange={(ev) => {
              void handleImportBoardFile(ev);
            }}
          />
        </div>
      </div>

      {boardNotice && (
        <div
          className={`config-io-notice ${boardNotice.kind}`}
          data-testid="board-config-notice"
        >
          <span>{boardNotice.text}</span>
          <button
            type="button"
            className="scope-btn-xs"
            onClick={() => setBoardNotice(null)}
          >
            ×
          </button>
        </div>
      )}

      <div className="dockview-container">
        <DockviewReact
          theme={themeAbyss}
          components={components}
          watermarkComponent={WorkspaceWatermark}
          onReady={handleReady}
        />
      </div>
    </div>
  );
}
