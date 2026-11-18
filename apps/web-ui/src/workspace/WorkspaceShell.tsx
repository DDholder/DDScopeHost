import { useEffect, useRef, useState } from "react";
import type { UiLocale } from "../i18n/index.ts";
import { formatBytesCompact } from "../scope-engine/time-format.ts";
import { ChannelTreeSidebar } from "./ChannelTreeSidebar.tsx";
import { DockviewStage } from "./DockviewStage.tsx";
import { readJsonFileFromInput, saveJsonFile } from "./file-dialog.ts";
import { SourceWizardModal } from "./SourceWizardModal.tsx";
import { useWorkspaceStore } from "./store.ts";
import { useI18n } from "./use-i18n.ts";
import { WorkspacePageBar } from "./WorkspacePageBar.tsx";

export function WorkspaceShell() {
  const { locale, setLocale, t } = useI18n();
  const initialized = useWorkspaceStore((s) => s.initialized);
  const initError = useWorkspaceStore((s) => s.initError);
  const runtimeStatus = useWorkspaceStore((s) => s.runtimeStatus);
  const dataPlaneMetrics = useWorkspaceStore((s) => s.dataPlaneMetrics);
  const quota = useWorkspaceStore((s) => s.quota);
  const scopes = useWorkspaceStore((s) => s.scopes);
  const initWorkspace = useWorkspaceStore((s) => s.initWorkspace);
  const rearmScope = useWorkspaceStore((s) => s.rearmScope);
  const exportProjectConfig = useWorkspaceStore((s) => s.exportProjectConfig);
  const importProjectConfig = useWorkspaceStore((s) => s.importProjectConfig);
  const saveDefaultProjectConfig = useWorkspaceStore((s) => s.saveDefaultProjectConfig);

  const [projectNotice, setProjectNotice] = useState<{
    kind: "ok" | "warn" | "err";
    text: string;
  } | null>(null);
  const [projectBusy, setProjectBusy] = useState(false);
  const projectFileInputRef = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    void initWorkspace();
  }, [initWorkspace]);

  const handleReleaseAllPins = () => {
    Object.values(scopes).forEach((sc) => {
      rearmScope(sc.scopeId);
    });
  };

  const handleExportProject = async () => {
    try {
      const json = exportProjectConfig();
      const saved = await saveJsonFile(json, "ddscope-project.json");
      if (saved) {
        setProjectNotice({
          kind: "ok",
          text: t("topbar.projectExported"),
        });
      }
    } catch (err: unknown) {
      setProjectNotice({
        kind: "err",
        text: t("topbar.projectConfigError", {
          error: err instanceof Error ? err.message : String(err),
        }),
      });
    }
  };

  const handleImportProjectFile = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const input = e.currentTarget;
    try {
      const jsonText = await readJsonFileFromInput(input);
      if (jsonText === null) return;
      setProjectBusy(true);
      const res = await importProjectConfig(jsonText);
      if (res.missingChannelPaths.length > 0) {
        setProjectNotice({
          kind: "warn",
          text: t("topbar.projectImportedMissing", {
            sources: res.sourceCount,
            scopes: res.scopeCount,
            canvases: res.canvasCount,
            missing: res.missingChannelPaths.length,
          }),
        });
      } else {
        setProjectNotice({
          kind: "ok",
          text: t("topbar.projectImported", {
            sources: res.sourceCount,
            scopes: res.scopeCount,
            canvases: res.canvasCount,
            widgets: res.widgetCount,
          }),
        });
      }
    } catch (err: unknown) {
      setProjectNotice({
        kind: "err",
        text: t("topbar.projectConfigError", {
          error: err instanceof Error ? err.message : String(err),
        }),
      });
    } finally {
      setProjectBusy(false);
    }
  };

  const handleSaveDefaultProject = async () => {
    try {
      setProjectBusy(true);
      const res = await saveDefaultProjectConfig();
      setProjectNotice({
        kind: "ok",
        text:
          res.mode === "exe-dir"
            ? t("topbar.projectDefaultSavedExe", { path: res.location })
            : t("topbar.projectDefaultSavedBrowser"),
      });
    } catch (err: unknown) {
      setProjectNotice({
        kind: "err",
        text: t("topbar.projectConfigError", {
          error: err instanceof Error ? err.message : String(err),
        }),
      });
    } finally {
      setProjectBusy(false);
    }
  };

  const quotaPct =
    quota.limitBytes > 0
      ? Math.min(100, Math.round((quota.usedBytes / quota.limitBytes) * 100))
      : 0;
  const isQuotaWarn = dataPlaneMetrics.quotaWarning || quotaPct >= 85;
  const hasPinned = dataPlaneMetrics.pinnedActive || quota.pinnedBytes > 0;
  const hostModeText = !runtimeStatus
    ? t("topbar.detecting")
    : runtimeStatus.mode === "browser-mock"
      ? t("topbar.hostMock")
      : t("topbar.hostTauri");

  return (
    <div className="ddscope-shell" data-testid="workspace-shell">
      <header className="ddscope-topbar">
        <div className="ddscope-brand">
          <span className="ddscope-badge">DDScopeHost</span>
          <span className="ddscope-subtitle">{t("topbar.subtitle")}</span>
        </div>

        <div className="topbar-project-actions" data-testid="topbar-project-actions">
          <button
            type="button"
            className="scope-btn-xs"
            data-testid="export-project-btn"
            title={t("topbar.exportProjectTitle")}
            disabled={!initialized || projectBusy}
            onClick={() => void handleExportProject()}
          >
            {t("topbar.exportProject")}
          </button>
          <button
            type="button"
            className="scope-btn-xs"
            data-testid="import-project-btn"
            title={t("topbar.importProjectTitle")}
            disabled={!initialized || projectBusy}
            onClick={() => projectFileInputRef.current?.click()}
          >
            {t("topbar.importProject")}
          </button>
          <button
            type="button"
            className="scope-btn-xs scope-btn-accent"
            data-testid="save-default-project-btn"
            title={t("topbar.saveDefaultProjectTitle")}
            disabled={!initialized || projectBusy}
            onClick={() => void handleSaveDefaultProject()}
          >
            {t("topbar.saveDefaultProject")}
          </button>
          <input
            ref={projectFileInputRef}
            type="file"
            accept=".json,application/json"
            style={{ display: "none" }}
            data-testid="import-project-input"
            onChange={(e) => void handleImportProjectFile(e)}
          />
        </div>

        <div className="ddscope-telemetry">
          <span className="telemetry-pill">
            {t("topbar.host")}:{" "}
            <strong data-testid="topbar-host-mode">{hostModeText}</strong>
          </span>

          <span className="telemetry-pill">
            {t("topbar.core")}:{" "}
            <strong>
              {runtimeStatus
                ? `${runtimeStatus.engine.core_name} v${runtimeStatus.engine.core_version}`
                : "..."}
            </strong>
          </span>

          <span className="telemetry-pill">
            {t("topbar.gpu")}:{" "}
            <strong data-testid="topbar-gpu-mode">
              {runtimeStatus
                ? runtimeStatus.webgpuAvailable
                  ? "WebGPU"
                  : "WebGL2"
                : "..."}
            </strong>
          </span>

          <span className="telemetry-pill telemetry-fps">
            {t("topbar.ddsf")}:{" "}
            <strong data-testid="topbar-fps">
              {dataPlaneMetrics.ddsfFps.toFixed(0)} fps
            </strong>
            <span className="telemetry-sub">
              {formatBytesCompact(dataPlaneMetrics.bytesPerSec)}/s ·{" "}
              {t("topbar.seq", { seq: dataPlaneMetrics.frameSeq })}
            </span>
          </span>

          <span className={`telemetry-pill ${isQuotaWarn ? "pill-warn" : ""}`}>
            {t("topbar.ram")}:{" "}
            <strong>
              {formatBytesCompact(quota.usedBytes)} / {formatBytesCompact(quota.limitBytes)}
            </strong>{" "}
            <span>({quotaPct}%)</span>
          </span>

          {hasPinned && (
            <span className="telemetry-pill pill-pinned" data-testid="topbar-pinned-badge">
              {t("topbar.pinned", { size: formatBytesCompact(quota.pinnedBytes) })}
              <button
                type="button"
                className="scope-btn-xs scope-btn-accent"
                data-testid="topbar-release-pins-btn"
                onClick={handleReleaseAllPins}
              >
                {t("topbar.releasePins")}
              </button>
            </span>
          )}

          <select
            className="scope-select topbar-lang-select"
            data-testid="topbar-lang-select"
            aria-label={t("topbar.languageLabel")}
            value={locale}
            onChange={(e) => setLocale(e.target.value as UiLocale)}
          >
            <option value="zh-CN">简体中文</option>
            <option value="en">English</option>
          </select>
        </div>
      </header>

      {projectNotice && (
        <div
          className={`config-io-notice config-io-notice-${projectNotice.kind}`}
          data-testid="project-config-notice"
        >
          <span>{projectNotice.text}</span>
          <button
            type="button"
            className="config-io-notice-close"
            onClick={() => setProjectNotice(null)}
          >
            ×
          </button>
        </div>
      )}

      <div className="ddscope-workspace-body">
        {initError ? (
          <main className="ddscope-viewport-stage ddscope-center-stage">
            <div className="ddscope-card ddscope-error">
              <h2>{t("topbar.initErrorTitle")}</h2>
              <p>{initError}</p>
            </div>
          </main>
        ) : !initialized ? (
          <main className="ddscope-viewport-stage ddscope-center-stage">
            <div className="ddscope-card">
              <h2>{t("topbar.startingTitle")}</h2>
              <p>{t("topbar.startingDesc")}</p>
            </div>
          </main>
        ) : (
          <>
            <WorkspacePageBar />
            <div className="ddscope-main-row">
              <ChannelTreeSidebar />
              <main className="ddscope-viewport-stage">
                <DockviewStage />
              </main>
            </div>
          </>
        )}
      </div>

      <SourceWizardModal />
    </div>
  );
}
