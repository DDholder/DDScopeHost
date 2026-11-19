import type { ScopeRenderBackend } from "./backend.ts";
import {
  FLOATS_PER_VERTEX,
  VERTEX_MODE_CAPSULE,
  VERTEX_MODE_DENSE_HATCH,
  type BuiltScopeGeometry,
} from "./geometry.ts";
import type { MsoLayout } from "./layout.ts";
import type { PersistenceRenderState } from "./persistence.ts";

type PersistCanvasElement = OffscreenCanvas | HTMLCanvasElement;
type PersistCanvasCtx = OffscreenCanvasRenderingContext2D | CanvasRenderingContext2D;

/**
 * Software Canvas 2D fallback used only when both WebGPU and WebGL2 are unavailable.
 */
export class Canvas2dScopeBackend implements ScopeRenderBackend {
  readonly kind = "canvas2d" as const;

  private readonly ctx: CanvasRenderingContext2D;
  private persistCanvas: PersistCanvasElement | null = null;
  private persistCtx: PersistCanvasCtx | null = null;
  private persistWidth = 0;
  private persistHeight = 0;
  private persistNeedsClear = true;
  private pendingDecayDtSec = 0;

  constructor(ctx: CanvasRenderingContext2D) {
    this.ctx = ctx;
  }

  resize(_physicalWidth: number, _physicalHeight: number): void {
    // Dimensions are controlled on the canvas element directly
  }

  private ensurePersistenceCanvas(
    physicalWidth: number,
    physicalHeight: number,
  ): { allocated: boolean; ctx: PersistCanvasCtx } | null {
    if (
      this.persistCanvas &&
      this.persistCtx &&
      this.persistWidth === physicalWidth &&
      this.persistHeight === physicalHeight
    ) {
      return { allocated: false, ctx: this.persistCtx };
    }

    let canvas: PersistCanvasElement | null = null;
    if (typeof OffscreenCanvas !== "undefined") {
      canvas = new OffscreenCanvas(physicalWidth, physicalHeight);
    } else if (typeof document !== "undefined") {
      const el = document.createElement("canvas");
      el.width = physicalWidth;
      el.height = physicalHeight;
      canvas = el;
    }

    if (!canvas) {
      return null;
    }

    const pCtx = canvas.getContext("2d") as PersistCanvasCtx | null;
    if (!pCtx) {
      return null;
    }

    this.persistCanvas = canvas;
    this.persistCtx = pCtx;
    this.persistWidth = physicalWidth;
    this.persistHeight = physicalHeight;
    this.persistNeedsClear = true;
    this.pendingDecayDtSec = 0;
    return { allocated: true, ctx: pCtx };
  }

  private drawTriangles(
    targetCtx: PersistCanvasCtx,
    geometry: BuiltScopeGeometry,
    startVertex: number,
    vertexCount: number,
    alphaMul = 1,
    thermalTint = false,
  ): void {
    const v = geometry.vertices;
    for (let idx = startVertex; idx + 2 < startVertex + vertexCount; idx += 3) {
      const b0 = idx * FLOATS_PER_VERTEX;
      const b1 = (idx + 1) * FLOATS_PER_VERTEX;
      const b2 = (idx + 2) * FLOATS_PER_VERTEX;
      const r = thermalTint ? 238 : Math.round(v[b0 + 2] * 255);
      const g = thermalTint ? 112 : Math.round(v[b0 + 3] * 255);
      const b = thermalTint ? 32 : Math.round(v[b0 + 4] * 255);
      const mode = Math.round(v[b0 + 10]);
      const baseAlpha =
        mode === VERTEX_MODE_DENSE_HATCH
          ? v[b0 + 5] * 0.65
          : mode === VERTEX_MODE_CAPSULE
            ? v[b0 + 5]
            : v[b0 + 5];
      const alpha = Math.max(0, Math.min(1, baseAlpha * alphaMul));

      targetCtx.fillStyle = `rgba(${r}, ${g}, ${b}, ${alpha})`;
      targetCtx.beginPath();
      targetCtx.moveTo(v[b0], v[b0 + 1]);
      targetCtx.lineTo(v[b1], v[b1 + 1]);
      targetCtx.lineTo(v[b2], v[b2 + 1]);
      targetCtx.closePath();
      targetCtx.fill();
    }
  }

