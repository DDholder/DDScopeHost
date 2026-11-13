import { useEffect, useState } from "react";
import { fetchEngineStatus, type RuntimeStatus } from "../bridge";

export function WorkspaceShell() {
  const [status, setStatus] = useState<RuntimeStatus | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    fetchEngineStatus()
      .then((res) => {
        if (active) setStatus(res);
      })
      .catch((err: unknown) => {
        if (active) {
          setError(err instanceof Error ? err.message : String(err));
        }
      });
    return () => {
      active = false;
    };
  }, []);

  return (
    <div className="ddscope-shell">
      <header className="ddscope-topbar">
        <div className="ddscope-brand">
          <span className="ddscope-badge">DDScopeHost</span>
          <span className="ddscope-subtitle">Phase 1 · M0 Workspace Foundation</span>
        </div>
        <div className="ddscope-telemetry">
          <span>
            Host:{" "}
            <strong>{status ? status.mode : "detecting..."}</strong>
          </span>
          <span>
            Core:{" "}
            <strong>
              {status
                ? `${status.engine.core_name} v${status.engine.core_version}`
                : "loading..."}
            </strong>
          </span>
          <span>
            Bridge:{" "}
            <strong>
              {status ? (status.engine.bridge_ready ? "connected" : "mock") : "..."}
            </strong>
          </span>
          <span>
            WebGPU:{" "}
            <strong>
              {status
                ? status.webgpuAvailable
                  ? "available"
                  : "fallback-webgl2"
                : "..."}
            </strong>
          </span>
        </div>
      </header>

      <main className="ddscope-viewport-stage">
        {error ? (
          <div className="ddscope-card ddscope-error">
            <h2>Bridge Initialization Error</h2>
            <p>{error}</p>
          </div>
        ) : (
          <div className="ddscope-card">
            <h1>MSO Oscilloscope Workspace Shell Ready</h1>
            <p>
              Dockview multi-pane layout, global channel tree, and WebGPU/WebGL2 MSO
              shaders will mount inside this stage in Track C.
            </p>
          </div>
        )}
      </main>
    </div>
  );
}
