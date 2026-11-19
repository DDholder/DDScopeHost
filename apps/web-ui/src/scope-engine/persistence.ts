import type { TriggerSweepModeDto } from "../bridge/bindings.ts";
import type { DdsfScopeView } from "../bridge/ddsf-parser.ts";
import { DDSF_M4_COLUMN_FLOATS, DdsfBlockKind } from "../bridge/ddsf-spec.ts";
import type { AnalogLayoutMode, Rect, ScopeTrackRenderConfig } from "./layout.ts";

export type PersistenceMode = "off" | "short" | "medium" | "long" | "infinite";
export type PersistenceColorMode = "phosphor" | "thermal";

export const DEFAULT_PERSISTENCE_MODE: PersistenceMode = "off";
export const DEFAULT_PERSISTENCE_COLOR_MODE: PersistenceColorMode = "phosphor";

export const PERSISTENCE_MODES = [
  "off",
  "short",
  "medium",
  "long",
  "infinite",
] as const satisfies ReadonlyArray<PersistenceMode>;

export const PERSISTENCE_COLOR_MODES = [
  "phosphor",
  "thermal",
] as const satisfies ReadonlyArray<PersistenceColorMode>;

export interface PersistenceRenderState {
  mode: PersistenceMode;
  colorMode: PersistenceColorMode;
  decayFactor: number;
  dtSec: number;
  tauSec: number;
  depositCurrentFrame: boolean;
  clear: boolean;
}

export interface PersistenceViewKeyTrackInput {
  channelId: number;
  lane: ScopeTrackRenderConfig["lane"];
  visible: boolean;
  yScale: number;
  yOffset: number;
}

export interface PersistenceViewKeyInput {
  physicalWidth: number;
  physicalHeight: number;
  analogRegion: Rect;
  timeSpanNs: number;
  panOffsetNs: number;
  preTriggerRatio: number;
  yAxisScale: number;
  yAxisOffset: number;
  analogLayout: AnalogLayoutMode;
  sweepMode: TriggerSweepModeDto;
  tracks: ReadonlyArray<PersistenceViewKeyTrackInput>;
  clearToken?: number;
}

export interface ThermalLutStop {
  readonly t: number;
  readonly rgb: readonly [number, number, number];
}

/**
 * 6-stop scientific DSO thermal hit-density palette:
 * Cobalt Blue (#1d4ed8) -> Cyan (#06b6d4) -> Green (#22c55e)
 * -> Yellow (#facc15) -> Red (#ef4444) -> White-Hot (#fff7ed).
 */
export const THERMAL_LUT_STOPS: ReadonlyArray<ThermalLutStop> = [
  { t: 0.0, rgb: [0.114, 0.306, 0.847] },
  { t: 0.25, rgb: [0.024, 0.714, 0.831] },
  { t: 0.5, rgb: [0.133, 0.773, 0.369] },
  { t: 0.75, rgb: [0.98, 0.8, 0.082] },
  { t: 0.9, rgb: [0.937, 0.267, 0.267] },
  { t: 1.0, rgb: [1.0, 0.969, 0.929] },
];

export function isPersistenceMode(v: unknown): v is PersistenceMode {
  return (
    v === "off" ||
    v === "short" ||
    v === "medium" ||
    v === "long" ||
    v === "infinite"
  );
}

export function isPersistenceColorMode(v: unknown): v is PersistenceColorMode {
  return v === "phosphor" || v === "thermal";
}

export function getPersistenceTauSec(mode: PersistenceMode): number {
  switch (mode) {
    case "off":
      return 0;
    case "short":
      return 0.28;
    case "medium":
      return 1.0;
    case "long":
      return 2.6;
    case "infinite":
      return Infinity;
  }
}

export function getPersistenceSettleDurationSec(mode: PersistenceMode): number {
  if (mode === "off") return 0;
  if (mode === "infinite") return Infinity;
  return 5.5 * getPersistenceTauSec(mode);
}

