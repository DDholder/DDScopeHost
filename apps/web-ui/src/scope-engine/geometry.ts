import {
  DDSF_M4_COLUMN_FLOATS,
  DdsfBlockKind,
} from "../bridge/ddsf-spec.ts";
import type {
  DdsfScopeView,
  DdsfTrackBlockView,
} from "../bridge/ddsf-parser.ts";
import {
  analogValueToY,
  type MsoLayout,
  type Rect,
  type ScopeTrackRenderConfig,
} from "./layout.ts";
import { rgbaNumberToFloat4 } from "./time-format.ts";

export const FLOATS_PER_VERTEX = 12;
export const BYTES_PER_VERTEX = FLOATS_PER_VERTEX * 4;

export const VERTEX_MODE_SOLID = 0;
export const VERTEX_MODE_AA_LINE = 1;
export const VERTEX_MODE_DENSE_HATCH = 2;
export const VERTEX_MODE_CAPSULE = 3;

const STATE_PALETTE: ReadonlyArray<[number, number, number, number]> = [
  [0.39, 0.45, 0.55, 0.85], // 0: IDLE (slate)
  [0.05, 0.65, 0.91, 0.88], // 1: ALIGN (sky)
  [0.06, 0.73, 0.51, 0.88], // 2: CLOSED_LOOP (emerald)
  [0.96, 0.25, 0.37, 0.94], // 3: OCP_FAULT (rose/red)
  [0.96, 0.62, 0.04, 0.88], // 4: WARN (amber)
  [0.55, 0.36, 0.96, 0.88], // 5: CUSTOM (violet)
];

export interface RibbonLabelPlacement {
  x0Css: number;
  x1Css: number;
  yCenterCss: number;
  heightCss: number;
  stateId: number;
  text: string;
}

export interface BuiltScopeGeometry {
  vertices: Float32Array;
  gridVertexCount: number;
  signalVertexCount: number;
  totalVertexCount: number;
  ribbonLabels: RibbonLabelPlacement[];
  dataModeTag: "RAW" | "M4" | "MIXED" | "IDLE";
}

function clamp(v: number, min: number, max: number): number {
  return Math.max(min, Math.min(max, v));
}

/**
 * Reusable zero-per-frame-allocation typed-array geometry builder for MSO waveforms,
 * dense PWM digital timing bands, and SDF rounded state-machine capsules.
 */
export class ScopeGeometryBuilder {
  private buffer: Float32Array;
  private floatCursor = 0;

  constructor(initialVertices = 32_768) {
    this.buffer = new Float32Array(initialVertices * FLOATS_PER_VERTEX);
  }

  private ensureFloats(additionalFloats: number): void {
    const needed = this.floatCursor + additionalFloats;
    if (needed <= this.buffer.length) return;
    let nextCap = this.buffer.length * 2;
    while (nextCap < needed) {
      nextCap *= 2;
    }
    const next = new Float32Array(nextCap);
    next.set(this.buffer.subarray(0, this.floatCursor));
    this.buffer = next;
  }

  private pushVertex(
    x: number,
    y: number,
    r: number,
    g: number,
    b: number,
    a: number,
    u0: number,
    u1: number,
    u2: number,
    u3: number,
    mode: number,
    param: number,
  ): void {
    const c = this.floatCursor;
    this.buffer[c] = x;
    this.buffer[c + 1] = y;
    this.buffer[c + 2] = r;
    this.buffer[c + 3] = g;
    this.buffer[c + 4] = b;
    this.buffer[c + 5] = a;
    this.buffer[c + 6] = u0;
    this.buffer[c + 7] = u1;
    this.buffer[c + 8] = u2;
    this.buffer[c + 9] = u3;
    this.buffer[c + 10] = mode;
    this.buffer[c + 11] = param;
    this.floatCursor = c + FLOATS_PER_VERTEX;
  }

