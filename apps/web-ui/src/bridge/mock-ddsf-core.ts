import type {
  ChannelModalityDto,
  ChannelNodeDto_Serialize,
  ChannelTreeSnapshotDto_Serialize,
  CreateSourceRequestDto_Deserialize,
  CreateSourceRequestDto_Serialize,
  DownlinkPayloadDto,
  DownlinkReceiptDto,
  RawRecorderRequestDto_Deserialize,
  ScopeTrackBindingDto_Deserialize,
  ScopeViewportConfigDto_Deserialize,
  ScopeViewportSummaryDto_Serialize,
  SeekReplayRequestDto_Deserialize,
  SendDownlinkRequestDto_Deserialize,
  SourceChannelGroupDto_Serialize,
  SourceRuntimeStateDto,
  SourceSummaryDto_Serialize,
  SourceTelemetryDto,
  StateLabelEntryDto_Serialize,
  SyncGroupUpdateDto_Deserialize,
  TriggerConditionDto,
  TriggerSweepModeDto,
  WorkspaceTelemetryEvent,
} from "./bindings.ts";
import {
  DDSF_FLAG_PINNED_ACTIVE,
  DDSF_FLAG_QUOTA_WARNING,
  DDSF_TRACK_FLAG_HAS_STATS,
  DdsfBlockKind,
  DdsfSnapshotKind,
  TriggerRuntimeStateCode,
  TriggerSweepModeCode,
} from "./ddsf-spec.ts";
import {
  DdsfFrameBuilder,
  type DdsfScopeBuildInput,
  type DdsfSnapshotBuildInput,
  type DdsfTrackBuildInput,
  type DdsfTrackStatsView,
} from "./ddsf-parser.ts";
import { normalizeSourceConfigToSerialize } from "./source-config-normalize.ts";

interface InternalChannelDef {
  channelId: number;
  sourceId: string;
  localPath: string;
  fullPath: string;
  displayName: string;
  unit: string | null;
  modality: ChannelModalityDto;
  scalarType: string | null;
  stateLabels: StateLabelEntryDto_Serialize[];
  signalRole:
    | "ia"
    | "ib"
    | "ic"
    | "v_bus"
    | "pwm_a"
    | "pwm_b"
    | "fault_pin"
    | "ctrl_state"
    | "aux_sine"
    | "aux_cos"
    | "aux_bool"
    | "aux_state";
  channelIndex: number;
}

interface InternalSourceState {
  summary: SourceSummaryDto_Serialize;
  config: CreateSourceRequestDto_Serialize;
  sampleRateHz: number;
  replayTotalBytes: number;
  replayProgressRatio: number;
  replaySpeed: number;
  replayPaused: boolean;
  replayLoop: boolean;
  isReplay: boolean;
  channels: InternalChannelDef[];
  droppedPackets: number;
}

interface InternalScopeState {
  scopeId: number;
  title: string;
  canvasWidthPx: number;
  timeSpanNs: number;
  preTriggerRatio: number;
  panOffsetNs: number;
  sweepMode: TriggerSweepModeDto;
  autoTimeoutMs: number;
  trigger: TriggerConditionDto | null;
  tracks: ScopeTrackBindingDto_Deserialize[];
  syncGroupId: number | null;
  paused: boolean;

  runtimeState: number;
  lastTriggerNs: number;
  latchedTriggerNs: number | null;
  latchedSubpixelNs: number;
  pinnedBytes: number;
  lastAutoRefreshNs: number;
  pausedSimTimeNs: number | null;
}

interface CoalescedEntry {
  sourceId: string;
  payload: DownlinkPayloadDto;
  queuedAtMs: number;
}

export interface MockStepResult {
  frameBuffer: ArrayBuffer;
  telemetry: WorkspaceTelemetryEvent;
}

const DEFAULT_STATE_LABELS: StateLabelEntryDto_Serialize[] = [
  { code: 0, name: "IDLE", color_rgba: 0x64748bff },
  { code: 1, name: "ALIGN", color_rgba: 0x0ea5e9ff },
  { code: 2, name: "CLOSED_LOOP", color_rgba: 0x10b981ff },
  { code: 3, name: "OCP_FAULT", color_rgba: 0xf43f5eff },
];

const QUOTA_LIMIT_BYTES = 2 * 1024 * 1024 * 1024; // 2 GiB
export const MOCK_BASE_HOST_NS = 1_700_000_000_000_000;
const BASE_HOST_NS = MOCK_BASE_HOST_NS;
const INITIAL_GLITCH_OFFSET_NS = 200_000_000; // 200 ms after start
const PERIODIC_GLITCH_INTERVAL_NS = 2_500_000_000; // every 2.5 s
const FAULT_PULSE_DURATION_NS = 350_000_000; // 350 ms OCP_FAULT ribbon state
const FAULT_PIN_DURATION_NS = 350_000; // 350 us logic pin high

function clamp(v: number, min: number, max: number): number {
  return Math.max(min, Math.min(max, v));
}

function sweepModeToCode(mode: TriggerSweepModeDto): number {
  switch (mode) {
    case "roll":
      return TriggerSweepModeCode.Roll;
    case "auto":
      return TriggerSweepModeCode.Auto;
    case "normal":
      return TriggerSweepModeCode.Normal;
    case "single":
      return TriggerSweepModeCode.Single;
  }
}

function fastPseudoNoise(tNs: number, seed: number): number {
  const x = Math.sin(tNs * 0.000173 + seed * 12.9898) * 43758.5453;
  return (x - Math.floor(x)) * 2 - 1;
}

/**
 * Pure TypeScript deterministic `DDSF v1` simulation core for browser Worker
 * development and headless Node unit testing.
 */
export class MockSimulationCore {
  private readonly builder = new DdsfFrameBuilder(128 * 1024);
  private readonly sources = new Map<string, InternalSourceState>();
  private readonly channelsById = new Map<number, InternalChannelDef>();
  private readonly scopes = new Map<number, InternalScopeState>();
  private readonly coalescedQueue = new Map<string, CoalescedEntry>();
  private readonly manualGlitchesNs: number[] = [];

  private nextChannelId = 101;
  private frameSeq = 0;
  private simTimeNs = 0;
  private prevSimTimeNs = 0;
  private lastDrainMs = 0;

  // Controllable motor parameters via downlink
  private motorKp = 1.0;
  private motorFreqHz = 50.0;
  private quotaUsedBytes = 68 * 1024 * 1024;
  private totalDownlinkCount = 0;
  private coalescedOverwrites = 0;
  private txDrainIntervalMs = 90;