  render(
    physicalWidth: number,
    physicalHeight: number,
    dpr: number,
    layout: MsoLayout,
    geometry: BuiltScopeGeometry,
    persistence?: PersistenceRenderState,
  ): void {
    if (physicalWidth < 2 || physicalHeight < 2) return;
    const ctx = this.ctx;
    ctx.save();
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.fillStyle = "#070a11";
    ctx.fillRect(0, 0, physicalWidth, physicalHeight);

    const persistSurface =
      persistence && persistence.mode !== "off"
        ? this.ensurePersistenceCanvas(physicalWidth, physicalHeight)
        : null;

    if (!persistence || persistence.mode === "off" || !persistSurface || !this.persistCanvas) {
      this.persistNeedsClear = true;
      this.pendingDecayDtSec = 0;

      if (geometry.gridVertexCount > 0) {
        this.drawTriangles(ctx, geometry, 0, geometry.gridVertexCount);
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
        this.drawTriangles(
          ctx,
          geometry,
          geometry.gridVertexCount,
          geometry.signalVertexCount,
        );
        ctx.restore();
      }
      ctx.restore();
      return;
    }

    const pCtx = persistSurface.ctx;
    const doClear = persistSurface.allocated || persistence.clear || this.persistNeedsClear;

    if (doClear) {
      pCtx.save();
      pCtx.setTransform(1, 0, 0, 1, 0, 0);
      pCtx.clearRect(0, 0, physicalWidth, physicalHeight);
      pCtx.restore();
      this.persistNeedsClear = false;
      this.pendingDecayDtSec = 0;
    } else if (
      persistence.mode === "infinite" ||
      persistence.decayFactor >= 0.99999 ||
      !Number.isFinite(persistence.tauSec)
    ) {
      this.pendingDecayDtSec = 0;
    } else {
      this.pendingDecayDtSec += persistence.dtSec;
      const accumFactor = Math.exp(
        -this.pendingDecayDtSec / Math.max(1e-4, persistence.tauSec),
      );
      const fadeAmount = 1 - accumFactor;
      if (fadeAmount >= 0.04) {
        pCtx.save();
        pCtx.setTransform(1, 0, 0, 1, 0, 0);
        pCtx.globalCompositeOperation = "destination-out";
        pCtx.fillStyle = `rgba(0, 0, 0, ${Math.min(1, fadeAmount).toFixed(4)})`;
        pCtx.fillRect(0, 0, physicalWidth, physicalHeight);
        pCtx.restore();
        this.pendingDecayDtSec = 0;
      }
    }

    if (persistence.depositCurrentFrame && geometry.analogVertexCount > 0) {
      pCtx.save();
      pCtx.setTransform(1, 0, 0, 1, 0, 0);
      pCtx.beginPath();
      pCtx.rect(
        layout.analogRegion.x * dpr,
        layout.analogRegion.y * dpr,
        layout.analogRegion.width * dpr,
        layout.analogRegion.height * dpr,
      );
      pCtx.clip();
      pCtx.globalCompositeOperation = "lighter";
      this.drawTriangles(
        pCtx,
        geometry,
        geometry.gridVertexCount,
        geometry.analogVertexCount,
        0.42,
        persistence.colorMode === "thermal",
      );
      pCtx.restore();
    }

    if (geometry.gridVertexCount > 0) {
      this.drawTriangles(ctx, geometry, 0, geometry.gridVertexCount);
    }

    ctx.save();
    ctx.beginPath();
    ctx.rect(
      layout.analogRegion.x * dpr,
      layout.analogRegion.y * dpr,
      layout.analogRegion.width * dpr,
      layout.analogRegion.height * dpr,
    );
    ctx.clip();
    ctx.drawImage(this.persistCanvas, 0, 0);
    if (persistence.colorMode === "phosphor" && geometry.analogVertexCount > 0) {
      this.drawTriangles(
        ctx,
        geometry,
        geometry.gridVertexCount,
        geometry.analogVertexCount,
      );
    }
    ctx.restore();

    if (geometry.overlaySignalVertexCount > 0) {
      ctx.save();
      ctx.beginPath();
      ctx.rect(
        layout.plotLeft * dpr,
        layout.plotTop * dpr,
        layout.plotWidth * dpr,
        layout.plotHeight * dpr,
      );
      ctx.clip();
      this.drawTriangles(
        ctx,
        geometry,
        geometry.gridVertexCount + geometry.analogVertexCount,
        geometry.overlaySignalVertexCount,
      );
      ctx.restore();
    }

    ctx.restore();
  }

  dispose(): void {
    this.persistCanvas = null;
    this.persistCtx = null;
    this.persistWidth = 0;
    this.persistHeight = 0;
    this.persistNeedsClear = true;
    this.pendingDecayDtSec = 0;
  }
}
