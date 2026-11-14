import { useCallback, useEffect, useMemo, useRef } from "react";
import {
  DockviewReact,
  themeAbyss,
  type DockviewApi,
  type DockviewReadyEvent,
  type IDockviewPanelProps,
  type IWatermarkPanelProps,
} from "dockview";
import { ScopeViewportWidget, type ScopeCallbacks } from "../scope-engine/index.ts";
import { useWorkspaceStore } from "./store.ts";
import { useI18n } from "./use-i18n.ts";

interface ScopePanelParams {
  scopeId: number;
}

function MsoScopePanel(props: IDockviewPanelProps<ScopePanelParams>) {
  const { locale, t } = useI18n();
  const scopeId = props.params.scopeId;
  const config = useWorkspaceStore((s) => s.scopes[scopeId]);
  const channelsById = useWorkspaceStore((s) => s.channelsById);
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
      onFocusScope={setFocusedScopeId}
      onCanvasWidthChanged={setScopeCanvasWidth}
    />
  );
}

function WorkspaceWatermark(_props: IWatermarkPanelProps) {
  const { t } = useI18n();
  const addScope = useWorkspaceStore((s) => s.addScope);
  return (
    <div className="dockview-watermark">
      <h3>{t("stage.emptyTitle")}</h3>
      <p>{t("stage.emptyDesc")}</p>
      <button
        type="button"
        className="scope-btn scope-btn-accent"
        onClick={() => addScope()}
      >
        {t("stage.addScope")}
      </button>
    </div>
  );
}

export function DockviewStage() {
  const { t } = useI18n();
  const scopeOrder = useWorkspaceStore((s) => s.scopeOrder);
  const scopes = useWorkspaceStore((s) => s.scopes);
  const addScope = useWorkspaceStore((s) => s.addScope);
  const removeScope = useWorkspaceStore((s) => s.removeScope);
  const setFocusedScopeId = useWorkspaceStore((s) => s.setFocusedScopeId);

  const apiRef = useRef<DockviewApi | null>(null);
  const scopesRef = useRef(scopes);
  scopesRef.current = scopes;

  const components = useMemo(
    () => ({
      msoScope: MsoScopePanel,
    }),
    [],
  );

  const syncPanelsToStore = useCallback((api: DockviewApi, order: number[]) => {
    let firstPanelId: string | undefined;
    for (let i = 0; i < order.length; i += 1) {
      const scopeId = order[i];
      const panelId = `scope-${scopeId}`;
      if (!api.getPanel(panelId)) {
        const cfg = scopesRef.current[scopeId];
        if (!cfg) continue;
        if (i === 0 || !firstPanelId || !api.getPanel(firstPanelId)) {
          api.addPanel({
            id: panelId,
            component: "msoScope",
            title: cfg.title,
            params: { scopeId },
          });
          firstPanelId = panelId;
        } else {
          api.addPanel({
            id: panelId,
            component: "msoScope",
            title: cfg.title,
            params: { scopeId },
            position: {
              referencePanel: firstPanelId,
              direction: i === 1 ? "below" : "within",
            },
          });
        }
      } else if (!firstPanelId) {
        firstPanelId = panelId;
      }
    }
  }, []);

  const handleReady = useCallback(
    (event: DockviewReadyEvent) => {
      const api = event.api;
      apiRef.current = api;
      syncPanelsToStore(api, useWorkspaceStore.getState().scopeOrder);

      api.onDidRemovePanel((panel) => {
        const prefix = "scope-";
        if (panel.id.startsWith(prefix)) {
          const idNum = Number(panel.id.slice(prefix.length));
          if (Number.isFinite(idNum) && useWorkspaceStore.getState().scopes[idNum]) {
            removeScope(idNum);
          }
        }
      });

      api.onDidActivePanelChange((panel) => {
        if (!panel) return;
        const prefix = "scope-";
        if (panel.id.startsWith(prefix)) {
          const idNum = Number(panel.id.slice(prefix.length));
          if (Number.isFinite(idNum)) {
            setFocusedScopeId(idNum);
          }
        }
      });
    },
    [removeScope, setFocusedScopeId, syncPanelsToStore],
  );

  useEffect(() => {
    const api = apiRef.current;
    if (!api) return;
    syncPanelsToStore(api, scopeOrder);
  }, [scopeOrder, syncPanelsToStore]);

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
      </div>

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