  private pushSolidRect(
    x0: number,
    y0: number,
    x1: number,
    y1: number,
    r: number,
    g: number,
    b: number,
    a: number,
  ): void {
    this.ensureFloats(6 * FLOATS_PER_VERTEX);
    this.pushVertex(x0, y0, r, g, b, a, 0, 0, 0, 0, VERTEX_MODE_SOLID, 0);
    this.pushVertex(x1, y0, r, g, b, a, 0, 0, 0, 0, VERTEX_MODE_SOLID, 0);
    this.pushVertex(x1, y1, r, g, b, a, 0, 0, 0, 0, VERTEX_MODE_SOLID, 0);

    this.pushVertex(x0, y0, r, g, b, a, 0, 0, 0, 0, VERTEX_MODE_SOLID, 0);
    this.pushVertex(x1, y1, r, g, b, a, 0, 0, 0, 0, VERTEX_MODE_SOLID, 0);
    this.pushVertex(x0, y1, r, g, b, a, 0, 0, 0, 0, VERTEX_MODE_SOLID, 0);
  }

  private pushDenseHatchRect(
    x0: number,
    y0: number,
    x1: number,
    y1: number,
    r: number,
    g: number,
    b: number,
    a: number,
    dpr: number,
  ): void {
    this.ensureFloats(6 * FLOATS_PER_VERTEX);
    this.pushVertex(x0, y0, r, g, b, a, x0, y0, dpr, 0, VERTEX_MODE_DENSE_HATCH, dpr);
    this.pushVertex(x1, y0, r, g, b, a, x1, y0, dpr, 0, VERTEX_MODE_DENSE_HATCH, dpr);
    this.pushVertex(x1, y1, r, g, b, a, x1, y1, dpr, 0, VERTEX_MODE_DENSE_HATCH, dpr);

    this.pushVertex(x0, y0, r, g, b, a, x0, y0, dpr, 0, VERTEX_MODE_DENSE_HATCH, dpr);
    this.pushVertex(x1, y1, r, g, b, a, x1, y1, dpr, 0, VERTEX_MODE_DENSE_HATCH, dpr);
    this.pushVertex(x0, y1, r, g, b, a, x0, y1, dpr, 0, VERTEX_MODE_DENSE_HATCH, dpr);
  }

  private pushCapsuleRect(
    x0: number,
    y0: number,
    x1: number,
    y1: number,
    r: number,
    g: number,
    b: number,
    a: number,
    radiusPx: number,
  ): void {
    const hw = Math.max(1, (x1 - x0) * 0.5);
    const hh = Math.max(1, (y1 - y0) * 0.5);
    const rad = Math.min(radiusPx, hw * 0.9, hh * 0.85);
    this.ensureFloats(6 * FLOATS_PER_VERTEX);

    this.pushVertex(x0, y0, r, g, b, a, -hw, -hh, hw, hh, VERTEX_MODE_CAPSULE, rad);
    this.pushVertex(x1, y0, r, g, b, a, hw, -hh, hw, hh, VERTEX_MODE_CAPSULE, rad);
    this.pushVertex(x1, y1, r, g, b, a, hw, hh, hw, hh, VERTEX_MODE_CAPSULE, rad);

    this.pushVertex(x0, y0, r, g, b, a, -hw, -hh, hw, hh, VERTEX_MODE_CAPSULE, rad);
    this.pushVertex(x1, y1, r, g, b, a, hw, hh, hw, hh, VERTEX_MODE_CAPSULE, rad);
    this.pushVertex(x0, y1, r, g, b, a, -hw, hh, hw, hh, VERTEX_MODE_CAPSULE, rad);
  }

  private pushAaSegment(
    x0: number,
    y0: number,
    x1: number,
    y1: number,
    halfThicknessPx: number,
    r: number,
    g: number,
    b: number,
    a: number,
  ): void {
    const dx = x1 - x0;
    const dy = y1 - y0;
    const len = Math.hypot(dx, dy);
    if (len < 1e-4) {
      this.pushSolidRect(
        x0 - halfThicknessPx * 0.7,
        y0 - halfThicknessPx * 0.7,
        x0 + halfThicknessPx * 0.7,
        y0 + halfThicknessPx * 0.7,
        r,
        g,
        b,
        a,
      );
      return;
    }

    const nx = (-dy / len) * halfThicknessPx;
    const ny = (dx / len) * halfThicknessPx;

    this.ensureFloats(6 * FLOATS_PER_VERTEX);
    this.pushVertex(x0 - nx, y0 - ny, r, g, b, a, -1, 0, 0, 0, VERTEX_MODE_AA_LINE, 0);
    this.pushVertex(x1 - nx, y1 - ny, r, g, b, a, -1, 0, 0, 0, VERTEX_MODE_AA_LINE, 0);
    this.pushVertex(x1 + nx, y1 + ny, r, g, b, a, 1, 0, 0, 0, VERTEX_MODE_AA_LINE, 0);

    this.pushVertex(x0 - nx, y0 - ny, r, g, b, a, -1, 0, 0, 0, VERTEX_MODE_AA_LINE, 0);
    this.pushVertex(x1 + nx, y1 + ny, r, g, b, a, 1, 0, 0, 0, VERTEX_MODE_AA_LINE, 0);
    this.pushVertex(x0 + nx, y0 + ny, r, g, b, a, 1, 0, 0, 0, VERTEX_MODE_AA_LINE, 0);
  }

