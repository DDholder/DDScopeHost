import type {
  ChannelNodeDto_Serialize,
  ScopeLaneKindDto,
  TriggerConditionDto,
  TriggerSweepModeDto,
} from "../bridge/bindings.ts";
import type { AnalogLayoutMode } from "./layout.ts";
import type { PersistenceColorMode, PersistenceMode } from "./persistence.ts";

export type WheelZoomTarget = "x" | "y" | "xy";

export interface ScopeTrackUiBinding {
  channelId: number;
  lane: ScopeLaneKindDto;
  visible: boolean;
  /** Per-channel vertical gain multiplier (1.0 = identity) on analog tracks. */
  yScale: number;
  /** Per-channel vertical bias offset in coordinate axis units on analog tracks. */
  yOffset: number;
  colorRgba: number;
}

export interface ScopeUiConfig {
  scopeId: number;
  title: string;
  timeSpanNs: number;
  preTriggerRatio: number;
  panOffsetNs: number;
  /** Coordinate Y-axis scale in axis units per division (8 vertical divisions). */
  yAxisScale: number;
  /** Coordinate Y-axis center value in axis units. */
  yAxisOffset: number;
  /** Wheel zoom target inside the plot area when no modifier key is held. */
  wheelZoomTarget?: WheelZoomTarget;
  /** Hardware DSO-style waveform persistence/afterglow decay preset. */
  persistenceMode?: PersistenceMode;
  /** Persistence color grading mode: per-channel phosphor or hit-density thermal LUT. */
  persistenceColorMode?: PersistenceColorMode;
  sweepMode: TriggerSweepModeDto;
  autoTimeoutMs: number;
  trigger: TriggerConditionDto | null;
  tracks: ScopeTrackUiBinding[];
  syncGroupId: number | null;
  paused: boolean;
  analogLayout: AnalogLayoutMode;
  cursorsEnabled: boolean;
  cursorARatio: number;
  cursorBRatio: number;
  activeAnalogChannelId: number | null;
}

export interface ScopeCallbacks {
  onUpdateScope: (scopeId: number, updater: (prev: ScopeUiConfig) => ScopeUiConfig) => void;
  onRearmScope: (scopeId: number) => void;
  onSyncGroupBroadcast: (
    groupId: number,
    originScopeId: number,
    patch: {
      timeSpanNs?: number;
      panOffsetNs?: number;
      cursorsEnabled?: boolean;
      cursorARatio?: number;
      cursorBRatio?: number;
    },
  ) => void;
  onDropChannel: (scopeId: number, channelId: number) => void;
}

export interface ScopeChannelLookup {
  [channelId: number]: ChannelNodeDto_Serialize;
}
