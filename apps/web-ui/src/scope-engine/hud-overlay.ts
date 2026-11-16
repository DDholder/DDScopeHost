import {
  DDSF_M4_COLUMN_FLOATS,
  DdsfBlockKind,
} from "../bridge/ddsf-spec.ts";
import type {
  DdsfScopeView,
  DdsfTrackBlockView,
} from "../bridge/ddsf-parser.ts";
import type { TriggerConditionDto, TriggerSweepModeDto } from "../bridge/bindings.ts";
import { translate, type UiLocale } from "../i18n/index.ts";
import type { RenderBackendKind } from "./backend.ts";
import type { RibbonLabelPlacement } from "./geometry.ts";
import {
  DEFAULT_Y_AXIS_CONFIG,
  channelValueToY,
  yToAnalogValue,
  yToChannelValue,
  type MsoLayout,
  type Rect,
  type ScopeTrackRenderConfig,
  type ScopeYAxisConfig,
} from "./layout.ts";
import {
  formatCursorTimeNs,
  formatFrequencyHz,
  formatMetricValue,
  formatTimeNs,
  formatTimePerDiv,
  rgbaNumberToCss,
} from "./time-format.ts";

export interface ScopeRunBoundaryOverlay {
  xNorm: number;
  runNumber: number;
}

export type EvaluatedTrackSample =
  | { kind: "analog"; value: number }
  | { kind: "digital"; level: 0 | 1 }
  | { kind: "state"; stateId: number; label: string; colorRgba: number | null };

export interface ScopeHudState {
  locale?: UiLocale;
  sweepMode: TriggerSweepModeDto;
  timeSpanNs: number;
  preTriggerRatio: number;
  panOffsetNs: number;
  yAxisScale?: number;
  yAxisOffset?: number;
  trigger: TriggerConditionDto | null;
  cursorsEnabled: boolean;
  cursorARatio: number;
  cursorBRatio: number;
  activeAnalogChannelId: number | null;
  tracks: ReadonlyArray<ScopeTrackRenderConfig>;
  scopeData: DdsfScopeView | null;
  ribbonLabels: ReadonlyArray<RibbonLabelPlacement>;
  dataModeTag: "RAW" | "M4" | "MIXED" | "IDLE";
  backendKind: RenderBackendKind;
  fps: number;
  p99FrameMs: number;
  hoverHandle: HudHitTarget;
  draggingHandle: HudHitTarget;
  pointerHover?: { cssX: number; cssY: number } | null;
  runBoundaries?: ReadonlyArray<ScopeRunBoundaryOverlay>;
}

export type HudHitTarget =
  | "cursorA"
  | "cursorB"
  | "triggerLevel"
  | "preTrigger"
  | "plot"
  | "yGutter"
  | "xGutter"
  | "none";

function clamp(v: number, min: number, max: number): number {
  return Math.max(min, Math.min(max, v));
}