  constructor(options?: { autoStartDefaultSim?: boolean }) {
    this.seedDefaultSimSource(options?.autoStartDefaultSim ?? true);
  }

  private buildSimPresetChannels(sourceId: string, analogCount: number): InternalChannelDef[] {
    const channels: InternalChannelDef[] = [];
    if (sourceId === "sim0" && analogCount === 4) {
      channels.push(
        this.makeChannel(sourceId, "ia", "Phase Current Ia", "A", "scalar", "f32", [], "ia", 0),
        this.makeChannel(sourceId, "ib", "Phase Current Ib", "A", "scalar", "f32", [], "ib", 1),
        this.makeChannel(sourceId, "ic", "Phase Current Ic", "A", "scalar", "f32", [], "ic", 2),
        this.makeChannel(sourceId, "v_bus", "DC Bus Voltage", "V", "scalar", "f32", [], "v_bus", 3),
      );
    } else {
      for (let i = 0; i < analogCount; i += 1) {
        channels.push(
          this.makeChannel(
            sourceId,
            `ch${i}`,
            `Channel ${i}`,
            i === 3 ? "V" : "A",
            "scalar",
            "f32",
            [],
            i === 3 ? "v_bus" : i % 2 === 0 ? "aux_sine" : "aux_cos",
            i,
          ),
        );
      }
    }

    channels.push(
      this.makeChannel(sourceId, "pwm_a", "Gate PWM A", null, "digital_bool", "bool", [], "pwm_a", analogCount),
      this.makeChannel(sourceId, "pwm_b", "Gate PWM B", null, "digital_bool", "bool", [], "pwm_b", analogCount + 1),
      this.makeChannel(
        sourceId,
        "fault_pin",
        "Hardware OCP Pin",
        null,
        "digital_bool",
        "bool",
        [],
        "fault_pin",
        analogCount + 2,
      ),
      this.makeChannel(
        sourceId,
        "ctrl_state",
        "FOC Controller State",
        null,
        "state_machine",
        "i32",
        DEFAULT_STATE_LABELS,
        "ctrl_state",
        analogCount + 3,
      ),
    );
    return channels;
  }

  private seedDefaultSimSource(autoStart: boolean): void {
    const channels = this.buildSimPresetChannels("sim0", 4);

    this.sources.set("sim0", {
      summary: {
        source_id: "sim0",
        display_name: "FOC Motor Drive Sim (2 MHz)",
        transport_kind: "sim",
        state: autoStart ? "running" : "stopped",
        raw_recording_path: null,
      },
      config: {
        source_id: "sim0",
        display_name: "FOC Motor Drive Sim (2 MHz)",
        transport: {
          kind: "sim",
          sample_rate_hz: 2_000_000,
          analog_channels: 4,
          enable_glitch: true,
          glitch_period_samples: 4_000_000,
        },
        clock: {
          mode: "tier1_hardware_counter",
          tick_hz: 2_000_000,
          counter_bits: 32,
        },
        protocol: {
          codec: "sim_preset",
        },
        auto_start: autoStart,
      },
      sampleRateHz: 2_000_000,
      replayTotalBytes: 0,
      replayProgressRatio: 0,
      replaySpeed: 1,
      replayPaused: false,
      replayLoop: true,
      isReplay: false,
      channels,
      droppedPackets: 0,
    });
  }

  private makeChannel(
    sourceId: string,
    localPath: string,
    displayName: string,
    unit: string | null,
    modality: ChannelModalityDto,
    scalarType: string | null,
    stateLabels: StateLabelEntryDto_Serialize[],
    signalRole: InternalChannelDef["signalRole"],
    channelIndex: number,
  ): InternalChannelDef {
    const channelId = this.nextChannelId;
    this.nextChannelId += 1;
    const def: InternalChannelDef = {
      channelId,
      sourceId,
      localPath,
      fullPath: `${sourceId}/${localPath}`,
      displayName,
      unit,
      modality,
      scalarType,
      stateLabels,
      signalRole,
      channelIndex,
    };
    this.channelsById.set(channelId, def);
    return def;
  }

  getNowNs(): number {
    return this.simTimeNs;
  }

  getDownlinkStats(): {
    pendingQueueDepth: number;
    totalDispatched: number;
    coalescedOverwrites: number;
    motorKp: number;
    motorFreqHz: number;
  } {
    return {
      pendingQueueDepth: this.coalescedQueue.size,
      totalDispatched: this.totalDownlinkCount,
      coalescedOverwrites: this.coalescedOverwrites,
      motorKp: this.motorKp,
      motorFreqHz: this.motorFreqHz,
    };
  }

  injectGlitch(): number {
    const targetNs = this.simTimeNs + 250_000;
    this.manualGlitchesNs.push(targetNs);
    if (this.manualGlitchesNs.length > 64) {
      this.manualGlitchesNs.shift();
    }
    return targetNs;
  }

  createSource(req: CreateSourceRequestDto_Deserialize): SourceSummaryDto_Serialize {
    if (this.sources.has(req.source_id)) {
      throw new Error(`Source '${req.source_id}' already exists`);
    }

    const transportKind = req.transport.kind;
    let sampleRateHz = 250_000;
    if (req.transport.kind === "sim") {
      sampleRateHz = req.transport.sample_rate_hz ?? 1_000_000;
    } else if (req.clock.mode === "tier2_software_pll" && req.clock.nominal_sample_rate_hz) {
      sampleRateHz = req.clock.nominal_sample_rate_hz;
    }

    const channels: InternalChannelDef[] = [];
    if (req.protocol.codec === "just_float" || req.protocol.codec === "fire_water") {
      const names =
        req.protocol.channel_names.length > 0 ? req.protocol.channel_names : ["ch0", "ch1", "ch2"];
      names.forEach((name, idx) => {
        channels.push(
          this.makeChannel(
            req.source_id,
            name,
            name.toUpperCase(),
            idx === 0 ? "V" : "A",
            "scalar",
            "f32",
            [],
            idx % 2 === 0 ? "aux_sine" : "aux_cos",
            idx,
          ),
        );
      });
    } else if (req.protocol.codec === "fixed_schema_binary") {
      req.protocol.fields.forEach((field, idx) => {
        const modality: ChannelModalityDto = field.is_state_enum
          ? "state_machine"
          : field.scalar_type === "bool"
            ? "digital_bool"
            : "scalar";
        const role: InternalChannelDef["signalRole"] =
          modality === "state_machine"
            ? "aux_state"
            : modality === "digital_bool"
              ? "aux_bool"
              : idx % 2 === 0
                ? "aux_sine"
                : "aux_cos";
        channels.push(
          this.makeChannel(
            req.source_id,
            field.name,
            field.name,
            field.unit ?? null,
            modality,
            field.scalar_type,
            modality === "state_machine" ? DEFAULT_STATE_LABELS : [],
            role,
            idx,
          ),
        );
      });
    } else {
      const analogCount =
        req.transport.kind === "sim" ? clamp(req.transport.analog_channels, 1, 16) : 4;
      channels.push(...this.buildSimPresetChannels(req.source_id, analogCount));
    }

    const summary: SourceSummaryDto_Serialize = {
      source_id: req.source_id,
      display_name: req.display_name?.trim() || `${req.source_id} (${transportKind})`,
      transport_kind: transportKind,
      state: req.auto_start ? "running" : "stopped",
      raw_recording_path: null,
    };

    this.sources.set(req.source_id, {
      summary,
      config: normalizeSourceConfigToSerialize(req),
      sampleRateHz,
      replayTotalBytes: req.transport.kind === "raw_file_replay" ? 128 * 1024 * 1024 : 0,
      replayProgressRatio: 0,
      replaySpeed: req.transport.kind === "raw_file_replay" ? (req.transport.playback_speed ?? 1) : 1,
      replayPaused: false,
      replayLoop: req.transport.kind === "raw_file_replay" ? req.transport.loop_playback : true,
      isReplay: req.transport.kind === "raw_file_replay",
      channels,
      droppedPackets: 0,
    });

    return { ...summary };
  }