export function computePersistenceDecayFactor(
  mode: PersistenceMode,
  dtSec: number,
  frozen: boolean,
): number {
  if (mode === "off") return 0;
  if (mode === "infinite" || frozen) return 1;
  if (!Number.isFinite(dtSec) || dtSec <= 0) return 1;
  const tau = getPersistenceTauSec(mode);
  if (!Number.isFinite(tau) || tau <= 0) return 0;
  return Math.exp(-dtSec / tau);
}

export function buildPersistenceViewKey(input: PersistenceViewKeyInput): string {
  const analogTrackKey = input.tracks
    .filter((tr) => tr.visible && tr.lane === "analog")
    .map((tr) => `${tr.channelId}:${tr.yScale}:${tr.yOffset}`)
    .join(",");

  const r = input.analogRegion;
  return [
    Math.round(input.physicalWidth),
    Math.round(input.physicalHeight),
    r.x.toFixed(2),
    r.y.toFixed(2),
    r.width.toFixed(2),
    r.height.toFixed(2),
    Math.round(input.timeSpanNs),
    Math.round(input.panOffsetNs),
    input.preTriggerRatio.toFixed(4),
    input.yAxisScale,
    input.yAxisOffset,
    input.analogLayout,
    input.sweepMode,
    analogTrackKey,
    input.clearToken ?? 0,
  ].join("|");
}

export function buildAcquisitionSignature(scopeData: DdsfScopeView | null): string | null {
  if (!scopeData) return null;

  const trackSigs: string[] = [];
  for (let i = 0; i < scopeData.tracks.length; i += 1) {
    const blk = scopeData.tracks[i];
    if (blk.payload.kind === DdsfBlockKind.AnalogRawXY) {
      const n = blk.elementCount;
      const pts = blk.payload.points;
      const lastX = n > 0 ? pts[(n - 1) * 2] : 0;
      const lastY = n > 0 ? pts[(n - 1) * 2 + 1] : 0;
      trackSigs.push(
        `${blk.channelId}:raw:${n}:${lastX.toFixed(5)}:${lastY.toFixed(5)}`,
      );
    } else if (blk.payload.kind === DdsfBlockKind.AnalogM4Column) {
      const n = blk.elementCount;
      const cols = blk.payload.columns;
      const base = n > 0 ? (n - 1) * DDSF_M4_COLUMN_FLOATS : 0;
      const lastX = n > 0 ? cols[base] : 0;
      const lastY = n > 0 ? cols[base + 4] : 0;
      trackSigs.push(
        `${blk.channelId}:m4:${n}:${lastX.toFixed(5)}:${lastY.toFixed(5)}`,
      );
    }
  }

  return [
    scopeData.winTStartNs,
    scopeData.winTSpanNs,
    scopeData.subpixelOffsetNs.toFixed(3),
    scopeData.triggerState,
    trackSigs.join(","),
  ].join("|");
}

export function sampleThermalPaletteRgb(t: number): [number, number, number] {
  const clamped = Number.isFinite(t) ? Math.max(0, Math.min(1, t)) : 0;
  for (let i = 0; i < THERMAL_LUT_STOPS.length - 1; i += 1) {
    const a = THERMAL_LUT_STOPS[i];
    const b = THERMAL_LUT_STOPS[i + 1];
    if (clamped <= b.t) {
      const span = b.t - a.t;
      const u = span > 0 ? (clamped - a.t) / span : 0;
      return [
        a.rgb[0] + (b.rgb[0] - a.rgb[0]) * u,
        a.rgb[1] + (b.rgb[1] - a.rgb[1]) * u,
        a.rgb[2] + (b.rgb[2] - a.rgb[2]) * u,
      ];
    }
  }
  const last = THERMAL_LUT_STOPS[THERMAL_LUT_STOPS.length - 1].rgb;
  return [last[0], last[1], last[2]];
}

export function thermalPaletteCss(t: number, alpha = 1): string {
  const [r, g, b] = sampleThermalPaletteRgb(t);
  return `rgba(${Math.round(r * 255)}, ${Math.round(g * 255)}, ${Math.round(b * 255)}, ${alpha})`;
}
