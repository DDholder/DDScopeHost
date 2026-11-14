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
