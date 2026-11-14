import { useEffect } from "react";
import { formatBytesCompact } from "../scope-engine/time-format.ts";
import { ChannelTreeSidebar } from "./ChannelTreeSidebar.tsx";
import { DockviewStage } from "./DockviewStage.tsx";
import { DownlinkBar } from "./DownlinkBar.tsx";
import { SourceWizardModal } from "./SourceWizardModal.tsx";
import { useWorkspaceStore } from "./store.ts";

export function WorkspaceShell() {
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

  return (
    <div className="ddscope-shell" data-testid="workspace-shell">
      <header className="ddscope-topbar">
        <div className="ddscope-brand">
          <span className="ddscope-badge">DDScopeHost</span>
          <span className="ddscope-subtitle">Phase 1 · MSO Oscilloscope Workbench</span>
        </div>

        <div className="ddscope-telemetry">
          <span className="telemetry-pill">
            Host:{" "}
            <strong data-testid="topbar-host-mode">
              {runtimeStatus ? runtimeStatus.mode : "detecting"}
            </strong>
          </span>

          <span className="telemetry-pill">
            Core:{" "}
            <strong>
              {runtimeStatus
                ? `${runtimeStatus.engine.core_name} v${runtimeStatus.engine.core_version}`
                : "..."}
            </strong>
          </span>

          <span className="telemetry-pill">
            GPU:{" "}
            <strong data-testid="topbar-gpu-mode">
              {runtimeStatus
                ? runtimeStatus.webgpuAvailable
                  ? "WebGPU"
                  : "WebGL2"
                : "..."}
            </strong>
          </span>

          <span className="telemetry-pill telemetry-fps">
            DDSF:{" "}
            <strong data-testid="topbar-fps">
              {dataPlaneMetrics.ddsfFps.toFixed(0)} fps
            </strong>
            <span className="telemetry-sub">
              {formatBytesCompact(dataPlaneMetrics.bytesPerSec)}/s · seq #
              {dataPlaneMetrics.frameSeq}
            </span>
          </span>

          {runtimeStatus?.mode === "browser-mock" && (
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
          )}

          <button
            type="button"
            className="scope-btn scope-btn-warn"
            data-testid="topbar-glitch-btn"
            title="Inject a single-sample +80V (128V peak) overvoltage glitch on v_bus"
            onClick={() => void injectGlitch()}
          >
            ⚡ Glitch +80V
          </button>

          <span className={`telemetry-pill ${isQuotaWarn ? "pill-warn" : ""}`}>
            RAM:{" "}
            <strong>
              {formatBytesCompact(quota.usedBytes)} / {formatBytesCompact(quota.limitBytes)}
            </strong>{" "}
            <span>({quotaPct}%)</span>
          </span>

          {hasPinned && (
            <span className="telemetry-pill pill-pinned" data-testid="topbar-pinned-badge">
              Pinned {formatBytesCompact(quota.pinnedBytes)}
              <button
                type="button"
                className="scope-btn-xs scope-btn-accent"
                data-testid="topbar-release-pins-btn"
                onClick={handleReleaseAllPins}
              >
                Release Pins
              </button>
            </span>
          )}
        </div>
      </header>

      <div className="ddscope-workspace-body">
        {initError ? (
          <main className="ddscope-viewport-stage ddscope-center-stage">
            <div className="ddscope-card ddscope-error">
              <h2>Workspace Initialization Error</h2>
              <p>{initError}</p>
            </div>
          </main>
        ) : !initialized ? (
          <main className="ddscope-viewport-stage ddscope-center-stage">
            <div className="ddscope-card">
              <h2>Starting DDScopeHost High-Frequency Workbench...</h2>
              <p>Initializing DDSF v1 data bus, simulation worker, and WebGPU/WebGL2 stage.</p>
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
