import { commands, events, type EngineStatusDto } from "./bindings";
import { DDSF_VERSION_V1 } from "./ddsf-spec";

export * from "./bindings";
export * from "./ddsf-spec";
export { commands, events };

export type HostRuntimeMode = "tauri" | "browser-mock";

export interface RuntimeStatus {
  mode: HostRuntimeMode;
  engine: EngineStatusDto;
  webgpuAvailable: boolean;
}

/**
 * Checks whether the frontend is hosted inside a Tauri WebView or a standalone browser.
 */
export function isTauriHost(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/**
 * Fetches core engine build metadata via `tauri-specta` in desktop mode,
 * or falls back to mock metadata for standalone browser development.
 */
export async function fetchEngineStatus(): Promise<RuntimeStatus> {
  const webgpuAvailable = typeof navigator !== "undefined" && "gpu" in navigator;

  if (isTauriHost()) {
    const engine = await commands.getEngineInfo();
    return {
      mode: "tauri",
      engine,
      webgpuAvailable,
    };
  }

  return {
    mode: "browser-mock",
    engine: {
      core_name: "ddscope-core (browser mock)",
      core_version: "0.1.0",
      bridge_ready: false,
      ddsf_version: DDSF_VERSION_V1,
    },
    webgpuAvailable,
  };
}
