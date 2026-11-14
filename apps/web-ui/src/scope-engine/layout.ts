export type AnalogLayoutMode = "overlay" | "stacked";

export interface ScopeTrackRenderConfig {
  channelId: number;
  label: string;
  unit: string | null;
  lane: "analog" | "digital" | "state_ribbon";
  visible: boolean;
  yScale: number;
  yOffset: number;
  colorRgba: number;
  stateLabels: ReadonlyArray<{ code: number; name: string; color_rgba: number | null }>;
}

export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface MsoTrackRect {
  track: ScopeTrackRenderConfig;
  rect: Rect;
}

export interface MsoLayout {
  canvasWidth: number;
  canvasHeight: number;
  plotLeft: number;
  plotRight: number;
  plotTop: number;
  plotBottom: number;
  plotWidth: number;
  plotHeight: number;
  analogRegion: Rect;
  analogSubLanes: MsoTrackRect[];
  digitalLanes: MsoTrackRect[];
  stateLanes: MsoTrackRect[];
}

export const SCOPE_LEFT_GUTTER_PX = 60;
export const SCOPE_RIGHT_GUTTER_PX = 22;
export const SCOPE_TOP_PADDING_PX = 24;
export const SCOPE_BOTTOM_PADDING_PX = 24;

export function computeMsoLayout(
  canvasWidth: number,
  canvasHeight: number,
  tracks: ReadonlyArray<ScopeTrackRenderConfig>,
  analogLayoutMode: AnalogLayoutMode,
): MsoLayout {
  const safeW = Math.max(160, canvasWidth);
  const safeH = Math.max(140, canvasHeight);

  const plotLeft = SCOPE_LEFT_GUTTER_PX;
  const plotRight = Math.max(plotLeft + 48, safeW - SCOPE_RIGHT_GUTTER_PX);
  const plotTop = SCOPE_TOP_PADDING_PX;
  const plotBottom = Math.max(plotTop + 60, safeH - SCOPE_BOTTOM_PADDING_PX);
  const plotWidth = plotRight - plotLeft;
  const plotHeight = plotBottom - plotTop;

  const visibleTracks = tracks.filter((t) => t.visible);
  const analogTracks = visibleTracks.filter((t) => t.lane === "analog");
  const digitalTracks = visibleTracks.filter((t) => t.lane === "digital");
  const stateTracks = visibleTracks.filter((t) => t.lane === "state_ribbon");

  const nonAnalogCount = digitalTracks.length + stateTracks.length;
  const minAnalogHeight = analogTracks.length > 0 ? Math.max(64, plotHeight * 0.36) : 0;
  const maxBandBudget = Math.max(0, plotHeight - minAnalogHeight);
  const idealBandHeight = 28;
  const bandHeight =
    nonAnalogCount > 0
      ? Math.max(16, Math.min(idealBandHeight, Math.floor(maxBandBudget / nonAnalogCount)))
      : 0;
  const bandsTotalHeight = nonAnalogCount * bandHeight;

  const analogHeight = Math.max(40, plotHeight - bandsTotalHeight);
  const analogRegion: Rect = {
    x: plotLeft,
    y: plotTop,
    width: plotWidth,
    height: analogHeight,
  };

  const analogSubLanes: MsoTrackRect[] = [];
  if (analogTracks.length > 0) {
    if (analogLayoutMode === "overlay" || analogTracks.length === 1) {
      for (let i = 0; i < analogTracks.length; i += 1) {
        analogSubLanes.push({
          track: analogTracks[i],
          rect: analogRegion,
        });
      }
    } else {
      const gap = 4;
      const totalGaps = gap * (analogTracks.length - 1);
      const laneH = Math.max(20, (analogHeight - totalGaps) / analogTracks.length);
      for (let i = 0; i < analogTracks.length; i += 1) {
        analogSubLanes.push({
          track: analogTracks[i],
          rect: {
            x: plotLeft,
            y: plotTop + i * (laneH + gap),
            width: plotWidth,
            height: laneH,
          },
        });
      }
    }
  }

  let cursorY = plotTop + analogHeight;
  const digitalLanes: MsoTrackRect[] = [];
  for (let i = 0; i < digitalTracks.length; i += 1) {
    digitalLanes.push({
      track: digitalTracks[i],
      rect: {
        x: plotLeft,
        y: cursorY,
        width: plotWidth,
        height: bandHeight,
      },
    });
    cursorY += bandHeight;
  }

  const stateLanes: MsoTrackRect[] = [];
  for (let i = 0; i < stateTracks.length; i += 1) {
    stateLanes.push({
      track: stateTracks[i],
      rect: {
        x: plotLeft,
        y: cursorY,
        width: plotWidth,
        height: bandHeight,
      },
    });
    cursorY += bandHeight;
  }

  return {
    canvasWidth: safeW,
    canvasHeight: safeH,
    plotLeft,
    plotRight,
    plotTop,
    plotBottom,
    plotWidth,
    plotHeight,
    analogRegion,
    analogSubLanes,
    digitalLanes,
    stateLanes,
  };
}