  startSource(sourceId: string): SourceSummaryDto_Serialize {
    const src = this.sources.get(sourceId);
    if (!src) throw new Error(`Unknown source '${sourceId}'`);
    src.summary = { ...src.summary, state: "running" };
    return { ...src.summary };
  }

  stopSource(sourceId: string): SourceSummaryDto_Serialize {
    const src = this.sources.get(sourceId);
    if (!src) throw new Error(`Unknown source '${sourceId}'`);
    src.summary = { ...src.summary, state: "stopped" };
    return { ...src.summary };
  }

  removeSource(sourceId: string): void {
    const src = this.sources.get(sourceId);
    if (!src) return;
    for (const ch of src.channels) {
      this.channelsById.delete(ch.channelId);
    }
    this.sources.delete(sourceId);
  }

  seekReplaySource(req: SeekReplayRequestDto_Deserialize): void {
    const src = this.sources.get(req.source_id);
    if (!src) throw new Error(`Unknown source '${req.source_id}'`);
    if (req.offset_ratio != null) {
      src.replayProgressRatio = clamp(req.offset_ratio, 0, 1);
    }
    if (req.paused != null) {
      src.replayPaused = req.paused;
      src.summary = {
        ...src.summary,
        state: req.paused ? "stopped" : "running",
      };
    }
    if (req.playback_speed != null) {
      src.replaySpeed = clamp(req.playback_speed, 0.1, 20);
    }
  }

  setRawRecorder(req: RawRecorderRequestDto_Deserialize): SourceSummaryDto_Serialize {
    const src = this.sources.get(req.source_id);
    if (!src) throw new Error(`Unknown source '${req.source_id}'`);
    const path = req.enabled
      ? (req.output_path ?? `captures/${req.source_id}_${Math.floor(this.simTimeNs / 1e6)}.ddraw`)
      : null;
    src.summary = { ...src.summary, raw_recording_path: path };
    return { ...src.summary };
  }

  getChannelTree(): ChannelTreeSnapshotDto_Serialize {
    const groups: SourceChannelGroupDto_Serialize[] = [];
    for (const src of this.sources.values()) {
      const channels: ChannelNodeDto_Serialize[] = src.channels.map((c) => ({
        channel_id: c.channelId,
        source_id: c.sourceId,
        local_path: c.localPath,
        full_path: c.fullPath,
        display_name: c.displayName,
        unit: c.unit,
        modality: c.modality,
        scalar_type: c.scalarType,
        state_labels: c.stateLabels,
      }));
      groups.push({
        source: { ...src.summary },
        channels,
      });
    }

    return {
      sources: groups,
      quota_limit_bytes: QUOTA_LIMIT_BYTES,
      quota_used_bytes: this.quotaUsedBytes,
      pinned_bytes: this.totalPinnedBytes(),
    };
  }

  getSourceConfigs(): CreateSourceRequestDto_Serialize[] {
    return Array.from(this.sources.values())
      .sort((a, b) => a.summary.source_id.localeCompare(b.summary.source_id))
      .map((src) => normalizeSourceConfigToSerialize(src.config));
  }

  upsertScopeViewport(cfg: ScopeViewportConfigDto_Deserialize): ScopeViewportSummaryDto_Serialize {
    const existing = this.scopes.get(cfg.scope_id);
    const timeSpanNs = Math.max(1_000, cfg.time_span_ns ?? 20_000_000);
    const preTriggerRatio = clamp(cfg.pre_trigger_ratio ?? 0.25, 0.05, 0.95);
    const panOffsetNs = cfg.pan_offset_ns ?? 0;

    let runtimeState = existing?.runtimeState ?? TriggerRuntimeStateCode.Roll;
    let latchedTriggerNs = existing?.latchedTriggerNs ?? null;
    let latchedSubpixelNs = existing?.latchedSubpixelNs ?? 0;
    let pinnedBytes = existing?.pinnedBytes ?? 0;

    if (cfg.sweep_mode === "roll") {
      runtimeState = TriggerRuntimeStateCode.Roll;
      latchedTriggerNs = null;
      pinnedBytes = 0;
    } else if (existing?.sweepMode !== cfg.sweep_mode) {
      runtimeState = TriggerRuntimeStateCode.Armed;
      if (cfg.sweep_mode !== "single") {
        latchedTriggerNs = null;
        pinnedBytes = 0;
      }
    }

    const pausedSimTimeNs = cfg.paused
      ? (existing?.pausedSimTimeNs ?? this.simTimeNs)
      : null;

    const nextState: InternalScopeState = {
      scopeId: cfg.scope_id,
      title: cfg.title ?? `Scope ${cfg.scope_id}`,
      canvasWidthPx: clamp(Math.round(cfg.canvas_width_px || 900), 64, 4096),
      timeSpanNs,
      preTriggerRatio,
      panOffsetNs,
      sweepMode: cfg.sweep_mode,
      autoTimeoutMs: Math.max(20, cfg.auto_timeout_ms || 120),
      trigger: cfg.trigger ?? null,
      tracks: cfg.tracks.map((t) => ({ ...t })),
      syncGroupId: cfg.sync_group_id ?? null,
      paused: cfg.paused,
      runtimeState,
      lastTriggerNs: existing?.lastTriggerNs ?? this.simTimeNs,
      latchedTriggerNs,
      latchedSubpixelNs,
      pinnedBytes,
      lastAutoRefreshNs: existing?.lastAutoRefreshNs ?? this.simTimeNs,
      pausedSimTimeNs,
    };

    this.scopes.set(cfg.scope_id, nextState);

    const activeTrackCount = nextState.tracks.filter((t) => t.visible).length;
    return {
      scope_id: cfg.scope_id,
      active_track_count: activeTrackCount,
      sync_group_id: nextState.syncGroupId,
    };
  }