  build(
    layout: MsoLayout,
    dpr: number,
    scopeData: DdsfScopeView | null,
    trackConfigs: ReadonlyArray<ScopeTrackRenderConfig>,
  ): BuiltScopeGeometry {
    this.floatCursor = 0;
    const ribbonLabels: RibbonLabelPlacement[] = [];

    // 1. Graticule background & division grid (drawn unclipped)
    this.buildGrid(layout, dpr);
    const gridVertexCount = this.floatCursor / FLOATS_PER_VERTEX;

    let sawRaw = false;
    let sawM4 = false;

    if (scopeData) {
      const trackMap = new Map<number, DdsfTrackBlockView>();
      for (let i = 0; i < scopeData.tracks.length; i += 1) {
        trackMap.set(scopeData.tracks[i].channelId, scopeData.tracks[i]);
      }

      const subpixelShiftNorm =
        scopeData.winTSpanNs > 0 ? -scopeData.subpixelOffsetNs / scopeData.winTSpanNs : 0;

      // 2a. Analog lanes (M4 envelopes + RawXY AA lines)
      for (let i = 0; i < layout.analogSubLanes.length; i += 1) {
        const lane = layout.analogSubLanes[i];
        const block = trackMap.get(lane.track.channelId);
        if (!block) continue;

        if (block.payload.kind === DdsfBlockKind.AnalogM4Column) {
          sawM4 = true;
          this.emitM4Columns(
            block.payload.columns,
            block.elementCount,
            lane.rect,
            lane.track,
            subpixelShiftNorm,
            dpr,
          );
        } else if (block.payload.kind === DdsfBlockKind.AnalogRawXY) {
          sawRaw = true;
          this.emitRawXyLine(
            block.payload.points,
            block.elementCount,
            lane.rect,
            lane.track,
            subpixelShiftNorm,
            dpr,
          );
        }
      }

      // 2b. Digital logic lanes
      for (let i = 0; i < layout.digitalLanes.length; i += 1) {
        const lane = layout.digitalLanes[i];
        const block = trackMap.get(lane.track.channelId);
        if (!block || block.payload.kind !== DdsfBlockKind.DigitalEdges) continue;
        this.emitDigitalEdges(
          block.payload.initLevel,
          block.payload.edges,
          lane.rect,
          lane.track,
          subpixelShiftNorm,
          dpr,
        );
      }

      // 2c. State-machine ribbon lanes
      for (let i = 0; i < layout.stateLanes.length; i += 1) {
        const lane = layout.stateLanes[i];
        const block = trackMap.get(lane.track.channelId);
        if (!block || block.payload.kind !== DdsfBlockKind.StateRibbons) continue;
        this.emitStateRibbons(
          block.payload.floats,
          block.payload.stateIds,
          block.payload.colorIndices,
          block.elementCount,
          lane.rect,
          lane.track,
          subpixelShiftNorm,
          dpr,
          ribbonLabels,
        );
      }
    } else if (trackConfigs.length > 0) {
      // Nothing to draw beyond grid
    }

    const totalVertexCount = this.floatCursor / FLOATS_PER_VERTEX;
    const signalVertexCount = totalVertexCount - gridVertexCount;

    const dataModeTag: BuiltScopeGeometry["dataModeTag"] =
      sawRaw && sawM4 ? "MIXED" : sawRaw ? "RAW" : sawM4 ? "M4" : "IDLE";

    return {
      vertices: this.buffer.subarray(0, this.floatCursor),
      gridVertexCount,
      signalVertexCount,
      totalVertexCount,
      ribbonLabels,
      dataModeTag,
    };
  }