export interface ScopeYAxisConfig {
  yAxisScale: number;
  yAxisOffset: number;
}

export const DEFAULT_Y_AXIS_CONFIG: ScopeYAxisConfig = {
  yAxisScale: 10,
  yAxisOffset: 0,
};

function clampLayoutValue(v: number, min: number, max: number): number {
  return Math.max(min, Math.min(max, v));
}

export function analogValueToY(
  val: number,
  rect: Rect,
  yScalePerDiv: number,
  yOffsetCenter: number,
): number {
  const safeScale = Math.max(1e-6, yScalePerDiv);
  const halfSpan = safeScale * 4; // 8 vertical divisions (+/- 4 div)
  const norm = 0.5 - (val - yOffsetCenter) / (2 * halfSpan);
  return rect.y + norm * rect.height;
}

export function yToAnalogValue(
  yPx: number,
  rect: Rect,
  yScalePerDiv: number,
  yOffsetCenter: number,
): number {
  const safeScale = Math.max(1e-6, yScalePerDiv);
  const halfSpan = safeScale * 4;
  const norm = (yPx - rect.y) / Math.max(1, rect.height);
  return yOffsetCenter + (0.5 - norm) * 2 * halfSpan;
}

/** Transforms a raw channel physical value into shared Y-axis coordinates: raw * scale + offset. */
export function channelValueToAxis(
  rawVal: number,
  channelScale: number,
  channelOffset: number,
): number {
  const safeScale = Number.isFinite(channelScale) ? channelScale : 1;
  const safeOffset = Number.isFinite(channelOffset) ? channelOffset : 0;
  return rawVal * safeScale + safeOffset;
}

/** Inverse transform from shared Y-axis coordinate value back to raw physical channel units. */
export function axisValueToChannel(
  axisVal: number,
  channelScale: number,
  channelOffset: number,
): number {
  const rawScale = Number.isFinite(channelScale) ? channelScale : 1;
  const safeScale =
    Math.abs(rawScale) < 1e-6 ? (rawScale < 0 ? -1e-6 : 1e-6) : rawScale;
  const safeOffset = Number.isFinite(channelOffset) ? channelOffset : 0;
  return (axisVal - safeOffset) / safeScale;
}

/** Maps a raw channel sample onto canvas CSS Y using per-channel scale/offset + whole-axis viewport scale/offset. */
export function channelValueToY(
  rawVal: number,
  rect: Rect,
  axis: ScopeYAxisConfig,
  channelScale: number,
  channelOffset: number,
): number {
  const axisVal = channelValueToAxis(rawVal, channelScale, channelOffset);
  return analogValueToY(axisVal, rect, axis.yAxisScale, axis.yAxisOffset);
}

/** Maps a canvas CSS Y pixel back into raw channel physical units via whole-axis + per-channel inverse transforms. */
export function yToChannelValue(
  yPx: number,
  rect: Rect,
  axis: ScopeYAxisConfig,
  channelScale: number,
  channelOffset: number,
): number {
  const axisVal = yToAnalogValue(yPx, rect, axis.yAxisScale, axis.yAxisOffset);
  return axisValueToChannel(axisVal, channelScale, channelOffset);
}

/** Zooms the entire Y coordinate axis anchored at cursor pixel `yPx` so the axis coordinate under cursor stays stationary. */
export function zoomYAxisAtCursor(
  yPx: number,
  rect: Rect,
  oldYAxisScale: number,
  oldYAxisOffset: number,
  zoomFactor: number,
): ScopeYAxisConfig {
  const safeOldScale = clampLayoutValue(
    Number.isFinite(oldYAxisScale) ? oldYAxisScale : DEFAULT_Y_AXIS_CONFIG.yAxisScale,
    1e-4,
    1e6,
  );
  const safeOldOffset = Number.isFinite(oldYAxisOffset) ? oldYAxisOffset : 0;
  const v = clampLayoutValue((yPx - rect.y) / Math.max(1, rect.height), 0, 1);
  const axisAtCursor = safeOldOffset + (0.5 - v) * 8 * safeOldScale;
  const newYAxisScale = clampLayoutValue(safeOldScale * zoomFactor, 1e-4, 1e6);
  const newYAxisOffset = axisAtCursor - (0.5 - v) * 8 * newYAxisScale;
  return {
    yAxisScale: newYAxisScale,
    yAxisOffset: newYAxisOffset,
  };
}

export interface HorizontalPanBounds {
  canPan: boolean;
  minPanNs: number;
  maxPanNs: number;
}