export function evaluateTrackSampleAtNorm(
  block: DdsfTrackBlockView | undefined,
  xNorm: number,
  track?: ScopeTrackRenderConfig,
  subpixelShiftNorm = 0,
): EvaluatedTrackSample | null {
  if (!block) return null;
  const clampedX = clamp(xNorm, 0, 1);

  if (block.payload.kind === DdsfBlockKind.AnalogRawXY) {
    const pts = block.payload.points;
    const n = block.elementCount;
    if (n <= 0) return null;
    const xAt = (i: number): number => pts[i * 2] + subpixelShiftNorm;
    if (n === 1) {
      return Math.abs(clampedX - xAt(0)) <= 0.015
        ? { kind: "analog", value: pts[1] }
        : null;
    }

    const xFirst = xAt(0);
    const xLast = xAt(n - 1);
    const edgeTol = 0.015;
    if (clampedX < xFirst - edgeTol || clampedX > xLast + edgeTol) {
      return null;
    }
    if (clampedX <= xFirst) {
      return { kind: "analog", value: pts[1] };
    }
    if (clampedX >= xLast) {
      return { kind: "analog", value: pts[(n - 1) * 2 + 1] };
    }

    let lo = 0;
    let hi = n - 1;
    while (lo + 1 < hi) {
      const mid = (lo + hi) >>> 1;
      if (xAt(mid) <= clampedX) {
        lo = mid;
      } else {
        hi = mid;
      }
    }
    const x0 = xAt(lo);
    const x1 = xAt(hi);
    const y0 = pts[lo * 2 + 1];
    const y1 = pts[hi * 2 + 1];
    const span = x1 - x0;
    if (Math.abs(span) < 1e-9) {
      return { kind: "analog", value: y1 };
    }
    const frac = clamp((clampedX - x0) / span, 0, 1);
    return { kind: "analog", value: y0 + frac * (y1 - y0) };
  }

  if (block.payload.kind === DdsfBlockKind.AnalogM4Column) {
    const cols = block.payload.columns;
    const n = block.elementCount;
    if (n <= 0) return null;
    const xAt = (i: number): number => cols[i * DDSF_M4_COLUMN_FLOATS] + subpixelShiftNorm;
    if (n === 1) {
      if (Math.abs(clampedX - xAt(0)) > 0.015) return null;
      return { kind: "analog", value: (cols[1] + cols[4]) * 0.5 };
    }

    const xFirst = xAt(0);
    const xLast = xAt(n - 1);
    const colStep = Math.abs(xLast - xFirst) / (n - 1);
    const edgeTol = Math.min(0.015, Math.max(0.005, colStep * 1.5));
    if (clampedX < xFirst - edgeTol || clampedX > xLast + edgeTol) {
      return null;
    }

    let idx: number;
    if (clampedX <= xFirst) {
      idx = 0;
    } else if (clampedX >= xLast) {
      idx = n - 1;
    } else {
      let lo = 0;
      let hi = n - 1;
      while (lo + 1 < hi) {
        const mid = (lo + hi) >>> 1;
        if (xAt(mid) <= clampedX) {
          lo = mid;
        } else {
          hi = mid;
        }
      }
      idx = Math.abs(clampedX - xAt(lo)) <= Math.abs(xAt(hi) - clampedX) ? lo : hi;
    }
    const base = idx * DDSF_M4_COLUMN_FLOATS;
    return { kind: "analog", value: (cols[base + 1] + cols[base + 4]) * 0.5 };
  }

  if (block.payload.kind === DdsfBlockKind.DigitalEdges) {
    const edges = block.payload.edges;
    let lo = 0;
    let hi = edges.length;
    while (lo < hi) {
      const mid = (lo + hi) >>> 1;
      if (edges[mid] + subpixelShiftNorm <= clampedX) {
        lo = mid + 1;
      } else {
        hi = mid;
      }
    }
    const level: 0 | 1 =
      lo % 2 === 0 ? block.payload.initLevel : block.payload.initLevel === 1 ? 0 : 1;
    return { kind: "digital", level };
  }

  if (block.payload.kind === DdsfBlockKind.StateRibbons) {
    const n = block.elementCount;
    if (n <= 0) return null;
    const floats = block.payload.floats;
    const stateIds = block.payload.stateIds;
    let lo = 0;
    let hi = n;
    while (lo < hi) {
      const mid = (lo + hi) >>> 1;
      if (floats[mid * 4 + 1] + subpixelShiftNorm < clampedX - 1e-6) {
        lo = mid + 1;
      } else {
        hi = mid;
      }
    }
    if (lo < n) {
      const x0 = floats[lo * 4] + subpixelShiftNorm;
      const x1 = floats[lo * 4 + 1] + subpixelShiftNorm;
      if (x1 > x0 && clampedX >= x0 - 1e-6 && clampedX <= x1 + 1e-6) {
        const stateId = stateIds[lo * 4 + 2];
        const meta = track?.stateLabels.find((lbl) => lbl.code === stateId);
        return {
          kind: "state",
          stateId,
          label: meta?.name ?? `S${stateId}`,
          colorRgba: meta?.color_rgba ?? null,
        };
      }
    }
    return null;
  }

  return null;
}

export function evaluateTrackValueAtNorm(
  block: DdsfTrackBlockView | undefined,
  xNorm: number,
  subpixelShiftNorm = 0,
): number | null {
  const sample = evaluateTrackSampleAtNorm(block, xNorm, undefined, subpixelShiftNorm);
  return sample?.kind === "analog" ? sample.value : null;
}

function findAnalogLaneRect(
  layout: MsoLayout,
  channelId: number | null,
): { rect: Rect; track: ScopeTrackRenderConfig } | null {
  if (layout.analogSubLanes.length === 0) return null;
  if (channelId !== null) {
    const exact = layout.analogSubLanes.find((l) => l.track.channelId === channelId);
    if (exact) return exact;
  }
  return layout.analogSubLanes[0];
}

export function hitTestHudHandle(
  cssX: number,
  cssY: number,
  layout: MsoLayout,
  state: Pick<
    ScopeHudState,
    | "sweepMode"
    | "preTriggerRatio"
    | "trigger"
    | "cursorsEnabled"
    | "cursorARatio"
    | "cursorBRatio"
    | "yAxisScale"
    | "yAxisOffset"
  >,
): HudHitTarget {
  const insideVertical = cssY >= layout.plotTop - 10 && cssY <= layout.plotBottom + 4;
  const insidePlotX = cssX >= layout.plotLeft && cssX <= layout.plotRight;
  const insidePlotY = cssY >= layout.plotTop && cssY <= layout.plotBottom;
  const axis: ScopeYAxisConfig = {
    yAxisScale: state.yAxisScale ?? DEFAULT_Y_AXIS_CONFIG.yAxisScale,
    yAxisOffset: state.yAxisOffset ?? DEFAULT_Y_AXIS_CONFIG.yAxisOffset,
  };

  if (state.cursorsEnabled && insideVertical) {
    const xA = layout.plotLeft + state.cursorARatio * layout.plotWidth;
    const xB = layout.plotLeft + state.cursorBRatio * layout.plotWidth;
    const distA = Math.abs(cssX - xA);
    const distB = Math.abs(cssX - xB);
    if (distA <= 8 || distB <= 8) {
      return distA <= distB ? "cursorA" : "cursorB";
    }
  }

  if (state.sweepMode !== "roll") {
    const t0X = layout.plotLeft + state.preTriggerRatio * layout.plotWidth;
    if (
      Math.abs(cssX - t0X) <= 9 &&
      cssY >= layout.plotTop - 14 &&
      cssY <= layout.plotTop + 14
    ) {
      return "preTrigger";
    }

    if (state.trigger && state.trigger.kind === "edge") {
      const targetLane = findAnalogLaneRect(layout, state.trigger.channel_id);
      if (targetLane) {
        const yTrig = channelValueToY(
          state.trigger.level ?? 0,
          targetLane.rect,
          axis,
          targetLane.track.yScale,
          targetLane.track.yOffset,
        );
        const onRightHandle =
          cssX >= layout.plotRight - 14 &&
          cssX <= layout.canvasWidth &&
          Math.abs(cssY - yTrig) <= 10;
        const onLine =
          insidePlotX && Math.abs(cssY - yTrig) <= 6;
        if (onRightHandle || onLine) {
          return "triggerLevel";
        }
      }
    }
  }

  if (cssX < layout.plotLeft && insidePlotY) {
    return "yGutter";
  }

  if (cssY > layout.plotBottom && insidePlotX) {
    return "xGutter";
  }

  if (insidePlotX && insidePlotY) {
    return "plot";
  }

  return "none";
}