  removeScopeViewport(scopeId: number): void {
    this.scopes.delete(scopeId);
  }

  rearmScope(scopeId: number): void {
    const scope = this.scopes.get(scopeId);
    if (!scope) return;
    scope.paused = false;
    scope.pausedSimTimeNs = null;
    scope.latchedTriggerNs = null;
    scope.latchedSubpixelNs = 0;
    scope.pinnedBytes = 0;
    scope.runtimeState =
      scope.sweepMode === "roll"
        ? TriggerRuntimeStateCode.Roll
        : TriggerRuntimeStateCode.Armed;
    scope.panOffsetNs = 0;
  }

  updateSyncGroup(update: SyncGroupUpdateDto_Deserialize): void {
    for (const scope of this.scopes.values()) {
      if (scope.syncGroupId !== update.group_id || scope.scopeId === update.origin_scope_id) {
        continue;
      }
      if (update.time_span_ns != null && update.time_span_ns >= 1_000) {
        scope.timeSpanNs = update.time_span_ns;
      }
      if (update.center_offset_ns != null) {
        scope.panOffsetNs = update.center_offset_ns;
      }
    }
  }

  sendDownlink(req: SendDownlinkRequestDto_Deserialize, nowMs = Date.now()): DownlinkReceiptDto {
    this.totalDownlinkCount += 1;

    if (req.priority === "normal" && req.coalesce_key) {
      const compositeKey = `${req.source_id}::${req.coalesce_key}`;
      const existing = this.coalescedQueue.has(compositeKey);
      this.coalescedQueue.set(compositeKey, {
        sourceId: req.source_id,
        payload: req.payload,
        queuedAtMs: nowMs,
      });
      if (existing) {
        this.coalescedOverwrites += 1;
      }
      this.applyDownlinkEffect(req.payload);
      return {
        accepted: true,
        coalesced_existing: existing,
        result_kind: existing ? "coalesced_in_place" : "queued_coalesce",
      };
    }

    this.applyDownlinkEffect(req.payload);
    return {
      accepted: true,
      coalesced_existing: false,
      result_kind: req.priority === "high" ? "dispatched_high_priority" : "dispatched_fifo",
    };
  }

  private applyDownlinkEffect(payload: DownlinkPayloadDto): void {
    if (payload.kind === "key_value" && payload.value != null) {
      if (payload.key === "kp") {
        this.motorKp = clamp(payload.value, 0.1, 8.0);
      } else if (payload.key === "freq_hz") {
        this.motorFreqHz = clamp(payload.value, 5, 2000);
      } else if (payload.key === "inject_glitch") {
        this.injectGlitch();
      }
    } else if (payload.kind === "ascii_text") {
      const txt = payload.text.trim();
      if (txt.startsWith("kp=")) {
        const parsed = Number.parseFloat(txt.slice(3));
        if (Number.isFinite(parsed)) this.motorKp = clamp(parsed, 0.1, 8.0);
      } else if (txt.startsWith("freq=")) {
        const parsed = Number.parseFloat(txt.slice(5));
        if (Number.isFinite(parsed)) this.motorFreqHz = clamp(parsed, 5, 2000);
      } else if (txt.toUpperCase() === "GLITCH") {
        this.injectGlitch();
      }
    }
  }

  drainCoalescedQueue(nowMs = Date.now()): number {
    if (nowMs - this.lastDrainMs < this.txDrainIntervalMs) {
      return 0;
    }
    const drained = this.coalescedQueue.size;
    this.coalescedQueue.clear();
    this.lastDrainMs = nowMs;
    return drained;
  }

  /**
   * Advances simulation clock by `dtMs` and returns a complete `DDSF v1` binary frame + telemetry.
   */
  step(dtMs: number, nowMs = Date.now()): MockStepResult {
    this.drainCoalescedQueue(nowMs);

    const safeDtMs = clamp(dtMs, 1, 100);
    this.prevSimTimeNs = this.simTimeNs;
    this.simTimeNs += Math.round(safeDtMs * 1_000_000);
    this.frameSeq = (this.frameSeq + 1) >>> 0;

    // Advance replay progress
    for (const src of this.sources.values()) {
      if (src.isReplay && src.summary.state === "running" && !src.replayPaused) {
        src.replayProgressRatio += (safeDtMs / 15_000) * src.replaySpeed;
        if (src.replayProgressRatio >= 1) {
          if (src.replayLoop) {
            src.replayProgressRatio -= 1;
          } else {
            src.replayProgressRatio = 1;
            src.summary = { ...src.summary, state: "completed" };
          }
        }
      }
    }

    // Ring buffer quota growth until capped at 1.92 GiB
    const runningSources = Array.from(this.sources.values()).filter(
      (s) => s.summary.state === "running",
    );
    const activeSamplesPerSec = runningSources.reduce((acc, s) => acc + s.sampleRateHz * s.channels.length, 0);
    this.quotaUsedBytes = Math.min(
      Math.round(QUOTA_LIMIT_BYTES * 0.88),
      this.quotaUsedBytes + Math.round((activeSamplesPerSec * 4 * safeDtMs) / 1000),
    );

    const scopePackets: DdsfScopeBuildInput[] = [];
    let anyPinned = false;

    for (const scope of this.scopes.values()) {
      const pkt = this.buildScopePacket(scope);
      if (scope.pinnedBytes > 0) {
        anyPinned = true;
      }
      scopePackets.push(pkt);
    }

    const snapshots: DdsfSnapshotBuildInput[] = [];
    for (const ch of this.channelsById.values()) {
      const src = this.sources.get(ch.sourceId);
      if (!src) continue;
      const val = this.evaluateChannelAt(ch, this.simTimeNs);
      const kind =
        ch.modality === "digital_bool"
          ? DdsfSnapshotKind.Bool
          : ch.modality === "state_machine"
            ? DdsfSnapshotKind.StateEnum
            : DdsfSnapshotKind.Scalar;
      snapshots.push({
        channelNumId: ch.channelId,
        kind,
        valF64: val,
      });
    }

    let flags = 0;
    if (this.quotaUsedBytes > QUOTA_LIMIT_BYTES * 0.85) {
      flags |= DDSF_FLAG_QUOTA_WARNING;
    }
    if (anyPinned) {
      flags |= DDSF_FLAG_PINNED_ACTIVE;
    }

    const frameBuffer = this.builder.pack({
      frameSeq: this.frameSeq,
      hostNowNs: BASE_HOST_NS + this.simTimeNs,
      flags,
      scopes: scopePackets,
      snapshots,
    });

    const sourceTelemetry: SourceTelemetryDto[] = [];
    for (const src of this.sources.values()) {
      const isRunning = src.summary.state === "running";
      const sps = isRunning ? src.sampleRateHz * src.channels.length : 0;
      const bps = isRunning ? sps * 4 : 0;
      sourceTelemetry.push({
        source_id: src.summary.source_id,
        state: src.summary.state as SourceRuntimeStateDto,
        samples_per_sec: sps,
        bytes_per_sec: bps,
        dropped_packets: src.droppedPackets,
      });
    }

    const telemetry: WorkspaceTelemetryEvent = {
      timestamp_ms: nowMs,
      sources: sourceTelemetry,
      quota_used_bytes: this.quotaUsedBytes,
      quota_limit_bytes: QUOTA_LIMIT_BYTES,
      pinned_bytes: this.totalPinnedBytes(),
    };

    return {
      frameBuffer,
      telemetry,
    };
  }