export interface ComputeHorizontalPanBoundsInput {
  hasScopeData: boolean;
  hasVisibleTracks: boolean;
  triggerState: number;
  totalTrackElements: number;
  sweepMode: "roll" | "auto" | "normal" | "single";
  preTriggerRatio?: number;
  paused: boolean;
  timeSpanNs: number;
  targetSpanNs?: number;
  currentPanNs: number;
  winTStartNs: number;
  winTSpanNs: number;
  hostNowNs: number;
  floorWinStartNs: number;
}

/**
 * Computes valid horizontal pan limits `[minPanNs, maxPanNs]` beyond which the
 * underlying waveform window cannot move, preventing phantom X-axis label drift.
 */
export function computeHorizontalPanBounds(
  input: ComputeHorizontalPanBoundsInput,
): HorizontalPanBounds {
  const {
    hasScopeData,
    hasVisibleTracks,
    triggerState,
    totalTrackElements,
    sweepMode,
    preTriggerRatio = 0.25,
    paused,
    timeSpanNs,
    targetSpanNs = timeSpanNs,
    currentPanNs,
    winTStartNs,
    winTSpanNs,
    hostNowNs,
    floorWinStartNs,
  } = input;

  // TriggerRuntimeStateCode.Armed === 1
  if (
    !hasScopeData ||
    !hasVisibleTracks ||
    winTSpanNs <= 0 ||
    (triggerState === 1 && totalTrackElements === 0)
  ) {
    return {
      canPan: false,
      minPanNs: currentPanNs,
      maxPanNs: currentPanNs,
    };
  }

  const curWinEndNs = winTStartNs + winTSpanNs;

  if (sweepMode === "roll" && !paused) {
    const safeHostNow = Math.max(floorWinStartNs, hostNowNs);
    const availableHistoryNs = Math.max(0, safeHostNow - floorWinStartNs);
    if (availableHistoryNs <= targetSpanNs) {
      return {
        canPan: false,
        minPanNs: 0,
        maxPanNs: 0,
      };
    }
    const minPanNs = Math.min(0, Math.round(targetSpanNs - availableHistoryNs));
    const maxPanNs = 0;
    return {
      canPan: minPanNs < maxPanNs,
      minPanNs,
      maxPanNs,
    };
  }

  // Static/latched/triggered/paused modes: span delta from current to targetSpanNs uses zeroRatio
  const zeroRatio = sweepMode === "roll" ? 1.0 : clampLayoutValue(preTriggerRatio, 0.05, 0.95);
  const spanShrinkNs = winTSpanNs - targetSpanNs;
  const availLeftNs = Math.max(0, winTStartNs - floorWinStartNs);
  const rawMinPan = Math.round(currentPanNs - availLeftNs - spanShrinkNs * zeroRatio);

  const ceilEndNs = Math.max(curWinEndNs, hostNowNs);
  const availRightNs = Math.max(0, ceilEndNs - curWinEndNs);
  const rawMaxPan =
    sweepMode === "roll" && paused
      ? Math.min(0, Math.round(currentPanNs + availRightNs))
      : Math.round(currentPanNs + availRightNs + spanShrinkNs * (1 - zeroRatio));

  if (rawMaxPan < rawMinPan) {
    const locked = sweepMode === "roll" ? 0 : currentPanNs;
    return {
      canPan: false,
      minPanNs: locked,
      maxPanNs: locked,
    };
  }

  return {
    canPan: rawMinPan < rawMaxPan,
    minPanNs: rawMinPan,
    maxPanNs: rawMaxPan,
  };
}

/** Zooms horizontal X timebase anchored at cursor pixel `xPx`, clamping pan when boundaries stop the waveform. */
export function zoomXAxisAtCursor(
  xPx: number,
  plotLeft: number,
  plotWidth: number,
  oldSpanNs: number,
  oldPanNs: number,
  zeroRatio: number,
  zoomFactor: number,
  clampBounds?: (newSpanNs: number) => HorizontalPanBounds,
): { timeSpanNs: number; panOffsetNs: number } {
  const newSpanNs = Math.round(clampLayoutValue(oldSpanNs * zoomFactor, 1_000, 600_000_000_000));
  if (newSpanNs === oldSpanNs) {
    return { timeSpanNs: oldSpanNs, panOffsetNs: oldPanNs };
  }
  const u = clampLayoutValue((xPx - plotLeft) / Math.max(1, plotWidth), 0, 1);
  const rawNewPanNs = Math.round(oldPanNs + (u - zeroRatio) * (oldSpanNs - newSpanNs));

  if (clampBounds) {
    const bounds = clampBounds(newSpanNs);
    const clampedPanNs = Math.round(clampLayoutValue(rawNewPanNs, bounds.minPanNs, bounds.maxPanNs));
    return {
      timeSpanNs: newSpanNs,
      panOffsetNs: clampedPanNs,
    };
  }

  return {
    timeSpanNs: newSpanNs,
    panOffsetNs: rawNewPanNs,
  };
}
