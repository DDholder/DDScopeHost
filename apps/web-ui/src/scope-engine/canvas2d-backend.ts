import type { ScopeRenderBackend } from "./backend.ts";
import {
  FLOATS_PER_VERTEX,
  VERTEX_MODE_CAPSULE,
  VERTEX_MODE_DENSE_HATCH,
  type BuiltScopeGeometry,
} from "./geometry.ts";
import type { MsoLayout } from "./layout.ts";

/**
 * Software Canvas 2D fallback used only when both WebGPU and WebGL2 are unavailable.
 */
export class Canvas2dScopeBackend implements ScopeRenderBackend {
  readonly kind = "canvas2d" as const;

  private readonly ctx: CanvasRenderingContext2D;

  constructor(ctx: CanvasRenderingContext2D) {
    this.ctx = ctx;
  }

  resize(_physicalWidth: number, _physicalHeight: number): void {
    // Dimensions are controlled on the canvas element directly
  }

  render(
    physicalWidth: number,
    physicalHeight: number,
    dpr: number,
    layout: MsoLayout,
    geometry: BuiltScopeGeometry,
  ): void {
    if (physicalWidth < 2 || physicalHeight < 2) return;
    const ctx = this.ctx;
    ctx.save();
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.fillStyle = "#070a11";
    ctx.fillRect(0, 0, physicalWidth, physicalHeight);

    const drawTriangles = (startVertex: number, vertexCount: number) => {
      const v = geometry.vertices;
      for (let idx = startVertex; idx + 2 < startVertex + vertexCount; idx += 3) {
        const b0 = idx * FLOATS_PER_VERTEX;
        const b1 = (idx + 1) * FLOATS_PER_VERTEX;
        const b2 = (idx + 2) * FLOATS_PER_VERTEX;
        const r = Math.round(v[b0 + 2] * 255);
        const g = Math.round(v[b0 + 3] * 255);
        const b = Math.round(v[b0 + 4] * 255);
        const mode = Math.round(v[b0 + 10]);
        const alpha =
          mode === VERTEX_MODE_DENSE_HATCH
            ? v[b0 + 5] * 0.65
            : mode === VERTEX_MODE_CAPSULE
              ? v[b0 + 5]
              : v[b0 + 5];

        ctx.fillStyle = `rgba(${r}, ${g}, ${b}, ${alpha})`;
        ctx.beginPath();
        ctx.moveTo(v[b0], v[b0 + 1]);
        ctx.lineTo(v[b1], v[b1 + 1]);
        ctx.lineTo(v[b2], v[b2 + 1]);
        ctx.closePath();
        ctx.fill();
      }
    };

    if (geometry.gridVertexCount > 0) {
      drawTriangles(0, geometry.gridVertexCount);
    }
    if (geometry.signalVertexCount > 0) {
      ctx.save();
      ctx.beginPath();
      ctx.rect(
        layout.plotLeft * dpr,
        layout.plotTop * dpr,
        layout.plotWidth * dpr,
        layout.plotHeight * dpr,
      );
      ctx.clip();
      drawTriangles(geometry.gridVertexCount, geometry.signalVertexCount);
      ctx.restore();
    }

    ctx.restore();
  }

  dispose(): void {
    // Nothing to dispose
  }
}