  private totalPinnedBytes(): number {
    let total = 0;
    for (const scope of this.scopes.values()) {
      total += scope.pinnedBytes;
    }
    return total;
  }

  private findGlitchesInInterval(t0Ns: number, t1Ns: number): number[] {
    const out: number[] = [];
    if (t1Ns < t0Ns) return out;

    if (t1Ns >= INITIAL_GLITCH_OFFSET_NS && t0Ns <= INITIAL_GLITCH_OFFSET_NS) {
      out.push(INITIAL_GLITCH_OFFSET_NS);
    }
    if (t1Ns >= INITIAL_GLITCH_OFFSET_NS) {
      const minK = Math.max(
        1,
        Math.ceil((t0Ns - INITIAL_GLITCH_OFFSET_NS) / PERIODIC_GLITCH_INTERVAL_NS),
      );
      const maxK = Math.floor((t1Ns - INITIAL_GLITCH_OFFSET_NS) / PERIODIC_GLITCH_INTERVAL_NS);
      for (let k = minK; k <= maxK; k += 1) {
        out.push(INITIAL_GLITCH_OFFSET_NS + k * PERIODIC_GLITCH_INTERVAL_NS);
      }
    }

    for (let i = 0; i < this.manualGlitchesNs.length; i += 1) {
      const g = this.manualGlitchesNs[i];
      if (g >= t0Ns && g <= t1Ns) {
        out.push(g);
      }
    }
    return out;
  }

  private mostRecentGlitchBefore(tNs: number): number | null {
    let best: number | null = null;
    if (tNs >= INITIAL_GLITCH_OFFSET_NS) {
      const k = Math.floor((tNs - INITIAL_GLITCH_OFFSET_NS) / PERIODIC_GLITCH_INTERVAL_NS);
      best = INITIAL_GLITCH_OFFSET_NS + k * PERIODIC_GLITCH_INTERVAL_NS;
    }
    for (let i = 0; i < this.manualGlitchesNs.length; i += 1) {
      const g = this.manualGlitchesNs[i];
      if (g <= tNs && (best === null || g > best)) {
        best = g;
      }
    }
    return best;
  }

  private evaluateChannelAt(ch: InternalChannelDef, tNs: number): number {
    const tSec = Math.max(0, tNs) * 1e-9;
    const omega = 2 * Math.PI * this.motorFreqHz;
    const amp = 12.0 * this.motorKp;

    switch (ch.signalRole) {
      case "ia":
        return (
          amp * Math.sin(omega * tSec) +
          amp * 0.14 * Math.sin(5 * omega * tSec) +
          amp * 0.06 * Math.sin(7 * omega * tSec) +
          0.22 * fastPseudoNoise(tNs, 1)
        );
      case "ib":
        return (
          amp * Math.sin(omega * tSec - (2 * Math.PI) / 3) +
          amp * 0.14 * Math.sin(5 * omega * tSec + (2 * Math.PI) / 3) +
          0.22 * fastPseudoNoise(tNs, 2)
        );
      case "ic":
        return (
          amp * Math.sin(omega * tSec + (2 * Math.PI) / 3) +
          amp * 0.14 * Math.sin(5 * omega * tSec - (2 * Math.PI) / 3) +
          0.22 * fastPseudoNoise(tNs, 3)
        );
      case "v_bus": {
        const base =
          48.0 +
          0.95 * Math.sin(2 * Math.PI * 600 * tSec) +
          0.18 * fastPseudoNoise(tNs, 4);
        const glitches = this.findGlitchesInInterval(tNs - 260, tNs + 260);
        return glitches.length > 0 ? base + 80.0 : base;
      }
      case "pwm_a": {
        const pwmPeriodNs = 50_000; // 20 kHz
        const duty = clamp(0.5 + 0.36 * Math.sin(omega * tSec), 0.1, 0.9);
        const phase = (((tNs % pwmPeriodNs) + pwmPeriodNs) % pwmPeriodNs) / pwmPeriodNs;
        return phase < duty ? 1 : 0;
      }
      case "pwm_b": {
        const pwmPeriodNs = 50_000;
        const duty = clamp(0.5 - 0.36 * Math.sin(omega * tSec), 0.1, 0.9);
        const phase = (((tNs % pwmPeriodNs) + pwmPeriodNs) % pwmPeriodNs) / pwmPeriodNs;
        return phase < duty ? 1 : 0;
      }
      case "fault_pin": {
        const lastGlitch = this.mostRecentGlitchBefore(tNs);
        if (lastGlitch !== null && tNs - lastGlitch <= FAULT_PIN_DURATION_NS) {
          return 1;
        }
        return 0;
      }
      case "ctrl_state": {
        const lastGlitch = this.mostRecentGlitchBefore(tNs);
        if (lastGlitch !== null && tNs - lastGlitch <= FAULT_PULSE_DURATION_NS) {
          return 3; // OCP_FAULT
        }
        const cycleNs = 1_600_000_000;
        const mod = ((tNs % cycleNs) + cycleNs) % cycleNs;
        if (mod < 220_000_000) return 0; // IDLE
        if (mod < 480_000_000) return 1; // ALIGN
        return 2; // CLOSED_LOOP
      }
      case "aux_sine":
        return 5.0 * this.motorKp * Math.sin(omega * tSec + ch.channelIndex * 0.6);
      case "aux_cos":
        return 5.0 * this.motorKp * Math.cos(omega * tSec + ch.channelIndex * 0.6);
      case "aux_bool": {
        const periodNs = 100_000;
        const phase = (((tNs % periodNs) + periodNs) % periodNs) / periodNs;
        return phase < 0.5 ? 1 : 0;
      }
      case "aux_state": {
        const slice = Math.floor(Math.max(0, tNs) / 400_000_000);
        return slice % 4;
      }
    }
  }