  private buildGrid(layout: MsoLayout, dpr: number): void {
    const x0 = layout.plotLeft * dpr;
    const x1 = layout.plotRight * dpr;
    const y0 = layout.plotTop * dpr;
    const y1 = layout.plotBottom * dpr;
    const w = x1 - x0;
    const h = y1 - y0;
    const lineW = Math.max(1, Math.round(dpr));

    // Plot interior dark card background
    this.pushSolidRect(x0, y0, x1, y1, 0.043, 0.062, 0.102, 0.96);

    // 10 vertical time divisions
    const divsX = 10;
    for (let i = 0; i <= divsX; i += 1) {
      const gx = x0 + (w * i) / divsX;
      const isCenter = i === 5;
      const alpha = i === 0 || i === divsX ? 0.34 : isCenter ? 0.22 : 0.11;
      this.pushSolidRect(gx - lineW * 0.5, y0, gx + lineW * 0.5, y1, 0.56, 0.67, 0.84, alpha);
    }

    // 8 horizontal divisions across analog region
    const ay0 = layout.analogRegion.y * dpr;
    const ay1 = (layout.analogRegion.y + layout.analogRegion.height) * dpr;
    const ah = ay1 - ay0;
    const divsY = 8;
    for (let j = 0; j <= divsY; j += 1) {
      const gy = ay0 + (ah * j) / divsY;
      const isCenter = j === 4;
      const alpha = j === 0 || j === divsY ? 0.32 : isCenter ? 0.22 : 0.11;
      this.pushSolidRect(x0, gy - lineW * 0.5, x1, gy + lineW * 0.5, 0.56, 0.67, 0.84, alpha);
    }

    // Stacked analog sub-lane separators
    for (let i = 1; i < layout.analogSubLanes.length; i += 1) {
      const prev = layout.analogSubLanes[i - 1].rect;
      const curr = layout.analogSubLanes[i].rect;
      if (curr.y > prev.y) {
        const sepY = ((prev.y + prev.height + curr.y) * 0.5) * dpr;
        this.pushSolidRect(x0, sepY - lineW * 0.5, x1, sepY + lineW * 0.5, 0.28, 0.38, 0.52, 0.45);
      }
    }

    // Digital & state lane subtle bands + divider lines
    for (let i = 0; i < layout.digitalLanes.length; i += 1) {
      const r = layout.digitalLanes[i].rect;
      const ly0 = r.y * dpr;
      const ly1 = (r.y + r.height) * dpr;
      if (i % 2 === 1) {
        this.pushSolidRect(x0, ly0, x1, ly1, 0.08, 0.12, 0.19, 0.4);
      }
      this.pushSolidRect(x0, ly0, x1, ly0 + lineW, 0.24, 0.33, 0.46, 0.55);
    }

    for (let i = 0; i < layout.stateLanes.length; i += 1) {
      const r = layout.stateLanes[i].rect;
      const ly0 = r.y * dpr;
      this.pushSolidRect(x0, ly0, x1, ly0 + lineW, 0.30, 0.40, 0.54, 0.65);
    }

    // Outer border bottom
    this.pushSolidRect(x0, y1 - lineW * 0.5, x1, y1 + lineW * 0.5, 0.35, 0.46, 0.62, 0.45);
    // Avoid unused variable warning on h
    if (h < 0) this.pushSolidRect(0, 0, 0, 0, 0, 0, 0, 0);
  }