function fitTruncatedText(
  ctx: CanvasRenderingContext2D,
  text: string,
  maxWidth: number,
): string {
  if (maxWidth <= 8) return "";
  if (ctx.measureText(text).width <= maxWidth) return text;
  const ellipsis = "…";
  const ellW = ctx.measureText(ellipsis).width;
  if (ellW >= maxWidth) return "";
  let lo = 0;
  let hi = text.length;
  while (lo < hi) {
    const mid = Math.ceil((lo + hi) / 2);
    const slice = text.slice(0, mid);
    if (ctx.measureText(slice).width + ellW <= maxWidth) {
      lo = mid;
    } else {
      hi = mid - 1;
    }
  }
  return lo > 0 ? text.slice(0, lo) + ellipsis : "";
}

function formatSampleText(
  sample: EvaluatedTrackSample | null,
  track: ScopeTrackRenderConfig,
): string {
  if (!sample) return "--";
  switch (sample.kind) {
    case "analog":
      return formatMetricValue(sample.value, track.unit);
    case "digital":
      return sample.level === 1 ? "1" : "0";
    case "state":
      return sample.label;
  }
}

function formatSampleDeltaText(
  sampleA: EvaluatedTrackSample | null,
  sampleB: EvaluatedTrackSample | null,
  track: ScopeTrackRenderConfig,
): string {
  if (!sampleA || !sampleB || sampleA.kind !== sampleB.kind) return "--";
  switch (sampleA.kind) {
    case "analog":
      return formatMetricValue(
        (sampleB as { kind: "analog"; value: number }).value - sampleA.value,
        track.unit,
      );
    case "digital": {
      const lvlB = (sampleB as { kind: "digital"; level: 0 | 1 }).level;
      return sampleA.level === lvlB ? "=" : `${sampleA.level}->${lvlB}`;
    }
    case "state": {
      const b = sampleB as { kind: "state"; stateId: number; label: string };
      return sampleA.stateId === b.stateId ? "=" : `${sampleA.label}->${b.label}`;
    }
  }
}