  private evaluateTrigger(scope: InternalScopeState): void {
    if (scope.paused || scope.sweepMode === "roll") {
      if (scope.sweepMode === "roll") {
        scope.runtimeState = TriggerRuntimeStateCode.Roll;
      }
      return;
    }

    if (scope.sweepMode === "single" && scope.runtimeState === TriggerRuntimeStateCode.LatchedPinned) {
      return;
    }

    const trig = scope.trigger;
    let triggeredAtNs: number | null = null;
    let subpixelNs = 0;

    if (trig && trig.kind === "edge") {
      const ch = this.channelsById.get(trig.channel_id);
      if (ch) {
        const level = trig.level ?? 0;
        const hys = Math.max(0, trig.hysteresis ?? 0.25);

        if (ch.signalRole === "v_bus" && level > 55) {
          const glitches = this.findGlitchesInInterval(this.prevSimTimeNs, this.simTimeNs);
          if (glitches.length > 0) {
            triggeredAtNs = glitches[0];
            subpixelNs = ((triggeredAtNs % 500) - 250) * 0.4;
          }
        } else {
          const steps = 64;
          const span = Math.max(1_000, this.simTimeNs - this.prevSimTimeNs);
          const stepNs = span / steps;
          let prevVal = this.evaluateChannelAt(ch, this.prevSimTimeNs);
          let armedBelow = prevVal <= level - hys;
          let armedAbove = prevVal >= level + hys;

          for (let i = 1; i <= steps; i += 1) {
            const t1 = this.prevSimTimeNs + i * stepNs;
            const val = this.evaluateChannelAt(ch, t1);
            if (trig.slope === "rising" || trig.slope === "either") {
              if (armedBelow && prevVal < level && val >= level && Math.abs(val - prevVal) > 1e-6) {
                const frac = (level - prevVal) / (val - prevVal);
                triggeredAtNs = t1 - stepNs + frac * stepNs;
                subpixelNs = (frac - 0.5) * 400;
                break;
              }
            }
            if (trig.slope === "falling" || trig.slope === "either") {
              if (armedAbove && prevVal > level && val <= level && Math.abs(val - prevVal) > 1e-6) {
                const frac = (prevVal - level) / (prevVal - val);
                triggeredAtNs = t1 - stepNs + frac * stepNs;
                subpixelNs = (frac - 0.5) * 400;
                break;
              }
            }
            if (val <= level - hys) armedBelow = true;
            if (val >= level + hys) armedAbove = true;
            prevVal = val;
          }
        }
      }
    } else if (trig && trig.kind === "state_equals") {
      const ch = this.channelsById.get(trig.channel_id);
      if (ch) {
        const s0 = Math.round(this.evaluateChannelAt(ch, this.prevSimTimeNs));
        const s1 = Math.round(this.evaluateChannelAt(ch, this.simTimeNs));
        if (s0 !== trig.state_code && s1 === trig.state_code) {
          triggeredAtNs = this.simTimeNs;
        }
      }
    }

    if (triggeredAtNs !== null) {
      scope.lastTriggerNs = triggeredAtNs;
      scope.latchedSubpixelNs = subpixelNs;
      scope.lastAutoRefreshNs = this.simTimeNs;
      if (scope.sweepMode === "single") {
        scope.runtimeState = TriggerRuntimeStateCode.LatchedPinned;
        scope.latchedTriggerNs = triggeredAtNs;
        scope.pinnedBytes = 12 * 1024 * 1024 + scope.tracks.length * 1024 * 1024;
      } else {
        scope.runtimeState = TriggerRuntimeStateCode.Triggered;
      }
      return;
    }

    if (scope.sweepMode === "auto") {
      const elapsedSinceLastNs = this.simTimeNs - scope.lastAutoRefreshNs;
      if (elapsedSinceLastNs >= scope.autoTimeoutMs * 1_000_000) {
        scope.runtimeState = TriggerRuntimeStateCode.AutoFreerun;
        scope.lastTriggerNs = this.simTimeNs - Math.round(scope.timeSpanNs * (1 - scope.preTriggerRatio));
        scope.lastAutoRefreshNs = this.simTimeNs;
      } else if (scope.runtimeState !== TriggerRuntimeStateCode.AutoFreerun) {
        scope.runtimeState = TriggerRuntimeStateCode.Armed;
      }
    } else {
      scope.runtimeState = TriggerRuntimeStateCode.Armed;
    }
  }

