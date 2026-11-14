import type {
  ChannelNodeDto_Serialize,
  ScopeLaneKindDto,
  TriggerConditionDto,
  TriggerSweepModeDto,
} from "../bridge/bindings.ts";
import type { AnalogLayoutMode } from "./layout.ts";

export interface ScopeTrackUiBinding {
  channelId: number;
  lane: ScopeLaneKindDto;
  visible: boolean;
  yScale: number;
  yOffset: number;
  colorRgba: number;
}

export interface ScopeUiConfig {
  scopeId: number;
  title: string;
  timeSpanNs: number;
  preTriggerRatio: number;
  panOffsetNs: number;
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
      cursorARatio?: number;
      cursorBRatio?: number;
    },
  ) => void;
  onDropChannel: (scopeId: number, channelId: number) => void;
}

export interface ScopeChannelLookup {
  [channelId: number]: ChannelNodeDto_Serialize;
}
