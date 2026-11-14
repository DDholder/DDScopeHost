import type { ScopeRenderBackend } from "./backend.ts";
import { Canvas2dScopeBackend } from "./canvas2d-backend.ts";
import { WebGl2ScopeBackend } from "./webgl2-backend.ts";
import { WebGpuScopeBackend } from "./webgpu-backend.ts";

let sharedGpuDevicePromise: Promise<GPUDevice | null> | null = null;

async function getSharedWebGpuDevice(): Promise<GPUDevice | null> {
  if (typeof navigator === "undefined" || !("gpu" in navigator) || !navigator.gpu) {
    return null;
  }
  if (!sharedGpuDevicePromise) {
    sharedGpuDevicePromise = (async () => {
      try {
        const adapter = await navigator.gpu.requestAdapter({
          powerPreference: "high-performance",
        });
        if (!adapter) return null;
        const device = await adapter.requestDevice();
        device.lost.then(() => {
          sharedGpuDevicePromise = null;
        });
        return device;
      } catch {
        return null;
      }
    })();
  }
  return sharedGpuDevicePromise;
}

/**
 * Creates the optimal hardware-accelerated `ScopeRenderBackend` (`WebGPU` primary ->
 * `WebGL2` automatic fallback). Never calls `canvas.getContext("webgpu")` until a live
 * `GPUDevice` has been acquired, keeping the `<canvas>` unlocked for WebGL2 fallback.
 */
export async function createScopeRenderBackend(
  canvas: HTMLCanvasElement,
): Promise<ScopeRenderBackend> {
  const device = await getSharedWebGpuDevice();
  if (device && typeof navigator !== "undefined" && navigator.gpu) {
    const gpuCtx = canvas.getContext("webgpu");
    if (gpuCtx) {
      try {
        const format = navigator.gpu.getPreferredCanvasFormat();
        gpuCtx.configure({
          device,
          format,
          alphaMode: "premultiplied",
        });
        return new WebGpuScopeBackend(device, gpuCtx, format);
      } catch {
        // Fall through to WebGL2 / Canvas2D if WebGPU context configuration fails
      }
    }
  }

  const gl = canvas.getContext("webgl2", {
    alpha: false,
    antialias: true,
    powerPreference: "high-performance",
  });
  if (gl) {
    return new WebGl2ScopeBackend(gl);
  }

  const ctx2d = canvas.getContext("2d");
  if (ctx2d) {
    return new Canvas2dScopeBackend(ctx2d);
  }

  throw new Error("Unable to acquire WebGPU, WebGL2, or 2D canvas context");
}