  private buildScopePacket(scope: InternalScopeState): DdsfScopeBuildInput {
    this.evaluateTrigger(scope);

    const effectiveSimNs = scope.paused
      ? (scope.pausedSimTimeNs ?? this.simTimeNs)
      : this.simTimeNs;

    let winTStartNs: number;
    if (scope.sweepMode === "roll") {
      const maxWinStart = Math.max(0, effectiveSimNs - scope.timeSpanNs);
      winTStartNs = Math.round(
        clamp(effectiveSimNs - scope.timeSpanNs + scope.panOffsetNs, 0, maxWinStart),
      );
    } else if (scope.latchedTriggerNs !== null) {
      const baseStart = Math.max(
        0,
        Math.round(scope.latchedTriggerNs - scope.preTriggerRatio * scope.timeSpanNs),
      );
      const maxWinStart = Math.max(baseStart, effectiveSimNs - scope.timeSpanNs);
      winTStartNs = Math.round(
        clamp(
          scope.latchedTriggerNs - scope.preTriggerRatio * scope.timeSpanNs + scope.panOffsetNs,
          0,
          maxWinStart,
        ),
      );
    } else {
      const anchor = scope.lastTriggerNs || effectiveSimNs;
      const baseStart = Math.max(
        0,
        Math.round(anchor - scope.preTriggerRatio * scope.timeSpanNs),
      );
      const maxWinStart = Math.max(baseStart, effectiveSimNs - scope.timeSpanNs);
      winTStartNs = Math.round(
        clamp(anchor - scope.preTriggerRatio * scope.timeSpanNs + scope.panOffsetNs, 0, maxWinStart),
      );
    }

    const winTSpanNs = scope.timeSpanNs;
    const winTEndNs = winTStartNs + winTSpanNs;

    const tracks: DdsfTrackBuildInput[] = [];
    for (let i = 0; i < scope.tracks.length; i += 1) {
      const binding = scope.tracks[i];
      if (!binding.visible) continue;
      const ch = this.channelsById.get(binding.channel_id);
      if (!ch) continue;
      const src = this.sources.get(ch.sourceId);
      const sampleRateHz = src?.sampleRateHz ?? 2_000_000;

      if (binding.lane === "analog" || ch.modality === "scalar") {
        tracks.push(this.extractAnalogTrack(ch, sampleRateHz, winTStartNs, winTSpanNs, scope.canvasWidthPx));
      } else if (binding.lane === "digital" || ch.modality === "digital_bool") {
        tracks.push(this.extractDigitalTrack(ch, winTStartNs, winTEndNs));
      } else if (binding.lane === "state_ribbon" || ch.modality === "state_machine") {
        tracks.push(this.extractStateRibbonTrack(ch, winTStartNs, winTEndNs));
      }
    }

    return {
      scopeId: scope.scopeId,
      triggerState: scope.runtimeState,
      mode: sweepModeToCode(scope.sweepMode),
      winTStartNs: BASE_HOST_NS + winTStartNs,
      winTSpanNs,
      subpixelOffsetNs: scope.latchedSubpixelNs,
      tracks,
    };
  }

  private extractAnalogTrack(
    ch: InternalChannelDef,
    sampleRateHz: number,
    winTStartNs: number,
    winTSpanNs: number,
    canvasWidthPx: number,
  ): DdsfTrackBuildInput {
    const dtNs = Math.max(100, Math.round(1e9 / sampleRateHz));
    const totalSamplesInWindow = Math.max(2, Math.floor(winTSpanNs / dtNs));
    const useRawXy = totalSamplesInWindow <= Math.min(4096, 2 * canvasWidthPx);

    if (useRawXy) {
      const count = totalSamplesInWindow;
      const points = new Float32Array(count * 2);
      const firstSampleIdx = Math.ceil(winTStartNs / dtNs);
      let min = Infinity;
      let max = -Infinity;
      let sum = 0;
      let sqSum = 0;

      for (let i = 0; i < count; i += 1) {
        const sampleT = (firstSampleIdx + i) * dtNs;
        const xNorm = clamp((sampleT - winTStartNs) / winTSpanNs, 0, 1);
        const y = this.evaluateChannelAt(ch, sampleT);
        points[i * 2] = xNorm;
        points[i * 2 + 1] = y;
        if (y < min) min = y;
        if (y > max) max = y;
        sum += y;
        sqSum += y * y;
      }

      const mean = sum / count;
      const rms = Math.sqrt(sqSum / count);
      const stats: DdsfTrackStatsView = {
        min,
        max,
        vpp: max - min,
        mean,
        rms,
        freqHz: ch.signalRole === "v_bus" ? 600 : this.motorFreqHz,
        duty: 0.5,
      };

      return {
        channelId: ch.channelId,
        blockKind: DdsfBlockKind.AnalogRawXY,
        trackFlags: DDSF_TRACK_FLAG_HAS_STATS,
        stats,
        points,
      };
    }

    // M4 envelope column extraction — mathematically preserves 1-sample glitches inside bucket [t0, t1]!
    const colCount = clamp(Math.min(canvasWidthPx, 2048), 64, 2048);
    const columns = new Float32Array(colCount * 5);
    const colSpanNs = winTSpanNs / colCount;
    const isBus = ch.signalRole === "v_bus";
    const glitches = isBus ? this.findGlitchesInInterval(winTStartNs, winTStartNs + winTSpanNs) : [];
    let glitchCursor = 0;

    let globalMin = Infinity;
    let globalMax = -Infinity;
    let sum = 0;
    let sqSum = 0;

    for (let c = 0; c < colCount; c += 1) {
      const cT0 = winTStartNs + c * colSpanNs;
      const cT1 = cT0 + colSpanNs;
      const xNorm = (c + 0.5) / colCount;

      const vFirst = this.evaluateChannelAt(ch, cT0);
      const vMid1 = this.evaluateChannelAt(ch, cT0 + colSpanNs * 0.25);
      const vMid2 = this.evaluateChannelAt(ch, cT0 + colSpanNs * 0.5);
      const vMid3 = this.evaluateChannelAt(ch, cT0 + colSpanNs * 0.75);
      const vLast = this.evaluateChannelAt(ch, cT1);

      let vMin = Math.min(vFirst, vMid1, vMid2, vMid3, vLast);
      let vMax = Math.max(vFirst, vMid1, vMid2, vMid3, vLast);

      while (glitchCursor < glitches.length && glitches[glitchCursor] < cT0) {
        glitchCursor += 1;
      }
      if (glitchCursor < glitches.length && glitches[glitchCursor] <= cT1) {
        const glitchPeak = this.evaluateChannelAt(ch, glitches[glitchCursor]);
        if (glitchPeak > vMax) vMax = glitchPeak;
      }

      const base = c * 5;
      columns[base] = xNorm;
      columns[base + 1] = vFirst;
      columns[base + 2] = vMin;
      columns[base + 3] = vMax;
      columns[base + 4] = vLast;

      if (vMin < globalMin) globalMin = vMin;
      if (vMax > globalMax) globalMax = vMax;
      const avg = (vFirst + vMid2 + vLast) / 3;
      sum += avg;
      sqSum += avg * avg;
    }

    const stats: DdsfTrackStatsView = {
      min: globalMin,
      max: globalMax,
      vpp: globalMax - globalMin,
      mean: sum / colCount,
      rms: Math.sqrt(sqSum / colCount),
      freqHz: isBus ? 600 : this.motorFreqHz,
      duty: 0.5,
    };

    return {
      channelId: ch.channelId,
      blockKind: DdsfBlockKind.AnalogM4Column,
      trackFlags: DDSF_TRACK_FLAG_HAS_STATS,
      stats,
      columns,
    };
  }

