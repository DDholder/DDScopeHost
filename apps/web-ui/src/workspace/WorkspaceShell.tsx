import { useEffect } from "react";
import type { UiLocale } from "../i18n/index.ts";
import { formatBytesCompact } from "../scope-engine/time-format.ts";
import { ChannelTreeSidebar } from "./ChannelTreeSidebar.tsx";
import { DockviewStage } from "./DockviewStage.tsx";
import { DownlinkBar } from "./DownlinkBar.tsx";
import { SourceWizardModal } from "./SourceWizardModal.tsx";
import { useWorkspaceStore } from "./store.ts";
import { useI18n } from "./use-i18n.ts";

export function WorkspaceShell() {
  const { locale, setLocale, t } = useI18n();
  const initialized = useWorkspaceStore((s) => s.initialized);
  const initError = useWorkspaceStore((s) => s.initError);
  const runtimeStatus = useWorkspaceStore((s) => s.runtimeStatus);
  const dataPlaneMetrics = useWorkspaceStore((s) => s.dataPlaneMetrics);
  const quota = useWorkspaceStore((s) => s.quota);
  const mockRateHz = useWorkspaceStore((s) => s.mockRateHz);
  const scopes = useWorkspaceStore((s) => s.scopes);
  const initWorkspace = useWorkspaceStore((s) => s.initWorkspace);
  const setMockRateHz = useWorkspaceStore((s) => s.setMockRateHz);
  const injectGlitch = useWorkspaceStore((s) => s.injectGlitch);
  const rearmScope = useWorkspaceStore((s) => s.rearmScope);

  useEffect(() => {
    void initWorkspace();
  }, [initWorkspace]);

  const handleReleaseAllPins = () => {
    Object.values(scopes).forEach((sc) => {
      if (sc.sweepMode === "single" || sc.sweepMode === "normal") {
        rearmScope(sc.scopeId);
      }
    });
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

          <div className="topbar-rate-switch" role="group" aria-label="Mock Tick Rate">
            <button
              type="button"
              className={`scope-seg-btn ${mockRateHz === 60 ? "active" : ""}`}
              data-testid="rate-60-btn"
              onClick={() => setMockRateHz(60)}
            >
              60Hz
            </button>
            <button
              type="button"
              className={`scope-seg-btn ${mockRateHz === 120 ? "active" : ""}`}
              data-testid="rate-120-btn"
              onClick={() => setMockRateHz(120)}
            >
              120Hz
            </button>
          </div>

          <button
            type="button"
            className="scope-btn scope-btn-warn"
            data-testid="topbar-glitch-btn"
            title={t("topbar.glitchTitle")}
            onClick={() => void injectGlitch()}
          >
            {t("topbar.glitchBtn")}
          </button>

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
            <div className="ddscope-main-row">
              <ChannelTreeSidebar />
              <main className="ddscope-viewport-stage">
                <DockviewStage />
              </main>
            </div>
            <DownlinkBar />
          </>
        )}
      </div>

      <SourceWizardModal />
    </div>
  );
}