  private emitM4Columns(
    columns: Float32Array,
    count: number,
    rect: Rect,
    cfg: ScopeTrackRenderConfig,
    subpixelShiftNorm: number,
    dpr: number,
  ): void {
    if (count <= 0) return;
    const [r, g, b, a] = rgbaNumberToFloat4(cfg.colorRgba);
    const plotLeftPx = rect.x * dpr;
    const plotWidthPx = rect.width * dpr;
    const colHalfW = Math.max(0.75 * dpr, (plotWidthPx / Math.max(1, count)) * 0.55);
    const aaHalfW = 1.05 * dpr;

    let prevXPx = 0;
    let prevLastYPx = 0;

    for (let i = 0; i < count; i += 1) {
      const base = i * DDSF_M4_COLUMN_FLOATS;
      const xNorm = columns[base] + subpixelShiftNorm;
      const vFirst = columns[base + 1];
      const vMin = columns[base + 2];
      const vMax = columns[base + 3];
      const vLast = columns[base + 4];

      const xPx = plotLeftPx + xNorm * plotWidthPx;
      const yFirst = analogValueToY(vFirst, rect, cfg.yScale, cfg.yOffset) * dpr;
      const yMin = analogValueToY(vMin, rect, cfg.yScale, cfg.yOffset) * dpr;
      const yMax = analogValueToY(vMax, rect, cfg.yScale, cfg.yOffset) * dpr;
      const yLast = analogValueToY(vLast, rect, cfg.yScale, cfg.yOffset) * dpr;

      const topY = Math.min(yMin, yMax) - 0.7 * dpr;
      const botY = Math.max(yMin, yMax) + 0.7 * dpr;

      // Envelope vertical bar (100% glitch preserving)
      this.pushSolidRect(xPx - colHalfW, topY, xPx + colHalfW, botY, r, g, b, a * 0.92);

      // Connect prev vLast -> curr vFirst for continuous trace readability
      if (i > 0) {
        this.pushAaSegment(prevXPx, prevLastYPx, xPx, yFirst, aaHalfW, r, g, b, a * 0.85);
      }

      prevXPx = xPx;
      prevLastYPx = yLast;
    }
  }

  private emitRawXyLine(
    points: Float32Array,
    count: number,
    rect: Rect,
    cfg: ScopeTrackRenderConfig,
    subpixelShiftNorm: number,
    dpr: number,
  ): void {
    if (count <= 0) return;
    const [r, g, b, a] = rgbaNumberToFloat4(cfg.colorRgba);
    const plotLeftPx = rect.x * dpr;
    const plotWidthPx = rect.width * dpr;
    const halfThick = 1.45 * dpr;
    const showDots = count <= 96;
    const dotHalf = 2.6 * dpr;

    let prevX = 0;
    let prevY = 0;

    for (let i = 0; i < count; i += 1) {
      const xNorm = points[i * 2] + subpixelShiftNorm;
      const yVal = points[i * 2 + 1];
      const xPx = plotLeftPx + xNorm * plotWidthPx;
      const yPx = analogValueToY(yVal, rect, cfg.yScale, cfg.yOffset) * dpr;

      if (i > 0) {
        this.pushAaSegment(prevX, prevY, xPx, yPx, halfThick, r, g, b, a);
      }
      if (showDots) {
        this.pushSolidRect(
          xPx - dotHalf,
          yPx - dotHalf,
          xPx + dotHalf,
          yPx + dotHalf,
          Math.min(1, r + 0.2),
          Math.min(1, g + 0.2),
          Math.min(1, b + 0.2),
          a,
        );
      }

      prevX = xPx;
      prevY = yPx;
    }
  }