  private extractDigitalTrack(
    ch: InternalChannelDef,
    winTStartNs: number,
    winTEndNs: number,
  ): DdsfTrackBuildInput {
    const winSpan = Math.max(1, winTEndNs - winTStartNs);
    const edges: number[] = [];
    let initLevel: 0 | 1 = this.evaluateChannelAt(ch, winTStartNs) > 0.5 ? 1 : 0;
    let duty = 0.5;
    let freqHz = 0;

    if (ch.signalRole === "fault_pin") {
      const glitches = this.findGlitchesInInterval(winTStartNs - FAULT_PIN_DURATION_NS, winTEndNs);
      for (let i = 0; i < glitches.length; i += 1) {
        const gStart = glitches[i];
        const gEnd = gStart + FAULT_PIN_DURATION_NS;
        if (gStart > winTStartNs && gStart < winTEndNs) {
          edges.push((gStart - winTStartNs) / winSpan);
        }
        if (gEnd > winTStartNs && gEnd < winTEndNs) {
          edges.push((gEnd - winTStartNs) / winSpan);
        }
      }
      freqHz = glitches.length > 0 ? 1e9 / PERIODIC_GLITCH_INTERVAL_NS : 0;
      duty = glitches.length > 0 ? Math.min(1, (glitches.length * FAULT_PIN_DURATION_NS) / winSpan) : 0;
    } else {
      const periodNs = ch.signalRole === "aux_bool" ? 100_000 : 50_000;
      const firstCycle = Math.floor(winTStartNs / periodNs);
      const lastCycle = Math.ceil(winTEndNs / periodNs);
      const maxEdges = 2400;
      const totalCycles = Math.max(1, lastCycle - firstCycle);
      const stride = Math.max(1, Math.ceil((totalCycles * 2) / maxEdges));
      const omega = 2 * Math.PI * this.motorFreqHz;
      let dutySum = 0;
      let dutyCount = 0;

      for (let cyc = firstCycle; cyc <= lastCycle; cyc += stride) {
        const cycStartNs = cyc * periodNs;
        const d =
          ch.signalRole === "aux_bool"
            ? 0.5
            : ch.signalRole === "pwm_a"
              ? clamp(0.5 + 0.36 * Math.sin(omega * (cycStartNs * 1e-9)), 0.1, 0.9)
              : clamp(0.5 - 0.36 * Math.sin(omega * (cycStartNs * 1e-9)), 0.1, 0.9);
        dutySum += d;
        dutyCount += 1;

        const riseNs = cycStartNs;
        const fallNs = cycStartNs + d * periodNs;
        if (riseNs > winTStartNs && riseNs < winTEndNs) {
          edges.push((riseNs - winTStartNs) / winSpan);
        }
        if (fallNs > winTStartNs && fallNs < winTEndNs) {
          edges.push((fallNs - winTStartNs) / winSpan);
        }
      }
      freqHz = 1e9 / periodNs;
      duty = dutyCount > 0 ? dutySum / dutyCount : 0.5;
    }

    edges.sort((a, b) => a - b);

    return {
      channelId: ch.channelId,
      blockKind: DdsfBlockKind.DigitalEdges,
      digitalInitLevel: initLevel,
      trackFlags: DDSF_TRACK_FLAG_HAS_STATS,
      stats: {
        min: 0,
        max: 1,
        vpp: 1,
        mean: duty,
        rms: Math.sqrt(duty),
        freqHz,
        duty,
      },
      edges,
    };
  }

  private extractStateRibbonTrack(
    ch: InternalChannelDef,
    winTStartNs: number,
    winTEndNs: number,
  ): DdsfTrackBuildInput {
    const winSpan = Math.max(1, winTEndNs - winTStartNs);
    const segments: Array<{ x0Norm: number; x1Norm: number; stateId: number; colorIdx: number }> = [];

    if (ch.signalRole === "ctrl_state") {
      // Sample boundary checkpoints + glitch OCP intervals for exact state segmentation
      const checkpoints = new Set<number>([winTStartNs, winTEndNs]);
      const cycleNs = 1_600_000_000;
      const firstCyc = Math.floor(winTStartNs / cycleNs);
      const lastCyc = Math.ceil(winTEndNs / cycleNs);
      for (let c = firstCyc; c <= lastCyc; c += 1) {
        const base = c * cycleNs;
        for (const offset of [0, 220_000_000, 480_000_000]) {
          const t = base + offset;
          if (t > winTStartNs && t < winTEndNs) checkpoints.add(t);
        }
      }
      const glitches = this.findGlitchesInInterval(winTStartNs - FAULT_PULSE_DURATION_NS, winTEndNs);
      for (let i = 0; i < glitches.length; i += 1) {
        const t0 = glitches[i];
        const t1 = t0 + FAULT_PULSE_DURATION_NS;
        if (t0 > winTStartNs && t0 < winTEndNs) checkpoints.add(t0);
        if (t1 > winTStartNs && t1 < winTEndNs) checkpoints.add(t1);
      }

      const sorted = Array.from(checkpoints).sort((a, b) => a - b);
      for (let i = 0; i < sorted.length - 1; i += 1) {
        const tA = sorted[i];
        const tB = sorted[i + 1];
        if (tB <= tA) continue;
        const mid = (tA + tB) * 0.5;
        const stateId = Math.round(this.evaluateChannelAt(ch, mid));
        const x0Norm = clamp((tA - winTStartNs) / winSpan, 0, 1);
        const x1Norm = clamp((tB - winTStartNs) / winSpan, 0, 1);
        if (segments.length > 0 && segments[segments.length - 1].stateId === stateId) {
          segments[segments.length - 1].x1Norm = x1Norm;
        } else {
          segments.push({
            x0Norm,
            x1Norm,
            stateId,
            colorIdx: Math.max(0, stateId),
          });
        }
      }
    } else {
      const stepNs = 400_000_000;
      const first = Math.floor(winTStartNs / stepNs) * stepNs;
      for (let t = first; t < winTEndNs; t += stepNs) {
        const tA = Math.max(t, winTStartNs);
        const tB = Math.min(t + stepNs, winTEndNs);
        if (tB <= tA) continue;
        const stateId = Math.round(this.evaluateChannelAt(ch, (tA + tB) * 0.5));
        segments.push({
          x0Norm: (tA - winTStartNs) / winSpan,
          x1Norm: (tB - winTStartNs) / winSpan,
          stateId,
          colorIdx: Math.max(0, stateId),
        });
      }
    }

    return {
      channelId: ch.channelId,
      blockKind: DdsfBlockKind.StateRibbons,
      ribbons: segments,
    };
  }
}