export function renderScopeHud(
  ctx: CanvasRenderingContext2D,
  layout: MsoLayout,
  dpr: number,
  state: ScopeHudState,
): void {
  const { canvasWidth, canvasHeight, plotLeft, plotRight, plotTop, plotBottom, plotWidth } = layout;
  if (canvasWidth < 2 || canvasHeight < 2) return;

  ctx.save();
  ctx.setTransform(1, 0, 0, 1, 0, 0);
  ctx.clearRect(0, 0, Math.round(canvasWidth * dpr), Math.round(canvasHeight * dpr));
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);

  // 1. Top telemetry & timebase header
  const locale: UiLocale = state.locale ?? "en";
  const axis: ScopeYAxisConfig = {
    yAxisScale: state.yAxisScale ?? DEFAULT_Y_AXIS_CONFIG.yAxisScale,
    yAxisOffset: state.yAxisOffset ?? DEFAULT_Y_AXIS_CONFIG.yAxisOffset,
  };
  ctx.font = '600 11px "JetBrains Mono", "Fira Code", ui-monospace, monospace';
  ctx.textBaseline = "middle";
  const topMidY = plotTop * 0.5;

  const tbText = `${formatTimePerDiv(state.timeSpanNs)} · Y ${formatMetricValue(axis.yAxisScale, null)}/div · ${translate(locale, "hud.span")} ${formatTimeNs(state.timeSpanNs)}`;
  ctx.fillStyle = "#cbd5e1";
  ctx.textAlign = "left";
  ctx.fillText(tbText, plotLeft, topMidY);

  const perfText = `${state.backendKind.toUpperCase()} · ${state.dataModeTag} · ${state.fps.toFixed(0)} fps (${state.p99FrameMs.toFixed(1)}ms)`;
  ctx.fillStyle = state.backendKind === "webgpu" ? "#38bdf8" : "#a78bfa";
  ctx.textAlign = "right";
  ctx.fillText(perfText, plotRight, topMidY);

  // 2. X-Axis Time Labels along bottom gutter
  ctx.font = '500 10px "JetBrains Mono", ui-monospace, monospace';
  ctx.fillStyle = "#94a3b8";
  ctx.textBaseline = "middle";
  const xLabelY = plotBottom + 12;
  const zeroRatio = state.sweepMode === "roll" ? 1.0 : state.preTriggerRatio;

  for (let i = 0; i <= 10; i += 2) {
    const ratio = i / 10;
    const gx = plotLeft + ratio * plotWidth;
    const relNs = (ratio - zeroRatio) * state.timeSpanNs + state.panOffsetNs;
    const label = formatTimeNs(relNs);
    ctx.textAlign = i === 0 ? "left" : i === 10 ? "right" : "center";
    ctx.fillText(label, gx, xLabelY);
  }

  // 3. Whole-Axis Y Rulers & Per-Channel Baseline Markers
  ctx.font = '500 10px "JetBrains Mono", ui-monospace, monospace';
  if (layout.analogSubLanes.length > 0) {
    const uniqueLanes = new Map<Rect, ScopeTrackRenderConfig[]>();
    for (let i = 0; i < layout.analogSubLanes.length; i += 1) {
      const entry = layout.analogSubLanes[i];
      let arr = uniqueLanes.get(entry.rect);
      if (!arr) {
        arr = [];
        uniqueLanes.set(entry.rect, arr);
      }
      arr.push(entry.track);
    }

    uniqueLanes.forEach((tracksInLane, rect) => {
      const topVal = axis.yAxisOffset + 4 * axis.yAxisScale;
      const p2Val = axis.yAxisOffset + 2 * axis.yAxisScale;
      const midVal = axis.yAxisOffset;
      const m2Val = axis.yAxisOffset - 2 * axis.yAxisScale;
      const botVal = axis.yAxisOffset - 4 * axis.yAxisScale;

      ctx.textAlign = "right";
      ctx.fillStyle = "#cbd5e1";
      ctx.fillText(formatMetricValue(topVal, null), plotLeft - 7, rect.y + 8);
      ctx.fillText(formatMetricValue(midVal, null), plotLeft - 7, rect.y + rect.height * 0.5);
      ctx.fillText(formatMetricValue(botVal, null), plotLeft - 7, rect.y + rect.height - 8);
      if (rect.height >= 120) {
        ctx.fillStyle = "#64748b";
        ctx.fillText(formatMetricValue(p2Val, null), plotLeft - 7, rect.y + rect.height * 0.25);
        ctx.fillText(formatMetricValue(m2Val, null), plotLeft - 7, rect.y + rect.height * 0.75);
      }

      // Per-channel physical-zero baseline markers on the left axis border
      for (let k = 0; k < tracksInLane.length; k += 1) {
        const ch = tracksInLane[k];
        const yZero = channelValueToY(0, rect, axis, ch.yScale, ch.yOffset);
        if (yZero >= rect.y + 2 && yZero <= rect.y + rect.height - 2) {
          ctx.fillStyle = rgbaNumberToCss(ch.colorRgba, 0.95);
          ctx.fillRect(plotLeft - 5, yZero - 1.5, 5, 3);
        }
      }
    });
  }

  for (let i = 0; i < layout.digitalLanes.length; i += 1) {
    const lane = layout.digitalLanes[i];
    const midY = lane.rect.y + lane.rect.height * 0.5;
    ctx.textAlign = "right";
    ctx.fillStyle = rgbaNumberToCss(lane.track.colorRgba, 0.95);
    const shortLabel = fitTruncatedText(ctx, lane.track.label, plotLeft - 10);
    ctx.fillText(shortLabel, plotLeft - 6, midY);
  }

  for (let i = 0; i < layout.stateLanes.length; i += 1) {
    const lane = layout.stateLanes[i];
    const midY = lane.rect.y + lane.rect.height * 0.5;
    ctx.textAlign = "right";
    ctx.fillStyle = rgbaNumberToCss(lane.track.colorRgba, 0.95);
    const shortLabel = fitTruncatedText(ctx, lane.track.label, plotLeft - 10);
    ctx.fillText(shortLabel, plotLeft - 6, midY);
  }

  // 4. Adaptive State-Machine Capsule Text Labels
  ctx.font = '700 10px Inter, -apple-system, BlinkMacSystemFont, sans-serif';
  ctx.textAlign = "center";
  ctx.textBaseline = "middle";
  for (let i = 0; i < state.ribbonLabels.length; i += 1) {
    const lbl = state.ribbonLabels[i];
    const availW = lbl.x1Css - lbl.x0Css - 8;
    if (availW < 18) continue;
    const text = fitTruncatedText(ctx, lbl.text, availW);
    if (!text) continue;
    const centerX = (lbl.x0Css + lbl.x1Css) * 0.5;
    ctx.fillStyle = "rgba(4, 9, 17, 0.72)";
    ctx.fillText(text, centerX + 0.5, lbl.yCenterCss + 0.5);
    ctx.fillStyle = "#f8fafc";
    ctx.fillText(text, centerX, lbl.yCenterCss);
  }

  // 4.5 Acquisition Run Divider Vertical Lines & Badges
  if (state.runBoundaries && state.runBoundaries.length > 0) {
    ctx.font = '700 9px "JetBrains Mono", ui-monospace, monospace';
    for (let i = 0; i < state.runBoundaries.length; i += 1) {
      const boundary = state.runBoundaries[i];
      if (boundary.xNorm < 0 || boundary.xNorm > 1) continue;
      const bx = plotLeft + boundary.xNorm * plotWidth;

      ctx.save();
      ctx.setLineDash([6, 4]);
      ctx.strokeStyle = "rgba(52, 211, 153, 0.85)";
      ctx.lineWidth = 1.3;
      ctx.beginPath();
      ctx.moveTo(bx, plotTop);
      ctx.lineTo(bx, plotBottom);
      ctx.stroke();
      ctx.restore();

      const tag = translate(locale, "hud.runDivider", { run: boundary.runNumber });
      const pillW = Math.max(44, ctx.measureText(tag).width + 10);
      const pillH = 14;
      const pillX = clamp(bx - pillW * 0.5, plotLeft + 2, plotRight - pillW - 2);
      const pillY = plotTop + 16;

      ctx.fillStyle = "rgba(6, 78, 59, 0.92)";
      ctx.fillRect(pillX, pillY, pillW, pillH);
      ctx.strokeStyle = "#34d399";
      ctx.lineWidth = 1;
      ctx.strokeRect(pillX, pillY, pillW, pillH);

      ctx.fillStyle = "#a7f3d0";
      ctx.textAlign = "center";
      ctx.textBaseline = "middle";
      ctx.fillText(tag, pillX + pillW * 0.5, pillY + pillH * 0.5 + 0.5);
    }
  }

  // 5. Trigger Line, Hysteresis Band, and Pre-Trigger T0 Anchor
  if (state.sweepMode !== "roll") {
    const t0X = plotLeft + state.preTriggerRatio * plotWidth;
    const isHoverT0 = state.hoverHandle === "preTrigger" || state.draggingHandle === "preTrigger";

    ctx.save();
    ctx.setLineDash([4, 4]);
    ctx.strokeStyle = isHoverT0 ? "rgba(251, 191, 36, 0.85)" : "rgba(251, 191, 36, 0.35)";
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.moveTo(t0X, plotTop);
    ctx.lineTo(t0X, plotBottom);
    ctx.stroke();
    ctx.restore();

    // T0 triangle handle at top edge
    ctx.fillStyle = isHoverT0 ? "#fde68a" : "#f59e0b";
    ctx.beginPath();
    ctx.moveTo(t0X - 5, plotTop - 8);
    ctx.lineTo(t0X + 5, plotTop - 8);
    ctx.lineTo(t0X, plotTop);
    ctx.closePath();
    ctx.fill();

    if (state.trigger && state.trigger.kind === "edge") {
      const targetLane = findAnalogLaneRect(layout, state.trigger.channel_id);
      if (targetLane) {
        const level = state.trigger.level ?? 0;
        const hys = Math.max(0, state.trigger.hysteresis ?? 0);
        const yTrig = channelValueToY(
          level,
          targetLane.rect,
          axis,
          targetLane.track.yScale,
          targetLane.track.yOffset,
        );
        const yHys = channelValueToY(
          level - hys,
          targetLane.rect,
          axis,
          targetLane.track.yScale,
          targetLane.track.yOffset,
        );
        const clampedTrigY = clamp(yTrig, targetLane.rect.y, targetLane.rect.y + targetLane.rect.height);
        const clampedHysY = clamp(yHys, targetLane.rect.y, targetLane.rect.y + targetLane.rect.height);

        const isHoverTrig =
          state.hoverHandle === "triggerLevel" || state.draggingHandle === "triggerLevel";

        // Hysteresis shading band
        if (Math.abs(clampedHysY - clampedTrigY) >= 1) {
          const bandTop = Math.min(clampedTrigY, clampedHysY);
          const bandH = Math.abs(clampedHysY - clampedTrigY);
          ctx.fillStyle = "rgba(245, 158, 11, 0.11)";
          ctx.fillRect(plotLeft, bandTop, plotWidth, bandH);
        }

        ctx.save();
        ctx.setLineDash([5, 4]);
        ctx.strokeStyle = isHoverTrig ? "#fde68a" : "rgba(245, 158, 11, 0.82)";
        ctx.lineWidth = isHoverTrig ? 1.6 : 1.1;
        ctx.beginPath();
        ctx.moveTo(plotLeft, clampedTrigY);
        ctx.lineTo(plotRight, clampedTrigY);
        ctx.stroke();
        ctx.restore();

        // Right-edge draggable [T] handle
        ctx.fillStyle = isHoverTrig ? "#fbbf24" : "#d97706";
        ctx.beginPath();
        ctx.moveTo(plotRight, clampedTrigY);
        ctx.lineTo(plotRight + 16, clampedTrigY - 7);
        ctx.lineTo(plotRight + 16, clampedTrigY + 7);
        ctx.closePath();
        ctx.fill();

        ctx.font = '700 9px "JetBrains Mono", monospace';
        ctx.fillStyle = "#0b0f17";
        ctx.textAlign = "center";
        ctx.fillText("T", plotRight + 11, clampedTrigY + 0.5);

        if (state.draggingHandle === "triggerLevel") {
          const tip = `${translate(locale, "hud.trig")} ${formatMetricValue(level, targetLane.track.unit)}`;
          const tipW = ctx.measureText(tip).width + 12;
          ctx.fillStyle = "rgba(15, 23, 42, 0.92)";
          ctx.fillRect(plotRight - tipW - 6, clampedTrigY - 19, tipW, 16);
          ctx.strokeStyle = "#f59e0b";
          ctx.strokeRect(plotRight - tipW - 6, clampedTrigY - 19, tipW, 16);
          ctx.fillStyle = "#fef3c7";
          ctx.textAlign = "right";
          ctx.fillText(tip, plotRight - 11, clampedTrigY - 11);
        }
      }
    }
  }

  // Shared block map and subpixel offset for cursor/hover evaluation
  const subpixelShiftNorm =
    state.scopeData && state.scopeData.winTSpanNs > 0
      ? -state.scopeData.subpixelOffsetNs / state.scopeData.winTSpanNs
      : 0;
  const trackBlockById = new Map<number, DdsfTrackBlockView>();
  if (state.scopeData) {
    for (let i = 0; i < state.scopeData.tracks.length; i += 1) {
      const blk = state.scopeData.tracks[i];
      trackBlockById.set(blk.channelId, blk);
    }
  }
  const visibleTracks = state.tracks.filter((tr) => tr.visible);

  // 6. Dual Measurement Cursors A / B & Multi-Track Floating Readout
  if (state.cursorsEnabled) {
    const ratioA = clamp(state.cursorARatio, 0, 1);
    const ratioB = clamp(state.cursorBRatio, 0, 1);
    const xA = plotLeft + ratioA * plotWidth;
    const xB = plotLeft + ratioB * plotWidth;

    const drawCursorLine = (x: number, tag: string, color: string, active: boolean) => {
      ctx.save();
      ctx.strokeStyle = color;
      ctx.lineWidth = active ? 1.8 : 1.2;
      ctx.setLineDash([4, 3]);
      ctx.beginPath();
      ctx.moveTo(x, plotTop);
      ctx.lineTo(x, plotBottom);
      ctx.stroke();
      ctx.restore();

      ctx.fillStyle = color;
      ctx.fillRect(x - 8, plotTop, 16, 13);
      ctx.font = '700 9px "JetBrains Mono", monospace';
      ctx.fillStyle = "#040911";
      ctx.textAlign = "center";
      ctx.textBaseline = "middle";
      ctx.fillText(tag, x, plotTop + 6.5);
    };

    drawCursorLine(
      xA,
      "A",
      "#fbbf24",
      state.hoverHandle === "cursorA" || state.draggingHandle === "cursorA",
    );
    drawCursorLine(
      xB,
      "B",
      "#38bdf8",
      state.hoverHandle === "cursorB" || state.draggingHandle === "cursorB",
    );

    const tA = (ratioA - zeroRatio) * state.timeSpanNs + state.panOffsetNs;
    const tB = (ratioB - zeroRatio) * state.timeSpanNs + state.panOffsetNs;
    const dt = tB - tA;
    const invDt = Math.abs(dt) > 1 ? 1e9 / Math.abs(dt) : 0;

    const evaluatedRows = visibleTracks.map((track) => {
      const blk = trackBlockById.get(track.channelId);
      const sampleA = evaluateTrackSampleAtNorm(blk, ratioA, track, subpixelShiftNorm);
      const sampleB = evaluateTrackSampleAtNorm(blk, ratioB, track, subpixelShiftNorm);
      return { track, sampleA, sampleB };
    });

    const evalByChannelId = new Map(evaluatedRows.map((row) => [row.track.channelId, row]));

    // Draw intersection markers on all visible analog lanes
    for (let i = 0; i < layout.analogSubLanes.length; i += 1) {
      const lane = layout.analogSubLanes[i];
      const row = evalByChannelId.get(lane.track.channelId);
      if (!row) continue;
      if (row.sampleA?.kind === "analog") {
        const pyA = clamp(
          channelValueToY(
            row.sampleA.value,
            lane.rect,
            axis,
            lane.track.yScale,
            lane.track.yOffset,
          ),
          lane.rect.y,
          lane.rect.y + lane.rect.height,
        );
        ctx.fillStyle = "#fbbf24";
        ctx.beginPath();
        ctx.arc(xA, pyA, 3.8, 0, Math.PI * 2);
        ctx.fill();
        ctx.strokeStyle = "#040911";
        ctx.lineWidth = 1;
        ctx.stroke();
      }
      if (row.sampleB?.kind === "analog") {
        const pyB = clamp(
          channelValueToY(
            row.sampleB.value,
            lane.rect,
            axis,
            lane.track.yScale,
            lane.track.yOffset,
          ),
          lane.rect.y,
          lane.rect.y + lane.rect.height,
        );
        ctx.fillStyle = "#38bdf8";
        ctx.beginPath();
        ctx.arc(xB, pyB, 3.8, 0, Math.PI * 2);
        ctx.fill();
        ctx.strokeStyle = "#040911";
        ctx.lineWidth = 1;
        ctx.stroke();
      }
    }

    // Draw digital transition level markers on digital lanes
    for (let i = 0; i < layout.digitalLanes.length; i += 1) {
      const lane = layout.digitalLanes[i];
      const row = evalByChannelId.get(lane.track.channelId);
      if (!row) continue;
      const pad = Math.max(3, lane.rect.height * 0.18);
      const yHigh = lane.rect.y + pad;
      const yLow = lane.rect.y + lane.rect.height - pad;
      if (row.sampleA?.kind === "digital") {
        ctx.fillStyle = "#fbbf24";
        ctx.beginPath();
        ctx.arc(xA, row.sampleA.level === 1 ? yHigh : yLow, 3.0, 0, Math.PI * 2);
        ctx.fill();
      }
      if (row.sampleB?.kind === "digital") {
        ctx.fillStyle = "#38bdf8";
        ctx.beginPath();
        ctx.arc(xB, row.sampleB.level === 1 ? yHigh : yLow, 3.0, 0, Math.PI * 2);
        ctx.fill();
      }
    }

    // Floating multi-track cursor summary card
    ctx.font = '500 10px "JetBrains Mono", ui-monospace, monospace';
    const headerA = `A ${formatCursorTimeNs(tA, state.timeSpanNs)}`;
    const headerB = `B ${formatCursorTimeNs(tB, state.timeSpanNs)}`;
    const headerTime = `${headerA}  ${headerB}`;
    const headerDelta = `Δt ${formatCursorTimeNs(dt, state.timeSpanNs)}  (${formatFrequencyHz(invDt)})`;

    const maxTrackRows = Math.max(
      0,
      Math.min(6, Math.floor((layout.plotHeight - 48) / 14)),
    );
    const orderedRows =
      state.activeAnalogChannelId != null
        ? [
            ...evaluatedRows.filter((r) => r.track.channelId === state.activeAnalogChannelId),
            ...evaluatedRows.filter((r) => r.track.channelId !== state.activeAnalogChannelId),
          ]
        : evaluatedRows;
    const shownRows = orderedRows.slice(0, maxTrackRows);
    const hiddenCount = orderedRows.length - shownRows.length;

    const rowTexts = shownRows.map(({ track, sampleA, sampleB }) => ({
      track,
      label: fitTruncatedText(ctx, track.label, 58),
      valA: formatSampleText(sampleA, track),
      valB: formatSampleText(sampleB, track),
      delta: formatSampleDeltaText(sampleA, sampleB, track),
    }));

    let contentW = Math.max(ctx.measureText(headerTime).width, ctx.measureText(headerDelta).width);
    for (let i = 0; i < rowTexts.length; i += 1) {
      const r = rowTexts[i];
      const lineStr = `   ${r.label}: A ${r.valA} | B ${r.valB} | Δ ${r.delta}`;
      contentW = Math.max(contentW, ctx.measureText(lineStr).width);
    }

    const boxW = clamp(Math.ceil(contentW) + 20, 224, Math.max(224, plotWidth - 16));
    const totalLines = 2 + rowTexts.length + (hiddenCount > 0 ? 1 : 0);
    const boxH = 12 + totalLines * 14;
    const boxX = plotRight - boxW - 10;
    const boxY = plotTop + 8;

    ctx.fillStyle = "rgba(11, 15, 23, 0.90)";
    ctx.fillRect(boxX, boxY, boxW, boxH);
    ctx.strokeStyle = "rgba(56, 189, 248, 0.45)";
    ctx.lineWidth = 1;
    ctx.strokeRect(boxX, boxY, boxW, boxH);

    ctx.textBaseline = "middle";
    ctx.textAlign = "left";
    ctx.fillStyle = "#fde68a";
    ctx.fillText(headerA, boxX + 8, boxY + 11);
    const aWidth = ctx.measureText(headerA + "  ").width;
    ctx.fillStyle = "#7dd3fc";
    ctx.fillText(headerB, boxX + 8 + aWidth, boxY + 11);

    ctx.fillStyle = "#e2e8f0";
    ctx.fillText(headerDelta, boxX + 8, boxY + 25);

    for (let i = 0; i < rowTexts.length; i += 1) {
      const r = rowTexts[i];
      const ry = boxY + 39 + i * 14;
      ctx.fillStyle = rgbaNumberToCss(r.track.colorRgba, 0.95);
      ctx.fillRect(boxX + 8, ry - 3.5, 6, 7);

      ctx.fillStyle = "#cbd5e1";
      const rowText = fitTruncatedText(
        ctx,
        `${r.label}: A ${r.valA} | B ${r.valB} | Δ ${r.delta}`,
        boxW - 24,
      );
      ctx.fillText(rowText, boxX + 18, ry);
    }

    if (hiddenCount > 0) {
      const moreY = boxY + 39 + rowTexts.length * 14;
      ctx.fillStyle = "#94a3b8";
      ctx.fillText(`+${hiddenCount} ...`, boxX + 8, moreY);
    }
  }

  // 7. Axis Hover Guide Lines (Left Y-Axis & Bottom X-Axis Gutters)
  if (state.pointerHover && state.draggingHandle === "none") {
    const { cssX, cssY } = state.pointerHover;
    const insidePlotY = cssY >= plotTop && cssY <= plotBottom;
    const insidePlotX = cssX >= plotLeft && cssX <= plotRight;

    if (state.hoverHandle === "yGutter" && insidePlotY) {
      const startX = clamp(cssX, 2, plotLeft);
      let labelText: string | null = null;

      const laneEntries = layout.analogSubLanes.filter(
        (l) => cssY >= l.rect.y && cssY <= l.rect.y + l.rect.height,
      );
      if (laneEntries.length > 0) {
        const laneRect = laneEntries[0].rect;
        const axisVal = yToAnalogValue(cssY, laneRect, axis.yAxisScale, axis.yAxisOffset);
        const targetTrack =
          laneEntries.length === 1
            ? laneEntries[0].track
            : laneEntries.find((l) => l.track.channelId === state.activeAnalogChannelId)?.track;
        if (targetTrack && (targetTrack.yScale !== 1 || targetTrack.yOffset !== 0)) {
          const chVal = yToChannelValue(
            cssY,
            laneRect,
            axis,
            targetTrack.yScale,
            targetTrack.yOffset,
          );
          labelText = `${formatMetricValue(axisVal, null)} (${targetTrack.label}: ${formatMetricValue(chVal, targetTrack.unit)})`;
        } else if (laneEntries.length === 1) {
          labelText = formatMetricValue(axisVal, laneEntries[0].track.unit);
        } else {
          labelText = formatMetricValue(axisVal, null);
        }
      } else {
        const digitalLane = layout.digitalLanes.find(
          (l) => cssY >= l.rect.y && cssY <= l.rect.y + l.rect.height,
        );
        if (digitalLane) {
          const bit = cssY < digitalLane.rect.y + digitalLane.rect.height * 0.5 ? "1" : "0";
          labelText = `${digitalLane.track.label} (${bit})`;
        } else {
          const stateLane = layout.stateLanes.find(
            (l) => cssY >= l.rect.y && cssY <= l.rect.y + l.rect.height,
          );
          if (stateLane) {
            labelText = stateLane.track.label;
          } else {
            const fallbackVal = yToAnalogValue(
              cssY,
              layout.analogRegion,
              axis.yAxisScale,
              axis.yAxisOffset,
            );
            labelText = formatMetricValue(fallbackVal, null);
          }
        }
      }

      ctx.save();
      ctx.setLineDash([5, 3]);
      ctx.strokeStyle = "#38bdf8";
      ctx.lineWidth = 1.4;
      ctx.beginPath();
      ctx.moveTo(startX, cssY);
      ctx.lineTo(plotRight, cssY);
      ctx.stroke();
      ctx.restore();

      ctx.fillStyle = "#38bdf8";
      ctx.beginPath();
      ctx.arc(startX, cssY, 3, 0, Math.PI * 2);
      ctx.fill();

      ctx.font = '600 10px "JetBrains Mono", ui-monospace, monospace';
      const badgeW = Math.ceil(ctx.measureText(labelText).width) + 12;
      const badgeH = 17;
      const badgeX = plotLeft + 8;
      const badgeY = clamp(cssY - badgeH - 2, plotTop + 2, plotBottom - badgeH - 2);
      ctx.fillStyle = "rgba(11, 15, 23, 0.94)";
      ctx.fillRect(badgeX, badgeY, badgeW, badgeH);
      ctx.strokeStyle = "#38bdf8";
      ctx.lineWidth = 1;
      ctx.strokeRect(badgeX, badgeY, badgeW, badgeH);
      ctx.fillStyle = "#e0f2fe";
      ctx.textAlign = "left";
      ctx.textBaseline = "middle";
      ctx.fillText(labelText, badgeX + 6, badgeY + badgeH * 0.5 + 0.5);
    } else if (state.hoverHandle === "xGutter" && insidePlotX) {
      const startY = clamp(cssY, plotBottom, canvasHeight - 2);
      const ratio = clamp((cssX - plotLeft) / Math.max(1, plotWidth), 0, 1);
      const relNs = (ratio - zeroRatio) * state.timeSpanNs + state.panOffsetNs;
      const timeText = formatCursorTimeNs(relNs, state.timeSpanNs);

      ctx.save();
      ctx.setLineDash([5, 3]);
      ctx.strokeStyle = "#38bdf8";
      ctx.lineWidth = 1.4;
      ctx.beginPath();
      ctx.moveTo(cssX, startY);
      ctx.lineTo(cssX, plotTop);
      ctx.stroke();
      ctx.restore();

      ctx.fillStyle = "#38bdf8";
      ctx.beginPath();
      ctx.arc(cssX, startY, 3, 0, Math.PI * 2);
      ctx.fill();

      ctx.font = '600 10px "JetBrains Mono", ui-monospace, monospace';
      const badgeW = Math.ceil(ctx.measureText(timeText).width) + 12;
      const badgeH = 17;
      const badgeX = clamp(cssX - badgeW * 0.5, plotLeft + 2, plotRight - badgeW - 2);
      const badgeY = Math.max(plotTop + 2, plotBottom - badgeH - 4);
      ctx.fillStyle = "rgba(11, 15, 23, 0.94)";
      ctx.fillRect(badgeX, badgeY, badgeW, badgeH);
      ctx.strokeStyle = "#38bdf8";
      ctx.lineWidth = 1;
      ctx.strokeRect(badgeX, badgeY, badgeW, badgeH);
      ctx.fillStyle = "#e0f2fe";
      ctx.textAlign = "center";
      ctx.textBaseline = "middle";
      ctx.fillText(timeText, badgeX + badgeW * 0.5, badgeY + badgeH * 0.5 + 0.5);
    }
  }

  ctx.restore();
}
