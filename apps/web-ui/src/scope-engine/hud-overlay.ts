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
  analogValueToY,
  type MsoLayout,
  type Rect,
  type ScopeTrackRenderConfig,
} from "./layout.ts";
import {
  formatFrequencyHz,
  formatMetricValue,
  formatTimeNs,
  formatTimePerDiv,
  rgbaNumberToCss,
} from "./time-format.ts";

export interface ScopeHudState {
  locale?: UiLocale;
  sweepMode: TriggerSweepModeDto;
  timeSpanNs: number;
  preTriggerRatio: number;
  panOffsetNs: number;
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
}

export type HudHitTarget =
  | "cursorA"
  | "cursorB"
  | "triggerLevel"
  | "preTrigger"
  | "plot"
  | "yGutter"
  | "none";

function clamp(v: number, min: number, max: number): number {
  return Math.max(min, Math.min(max, v));
}

export function evaluateTrackValueAtNorm(
  block: DdsfTrackBlockView | undefined,
  xNorm: number,
): number | null {
  if (!block || block.elementCount === 0) return null;
  const clampedX = clamp(xNorm, 0, 1);

  if (block.payload.kind === DdsfBlockKind.AnalogRawXY) {
    const pts = block.payload.points;
    const n = block.elementCount;
    if (n === 1) return pts[1];
    for (let i = 1; i < n; i += 1) {
      const x0 = pts[(i - 1) * 2];
      const x1 = pts[i * 2];
      if (clampedX <= x1 || i === n - 1) {
        const y0 = pts[(i - 1) * 2 + 1];
        const y1 = pts[i * 2 + 1];
        const span = x1 - x0;
        if (Math.abs(span) < 1e-9) return y1;
        const frac = clamp((clampedX - x0) / span, 0, 1);
        return y0 + frac * (y1 - y0);
      }
    }
    return pts[(n - 1) * 2 + 1];
  }

  if (block.payload.kind === DdsfBlockKind.AnalogM4Column) {
    const cols = block.payload.columns;
    const n = block.elementCount;
    const idx = clamp(Math.round(clampedX * (n - 1)), 0, n - 1);
    const base = idx * DDSF_M4_COLUMN_FLOATS;
    return (cols[base + 1] + cols[base + 4]) * 0.5;
  }

  return null;
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
    "sweepMode" | "preTriggerRatio" | "trigger" | "cursorsEnabled" | "cursorARatio" | "cursorBRatio"
  >,
): HudHitTarget {
  const insideVertical = cssY >= layout.plotTop - 10 && cssY <= layout.plotBottom + 4;
  const insidePlotX = cssX >= layout.plotLeft && cssX <= layout.plotRight;
  const insidePlotY = cssY >= layout.plotTop && cssY <= layout.plotBottom;

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
        const yTrig = analogValueToY(
          state.trigger.level ?? 0,
          targetLane.rect,
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
  ctx.font = '600 11px "JetBrains Mono", "Fira Code", ui-monospace, monospace';
  ctx.textBaseline = "middle";
  const topMidY = plotTop * 0.5;

  const tbText = `${formatTimePerDiv(state.timeSpanNs)} · ${translate(locale, "hud.span")} ${formatTimeNs(state.timeSpanNs)}`;
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

  // 3. Y-Axis Rulers & Gutter Lane Labels
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
      const primary =
        tracksInLane.find((t) => t.channelId === state.activeAnalogChannelId) ??
        tracksInLane[0];
      const topVal = primary.yOffset + 4 * primary.yScale;
      const midVal = primary.yOffset;
      const botVal = primary.yOffset - 4 * primary.yScale;

      ctx.textAlign = "right";
      ctx.fillStyle = rgbaNumberToCss(primary.colorRgba, 0.95);
      ctx.fillText(formatMetricValue(topVal, null), plotLeft - 6, rect.y + 8);
      ctx.fillText(formatMetricValue(midVal, null), plotLeft - 6, rect.y + rect.height * 0.5);
      ctx.fillText(formatMetricValue(botVal, null), plotLeft - 6, rect.y + rect.height - 8);

      // Zero-reference ticks for overlaid channels
      if (tracksInLane.length > 1) {
        for (let k = 0; k < tracksInLane.length; k += 1) {
          const ch = tracksInLane[k];
          const yCenter = analogValueToY(ch.yOffset, rect, ch.yScale, ch.yOffset);
          if (yCenter >= rect.y + 4 && yCenter <= rect.y + rect.height - 4) {
            ctx.fillStyle = rgbaNumberToCss(ch.colorRgba, 0.95);
            ctx.fillRect(plotLeft - 4, yCenter - 1, 4, 2);
          }
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
    ctx.fillStyle = "#e2e8f0";
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
        const yTrig = analogValueToY(
          level,
          targetLane.rect,
          targetLane.track.yScale,
          targetLane.track.yOffset,
        );
        const yHys = analogValueToY(
          level - hys,
          targetLane.rect,
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

  // 6. Dual Measurement Cursors A / B & Floating Readout
  if (state.cursorsEnabled) {
    const xA = plotLeft + clamp(state.cursorARatio, 0, 1) * plotWidth;
    const xB = plotLeft + clamp(state.cursorBRatio, 0, 1) * plotWidth;
    const activeLane = findAnalogLaneRect(layout, state.activeAnalogChannelId);

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

    const tA = (state.cursorARatio - zeroRatio) * state.timeSpanNs + state.panOffsetNs;
    const tB = (state.cursorBRatio - zeroRatio) * state.timeSpanNs + state.panOffsetNs;
    const dt = tB - tA;
    const invDt = Math.abs(dt) > 1 ? 1e9 / Math.abs(dt) : 0;

    let yA: number | null = null;
    let yB: number | null = null;
    if (activeLane && state.scopeData) {
      const blk = state.scopeData.tracks.find(
        (t) => t.channelId === activeLane.track.channelId,
      );
      yA = evaluateTrackValueAtNorm(blk, state.cursorARatio);
      yB = evaluateTrackValueAtNorm(blk, state.cursorBRatio);
      if (yA !== null) {
        const pyA = clamp(
          analogValueToY(yA, activeLane.rect, activeLane.track.yScale, activeLane.track.yOffset),
          activeLane.rect.y,
          activeLane.rect.y + activeLane.rect.height,
        );
        ctx.fillStyle = "#fbbf24";
        ctx.beginPath();
        ctx.arc(xA, pyA, 3.8, 0, Math.PI * 2);
        ctx.fill();
      }
      if (yB !== null) {
        const pyB = clamp(
          analogValueToY(yB, activeLane.rect, activeLane.track.yScale, activeLane.track.yOffset),
          activeLane.rect.y,
          activeLane.rect.y + activeLane.rect.height,
        );
        ctx.fillStyle = "#38bdf8";
        ctx.beginPath();
        ctx.arc(xB, pyB, 3.8, 0, Math.PI * 2);
        ctx.fill();
      }
    }

    // Floating cursor summary card
    const unit = activeLane?.track.unit ?? "";
    const lines = [
      `A: ${formatTimeNs(tA)}  ${yA !== null ? formatMetricValue(yA, unit) : "--"}`,
      `B: ${formatTimeNs(tB)}  ${yB !== null ? formatMetricValue(yB, unit) : "--"}`,
      `Δt: ${formatTimeNs(dt)}  (${formatFrequencyHz(invDt)})`,
      `Δy: ${yA !== null && yB !== null ? formatMetricValue(yB - yA, unit) : "--"}`,
    ];

    const boxW = 212;
    const boxH = 66;
    const boxX = plotRight - boxW - 10;
    const boxY = plotTop + 8;

    ctx.fillStyle = "rgba(11, 15, 23, 0.88)";
    ctx.fillRect(boxX, boxY, boxW, boxH);
    ctx.strokeStyle = "rgba(56, 189, 248, 0.45)";
    ctx.lineWidth = 1;
    ctx.strokeRect(boxX, boxY, boxW, boxH);

    ctx.font = '500 10px "JetBrains Mono", ui-monospace, monospace';
    ctx.textAlign = "left";
    for (let i = 0; i < lines.length; i += 1) {
      ctx.fillStyle = i === 0 ? "#fde68a" : i === 1 ? "#7dd3fc" : "#e2e8f0";
      ctx.fillText(lines[i], boxX + 8, boxY + 11 + i * 14);
    }
  }

  ctx.restore();
}