  private emitDigitalEdges(
    initLevel: 0 | 1,
    edges: Float32Array,
    rect: Rect,
    cfg: ScopeTrackRenderConfig,
    subpixelShiftNorm: number,
    dpr: number,
  ): void {
    const [r, g, b, a] = rgbaNumberToFloat4(cfg.colorRgba);
    const plotLeftPx = rect.x * dpr;
    const plotWidthPx = rect.width * dpr;
    const plotRightPx = plotLeftPx + plotWidthPx;

    const pad = Math.max(3, rect.height * 0.18) * dpr;
    const yHigh = rect.y * dpr + pad;
    const yLow = (rect.y + rect.height) * dpr - pad;
    const strokeHalf = 0.9 * dpr;
    const denseThresholdPx = 2.0 * dpr;

    let level: 0 | 1 = initLevel;
    let prevX = plotLeftPx;

    let inBurst = false;
    let burstStartPx = 0;
    let burstEndPx = 0;

    const flushBurst = (): void => {
      if (!inBurst) return;
      if (burstEndPx - burstStartPx >= 0.8 * dpr) {
        this.pushDenseHatchRect(
          burstStartPx,
          yHigh,
          burstEndPx,
          yLow,
          r,
          g,
          b,
          a * 0.82,
          dpr,
        );
      }
      inBurst = false;
    };

    for (let i = 0; i <= edges.length; i += 1) {
      const isEnd = i === edges.length;
      const edgePx = isEnd
        ? plotRightPx
        : clamp(plotLeftPx + (edges[i] + subpixelShiftNorm) * plotWidthPx, plotLeftPx, plotRightPx);
      const segW = edgePx - prevX;

      if (!isEnd && segW < denseThresholdPx) {
        if (!inBurst) {
          inBurst = true;
          burstStartPx = prevX;
          burstEndPx = edgePx;
        } else {
          burstEndPx = Math.max(burstEndPx, edgePx);
        }
        prevX = edgePx;
        level = level === 1 ? 0 : 1;
        continue;
      }

      if (inBurst && segW < denseThresholdPx * 1.5) {
        burstEndPx = Math.max(burstEndPx, edgePx);
        flushBurst();
        prevX = edgePx;
        if (!isEnd) level = level === 1 ? 0 : 1;
        continue;
      }

      flushBurst();

      if (segW > 0.25 * dpr) {
        const yLevel = level === 1 ? yHigh : yLow;
        if (level === 1) {
          this.pushSolidRect(prevX, yHigh, edgePx, yLow, r, g, b, a * 0.17);
        }
        this.pushSolidRect(
          prevX,
          yLevel - strokeHalf,
          edgePx,
          yLevel + strokeHalf,
          r,
          g,
          b,
          a * 0.95,
        );
      }

      if (!isEnd) {
        this.pushSolidRect(
          edgePx - strokeHalf,
          yHigh - strokeHalf,
          edgePx + strokeHalf,
          yLow + strokeHalf,
          r,
          g,
          b,
          a * 0.95,
        );
        level = level === 1 ? 0 : 1;
      }

      prevX = edgePx;
    }

    flushBurst();
  }

  private emitStateRibbons(
    floats: Float32Array,
    stateIds: Int32Array,
    colorIndices: Uint32Array,
    count: number,
    rect: Rect,
    cfg: ScopeTrackRenderConfig,
    subpixelShiftNorm: number,
    dpr: number,
    labelsOut: RibbonLabelPlacement[],
  ): void {
    const plotLeftCss = rect.x;
    const plotWidthCss = rect.width;
    const padCss = Math.max(3, rect.height * 0.14);
    const y0Css = rect.y + padCss;
    const y1Css = rect.y + rect.height - padCss;
    const yCenterCss = (y0Css + y1Css) * 0.5;
    const heightCss = Math.max(8, y1Css - y0Css);

    const y0Px = y0Css * dpr;
    const y1Px = y1Css * dpr;
    const insetPx = 1.2 * dpr;
    const cornerRadiusPx = Math.min(6 * dpr, (y1Px - y0Px) * 0.42);

    const labelLookup = new Map<number, { name: string; color_rgba: number | null }>();
    for (let i = 0; i < cfg.stateLabels.length; i += 1) {
      labelLookup.set(cfg.stateLabels[i].code, cfg.stateLabels[i]);
    }

    for (let i = 0; i < count; i += 1) {
      const x0Norm = clamp(floats[i * 4] + subpixelShiftNorm, 0, 1);
      const x1Norm = clamp(floats[i * 4 + 1] + subpixelShiftNorm, 0, 1);
      if (x1Norm <= x0Norm) continue;

      const x0Css = plotLeftCss + x0Norm * plotWidthCss;
      const x1Css = plotLeftCss + x1Norm * plotWidthCss;
      const x0Px = x0Css * dpr + insetPx;
      const x1Px = x1Css * dpr - insetPx;
      if (x1Px - x0Px < 2 * dpr) continue;

      const stateId = stateIds[i * 4 + 2];
      const colorIdx = colorIndices[i * 4 + 3];
      const meta = labelLookup.get(stateId);
      const [r, g, b, a] =
        meta?.color_rgba != null
          ? rgbaNumberToFloat4(meta.color_rgba)
          : STATE_PALETTE[colorIdx % STATE_PALETTE.length];

      this.pushCapsuleRect(x0Px, y0Px, x1Px, y1Px, r, g, b, a, cornerRadiusPx);

      labelsOut.push({
        x0Css: x0Css + 2,
        x1Css: x1Css - 2,
        yCenterCss,
        heightCss,
        stateId,
        text: meta?.name ?? `S${stateId}`,
      });
    }
  }
}
