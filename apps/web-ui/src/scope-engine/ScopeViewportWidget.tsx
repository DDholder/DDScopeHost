import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type DragEvent,
} from "react";
import {
  type EdgeSlopeDto,
  type TriggerSweepModeDto,
} from "../bridge/bindings.ts";
import { bridgeClient } from "../bridge/control-client.ts";
import type { DdsfScopeView } from "../bridge/ddsf-parser.ts";
import { TriggerRuntimeStateCode } from "../bridge/ddsf-spec.ts";
import type { RenderBackendKind, ScopeRenderBackend } from "./backend.ts";
import { createScopeRenderBackend } from "./create-backend.ts";
import { ScopeGeometryBuilder } from "./geometry.ts";
import {
  hitTestHudHandle,
  renderScopeHud,
  type HudHitTarget,
} from "./hud-overlay.ts";
import {
  computeMsoLayout,
  yToAnalogValue,
  type ScopeTrackRenderConfig,
} from "./layout.ts";
import {
  formatFrequencyHz,
  formatMetricValue,
  formatTimeNs,
  rgbaNumberToCss,
} from "./time-format.ts";
import type {
  ScopeCallbacks,
  ScopeChannelLookup,
  ScopeUiConfig,
} from "./types.ts";

export interface ScopeViewportWidgetProps {
  config: ScopeUiConfig;
  channelsById: ScopeChannelLookup;
  callbacks: ScopeCallbacks;
  isFocused?: boolean;
  onFocusScope?: (scopeId: number) => void;
  onCanvasWidthChanged?: (scopeId: number, widthPx: number) => void;
}

const SWEEP_MODES: ReadonlyArray<{ id: TriggerSweepModeDto; label: string }> = [
  { id: "roll", label: "Roll" },
  { id: "auto", label: "Auto" },
  { id: "normal", label: "Normal" },
  { id: "single", label: "Single" },
];

const SYNC_GROUP_OPTIONS: ReadonlyArray<{ id: number | null; label: string }> = [
  { id: null, label: "Sync: Off" },
  { id: 1, label: "Sync: Group A" },
  { id: 2, label: "Sync: Group B" },
];

function triggerStateLabel(code: number): {
  text: string;
  tone: "roll" | "armed" | "auto" | "trig" | "latched";
} {
  switch (code) {
    case TriggerRuntimeStateCode.Roll:
      return { text: "ROLL", tone: "roll" };
    case TriggerRuntimeStateCode.Armed:
      return { text: "ARMED", tone: "armed" };
    case TriggerRuntimeStateCode.AutoFreerun:
      return { text: "AUTO", tone: "auto" };
    case TriggerRuntimeStateCode.Triggered:
      return { text: "TRIG'D", tone: "trig" };
    case TriggerRuntimeStateCode.LatchedPinned:
      return { text: "LATCHED · PINNED", tone: "latched" };
    default:
      return { text: "IDLE", tone: "roll" };
  }
}

function cursorForHit(hit: HudHitTarget): string {
  switch (hit) {
    case "cursorA":
    case "cursorB":
    case "preTrigger":
      return "ew-resize";
    case "triggerLevel":
      return "ns-resize";
    case "plot":
      return "grab";
    case "yGutter":
      return "ns-resize";
    default:
      return "default";
  }
}

function clamp(v: number, min: number, max: number): number {
  return Math.max(min, Math.min(max, v));
}

export function ScopeViewportWidget({
  config,
  channelsById,
  callbacks,
  isFocused = false,
  onFocusScope,
  onCanvasWidthChanged,
}: ScopeViewportWidgetProps) {
  const stageRef = useRef<HTMLDivElement | null>(null);
  const gpuCanvasRef = useRef<HTMLCanvasElement | null>(null);
  const hudCanvasRef = useRef<HTMLCanvasElement | null>(null);

  const measVppRef = useRef<HTMLSpanElement | null>(null);
  const measVmaxRef = useRef<HTMLSpanElement | null>(null);
  const measVminRef = useRef<HTMLSpanElement | null>(null);
  const measVrmsRef = useRef<HTMLSpanElement | null>(null);
  const measMeanRef = useRef<HTMLSpanElement | null>(null);
  const measFreqRef = useRef<HTMLSpanElement | null>(null);
  const measDutyRef = useRef<HTMLSpanElement | null>(null);
  const measChLabelRef = useRef<HTMLSpanElement | null>(null);

  const [runtimeTriggerState, setRuntimeTriggerState] = useState<number>(
    config.sweepMode === "roll"
      ? TriggerRuntimeStateCode.Roll
      : TriggerRuntimeStateCode.Armed,
  );
  const [backendKind, setBackendKind] = useState<RenderBackendKind>("webgl2");
  const [dropActive, setDropActive] = useState(false);

  const configRef = useRef(config);
  configRef.current = config;

  const channelsRef = useRef(channelsById);
  channelsRef.current = channelsById;

  const callbacksRef = useRef(callbacks);
  callbacksRef.current = callbacks;

  const pendingScopeRef = useRef<DdsfScopeView | null>(
    bridgeClient.topicBus.getLatestScopeFrame(config.scopeId)?.scope ?? null,
  );
  const dirtyRef = useRef(true);
  const backendRef = useRef<ScopeRenderBackend | null>(null);
  const backendKindRef = useRef<RenderBackendKind>("webgl2");
  const geometryRef = useRef(new ScopeGeometryBuilder(32_768));

  const sizeRef = useRef<{ cssW: number; cssH: number; dpr: number }>({
    cssW: 0,
    cssH: 0,
    dpr: 1,
  });

  const hoverHandleRef = useRef<HudHitTarget>("none");
  const dragStateRef = useRef<{
    target: HudHitTarget;
    startCssX: number;
    startCssY: number;
    initialPanNs: number;
  }>({
    target: "none",
    startCssX: 0,
    startCssY: 0,
    initialPanNs: 0,
  });

  // Track render frame timing for FPS & 99% frame-time telemetry
  const perfStatsRef = useRef<{
    fps: number;
    p99Ms: number;
    frameCount: number;
    lastReportMs: number;
    frameTimes: number[];
    lastMeasUpdateMs: number;
    lastTriggerCode: number;
  }>({
    fps: 0,
    p99Ms: 0,
    frameCount: 0,
    lastReportMs: performance.now(),
    frameTimes: [],
    lastMeasUpdateMs: 0,
    lastTriggerCode: -1,
  });

  const trackRenderConfigs = useMemo<ScopeTrackRenderConfig[]>(() => {
    return config.tracks.map((t) => {
      const ch = channelsById[t.channelId];
      return {
        channelId: t.channelId,
        label: ch ? ch.full_path : `ch${t.channelId}`,
        unit: ch?.unit ?? null,
        lane: t.lane,
        visible: t.visible,
        yScale: t.yScale,
        yOffset: t.yOffset,
        colorRgba: t.colorRgba,
        stateLabels: ch?.state_labels ?? [],
      };
    });
  }, [config.tracks, channelsById]);

  const trackConfigsRef = useRef(trackRenderConfigs);
  trackConfigsRef.current = trackRenderConfigs;

  useEffect(() => {
    dirtyRef.current = true;
  }, [config, trackRenderConfigs]);

  // Subscribe to zero-copy DDSF frames for this scope without triggering React re-renders
  useEffect(() => {
    const unsub = bridgeClient.topicBus.subscribeScope(config.scopeId, (scopeView) => {
      pendingScopeRef.current = scopeView;
      dirtyRef.current = true;

      if (scopeView.triggerState !== perfStatsRef.current.lastTriggerCode) {
        perfStatsRef.current.lastTriggerCode = scopeView.triggerState;
        setRuntimeTriggerState(scopeView.triggerState);
      }
    });
    return unsub;
  }, [config.scopeId]);

  // Initialize GPU backend, ResizeObserver, and requestAnimationFrame loop
  useEffect(() => {
    let cancelled = false;
    let rafId = 0;
    const gpuCanvas = gpuCanvasRef.current;
    const hudCanvas = hudCanvasRef.current;
    const stageEl = stageRef.current;
    if (!gpuCanvas || !hudCanvas || !stageEl) return;

    const hudCtx = hudCanvas.getContext("2d");

    const updateSizes = (widthCss: number, heightCss: number) => {
      const dpr = Math.min(2, Math.max(1, window.devicePixelRatio || 1));
      const w = Math.floor(widthCss);
      const h = Math.floor(heightCss);
      sizeRef.current = { cssW: w, cssH: h, dpr };

      if (w >= 2 && h >= 2) {
        const physW = Math.round(w * dpr);
        const physH = Math.round(h * dpr);
        if (gpuCanvas.width !== physW || gpuCanvas.height !== physH) {
          gpuCanvas.width = physW;
          gpuCanvas.height = physH;
        }
        if (hudCanvas.width !== physW || hudCanvas.height !== physH) {
          hudCanvas.width = physW;
          hudCanvas.height = physH;
        }
        backendRef.current?.resize(physW, physH);
        dirtyRef.current = true;

        const plotW = Math.max(64, w - 82);
        onCanvasWidthChanged?.(configRef.current.scopeId, plotW);
      }
    };

    const rect0 = stageEl.getBoundingClientRect();
    updateSizes(rect0.width, rect0.height);

    const resizeObserver = new ResizeObserver((entries) => {
      for (const entry of entries) {
        updateSizes(entry.contentRect.width, entry.contentRect.height);
      }
    });
    resizeObserver.observe(stageEl);

    createScopeRenderBackend(gpuCanvas)
      .then((backend) => {
        if (cancelled) {
          backend.dispose();
          return;
        }
        backendRef.current = backend;
        backendKindRef.current = backend.kind;
        setBackendKind(backend.kind);
        const { cssW, cssH, dpr } = sizeRef.current;
        if (cssW >= 2 && cssH >= 2) {
          backend.resize(Math.round(cssW * dpr), Math.round(cssH * dpr));
        }
        dirtyRef.current = true;
      })
      .catch(() => {
        // Handled by backend fallback chain
      });

    const updateMeasurementDom = (scopeData: DdsfScopeView | null) => {
      const cfg = configRef.current;
      const tracks = trackConfigsRef.current;
      const activeId =
        cfg.activeAnalogChannelId ??
        tracks.find((t) => t.lane === "analog" && t.visible)?.channelId ??
        tracks.find((t) => t.visible)?.channelId ??
        null;

      const activeTrackCfg = tracks.find((t) => t.channelId === activeId);
      const activeBlock = scopeData?.tracks.find((t) => t.channelId === activeId);
      const unit = activeTrackCfg?.unit ?? null;

      if (measChLabelRef.current) {
        measChLabelRef.current.textContent = activeTrackCfg ? activeTrackCfg.label : "--";
      }

      const stats = activeBlock?.stats ?? null;
      if (!stats) {
        if (measVppRef.current) measVppRef.current.textContent = "--";
        if (measVmaxRef.current) measVmaxRef.current.textContent = "--";
        if (measVminRef.current) measVminRef.current.textContent = "--";
        if (measVrmsRef.current) measVrmsRef.current.textContent = "--";
        if (measMeanRef.current) measMeanRef.current.textContent = "--";
        if (measFreqRef.current) measFreqRef.current.textContent = "--";
        if (measDutyRef.current) measDutyRef.current.textContent = "--";
        return;
      }

      if (measVppRef.current) measVppRef.current.textContent = formatMetricValue(stats.vpp, unit);
      if (measVmaxRef.current) measVmaxRef.current.textContent = formatMetricValue(stats.max, unit);
      if (measVminRef.current) measVminRef.current.textContent = formatMetricValue(stats.min, unit);
      if (measVrmsRef.current) measVrmsRef.current.textContent = formatMetricValue(stats.rms, unit);
      if (measMeanRef.current) measMeanRef.current.textContent = formatMetricValue(stats.mean, unit);
      if (measFreqRef.current) measFreqRef.current.textContent = formatFrequencyHz(stats.freqHz);
      if (measDutyRef.current) {
        measDutyRef.current.textContent =
          stats.duty > 0 && stats.duty <= 1 ? `${(stats.duty * 100).toFixed(1)}%` : "--";
      }
    };

    const tick = () => {
      if (cancelled) return;
      rafId = requestAnimationFrame(tick);

      const { cssW, cssH, dpr } = sizeRef.current;
      if (cssW < 2 || cssH < 2 || !backendRef.current) return;

      if (dirtyRef.current) {
        dirtyRef.current = false;
        const tStart = performance.now();
        const cfg = configRef.current;
        const tracks = trackConfigsRef.current;
        const scopeData = pendingScopeRef.current;

        const layout = computeMsoLayout(cssW, cssH, tracks, cfg.analogLayout);
        const built = geometryRef.current.build(layout, dpr, scopeData, tracks);

        const physW = Math.round(cssW * dpr);
        const physH = Math.round(cssH * dpr);
        backendRef.current.render(physW, physH, dpr, layout, built);

        const frameDt = performance.now() - tStart;
        const perf = perfStatsRef.current;
        perf.frameCount += 1;
        perf.frameTimes.push(frameDt);
        if (perf.frameTimes.length > 120) {
          perf.frameTimes.shift();
        }

        const nowMs = performance.now();
        if (nowMs - perf.lastReportMs >= 450) {
          const elapsed = nowMs - perf.lastReportMs;
          perf.fps = (perf.frameCount * 1000) / Math.max(1, elapsed);
          const sorted = [...perf.frameTimes].sort((a, b) => a - b);
          const p99Idx = Math.min(sorted.length - 1, Math.floor(sorted.length * 0.99));
          perf.p99Ms = sorted[p99Idx] ?? frameDt;
          perf.frameCount = 0;
          perf.lastReportMs = nowMs;
        }

        if (hudCtx) {
          renderScopeHud(hudCtx, layout, dpr, {
            sweepMode: cfg.sweepMode,
            timeSpanNs: cfg.timeSpanNs,
            preTriggerRatio: cfg.preTriggerRatio,
            panOffsetNs: cfg.panOffsetNs,
            trigger: cfg.trigger,
            cursorsEnabled: cfg.cursorsEnabled,
            cursorARatio: cfg.cursorARatio,
            cursorBRatio: cfg.cursorBRatio,
            activeAnalogChannelId: cfg.activeAnalogChannelId,
            tracks,
            scopeData,
            ribbonLabels: built.ribbonLabels,
            dataModeTag: built.dataModeTag,
            backendKind: backendKindRef.current,
            fps: perf.fps,
            p99FrameMs: perf.p99Ms,
            hoverHandle: hoverHandleRef.current,
            draggingHandle: dragStateRef.current.target,
          });
        }

        if (nowMs - perf.lastMeasUpdateMs >= 65) {
          perf.lastMeasUpdateMs = nowMs;
          updateMeasurementDom(scopeData);
        }
      }
    };

    rafId = requestAnimationFrame(tick);

    return () => {
      cancelled = true;
      cancelAnimationFrame(rafId);
      resizeObserver.disconnect();
      if (backendRef.current) {
        backendRef.current.dispose();
        backendRef.current = null;
      }
    };
  }, [onCanvasWidthChanged]);

  // Attach non-passive wheel listener for smooth cursor-centered zoom
  useEffect(() => {
    const hudCanvas = hudCanvasRef.current;
    if (!hudCanvas) return;

    const handleWheel = (ev: WheelEvent) => {
      ev.preventDefault();
      const rect = hudCanvas.getBoundingClientRect();
      const cssX = ev.clientX - rect.left;
      const cssY = ev.clientY - rect.top;
      const cfg = configRef.current;
      const layout = computeMsoLayout(
        sizeRef.current.cssW,
        sizeRef.current.cssH,
        trackConfigsRef.current,
        cfg.analogLayout,
      );

      if (ev.shiftKey || cssX < layout.plotLeft) {
        // Vertical Y-scale zoom on active analog channel
        const factor = ev.deltaY > 0 ? 1.2 : 0.82;
        const targetId =
          cfg.activeAnalogChannelId ??
          cfg.tracks.find((t) => t.lane === "analog" && t.visible)?.channelId ??
          null;
        if (targetId === null) return;
        callbacksRef.current.onUpdateScope(cfg.scopeId, (prev) => ({
          ...prev,
          tracks: prev.tracks.map((t) =>
            t.channelId === targetId
              ? { ...t, yScale: clamp(t.yScale * factor, 0.01, 5000) }
              : t,
          ),
        }));
        return;
      }

      const zoomFactor = ev.deltaY > 0 ? 1.25 : 0.8;
      const oldSpan = cfg.timeSpanNs;
      const newSpan = Math.round(clamp(oldSpan * zoomFactor, 1_000, 600_000_000_000));
      if (newSpan === oldSpan) return;

      const u = clamp((cssX - layout.plotLeft) / Math.max(1, layout.plotWidth), 0, 1);
      const zeroRatio = cfg.sweepMode === "roll" ? 1.0 : cfg.preTriggerRatio;
      const newPanOffset =
        cfg.panOffsetNs + (u - zeroRatio) * (oldSpan - newSpan);

      callbacksRef.current.onUpdateScope(cfg.scopeId, (prev) => ({
        ...prev,
        timeSpanNs: newSpan,
        panOffsetNs: Math.round(newPanOffset),
      }));

      if (cfg.syncGroupId !== null) {
        callbacksRef.current.onSyncGroupBroadcast(cfg.syncGroupId, cfg.scopeId, {
          timeSpanNs: newSpan,
          panOffsetNs: Math.round(newPanOffset),
        });
      }
      if (cssY < 0) return;
    };

    hudCanvas.addEventListener("wheel", handleWheel, { passive: false });
    return () => {
      hudCanvas.removeEventListener("wheel", handleWheel);
    };
  }, []);

  const handlePointerDown = useCallback((ev: React.PointerEvent<HTMLCanvasElement>) => {
    const hudCanvas = hudCanvasRef.current;
    if (!hudCanvas) return;
    onFocusScope?.(configRef.current.scopeId);

    const rect = hudCanvas.getBoundingClientRect();
    const cssX = ev.clientX - rect.left;
    const cssY = ev.clientY - rect.top;
    const cfg = configRef.current;
    const layout = computeMsoLayout(
      sizeRef.current.cssW,
      sizeRef.current.cssH,
      trackConfigsRef.current,
      cfg.analogLayout,
    );

    const target = hitTestHudHandle(cssX, cssY, layout, {
      sweepMode: cfg.sweepMode,
      preTriggerRatio: cfg.preTriggerRatio,
      trigger: cfg.trigger,
      cursorsEnabled: cfg.cursorsEnabled,
      cursorARatio: cfg.cursorARatio,
      cursorBRatio: cfg.cursorBRatio,
    });

    if (target === "none") return;

    hudCanvas.setPointerCapture(ev.pointerId);
    dragStateRef.current = {
      target,
      startCssX: cssX,
      startCssY: cssY,
      initialPanNs: cfg.panOffsetNs,
    };
    dirtyRef.current = true;
  }, [onFocusScope]);

  const handlePointerMove = useCallback((ev: React.PointerEvent<HTMLCanvasElement>) => {
    const hudCanvas = hudCanvasRef.current;
    if (!hudCanvas) return;
    const rect = hudCanvas.getBoundingClientRect();
    const cssX = ev.clientX - rect.left;
    const cssY = ev.clientY - rect.top;
    const cfg = configRef.current;
    const layout = computeMsoLayout(
      sizeRef.current.cssW,
      sizeRef.current.cssH,
      trackConfigsRef.current,
      cfg.analogLayout,
    );

    const dragging = dragStateRef.current;
    if (dragging.target === "none") {
      const hit = hitTestHudHandle(cssX, cssY, layout, {
        sweepMode: cfg.sweepMode,
        preTriggerRatio: cfg.preTriggerRatio,
        trigger: cfg.trigger,
        cursorsEnabled: cfg.cursorsEnabled,
        cursorARatio: cfg.cursorARatio,
        cursorBRatio: cfg.cursorBRatio,
      });
      if (hoverHandleRef.current !== hit) {
        hoverHandleRef.current = hit;
        hudCanvas.style.cursor = cursorForHit(hit);
        dirtyRef.current = true;
      }
      return;
    }

    const ratio = clamp((cssX - layout.plotLeft) / Math.max(1, layout.plotWidth), 0, 1);

    if (dragging.target === "cursorA") {
      callbacksRef.current.onUpdateScope(cfg.scopeId, (prev) => ({
        ...prev,
        cursorARatio: ratio,
      }));
      if (cfg.syncGroupId !== null) {
        callbacksRef.current.onSyncGroupBroadcast(cfg.syncGroupId, cfg.scopeId, {
          cursorARatio: ratio,
        });
      }
      return;
    }

    if (dragging.target === "cursorB") {
      callbacksRef.current.onUpdateScope(cfg.scopeId, (prev) => ({
        ...prev,
        cursorBRatio: ratio,
      }));
      if (cfg.syncGroupId !== null) {
        callbacksRef.current.onSyncGroupBroadcast(cfg.syncGroupId, cfg.scopeId, {
          cursorBRatio: ratio,
        });
      }
      return;
    }

    if (dragging.target === "preTrigger") {
      const clampedPre = clamp(ratio, 0.05, 0.95);
      callbacksRef.current.onUpdateScope(cfg.scopeId, (prev) => ({
        ...prev,
        preTriggerRatio: clampedPre,
      }));
      return;
    }

    if (dragging.target === "triggerLevel" && cfg.trigger?.kind === "edge") {
      const trigChId = cfg.trigger.channel_id;
      const lane =
        layout.analogSubLanes.find((l) => l.track.channelId === trigChId) ??
        layout.analogSubLanes[0];
      if (!lane) return;
      const nextLevel = Number(
        yToAnalogValue(cssY, lane.rect, lane.track.yScale, lane.track.yOffset).toFixed(2),
      );
      callbacksRef.current.onUpdateScope(cfg.scopeId, (prev) => ({
        ...prev,
        trigger:
          prev.trigger?.kind === "edge"
            ? { ...prev.trigger, level: nextLevel }
            : prev.trigger,
      }));
      return;
    }

    if (dragging.target === "plot") {
      hudCanvas.style.cursor = "grabbing";
      const dxPx = cssX - dragging.startCssX;
      const dNs = -(dxPx / Math.max(1, layout.plotWidth)) * cfg.timeSpanNs;
      const nextPanNs = Math.round(dragging.initialPanNs + dNs);
      callbacksRef.current.onUpdateScope(cfg.scopeId, (prev) => ({
        ...prev,
        panOffsetNs: nextPanNs,
      }));
      if (cfg.syncGroupId !== null) {
        callbacksRef.current.onSyncGroupBroadcast(cfg.syncGroupId, cfg.scopeId, {
          panOffsetNs: nextPanNs,
        });
      }
    }
  }, []);

  const handlePointerUp = useCallback((ev: React.PointerEvent<HTMLCanvasElement>) => {
    const hudCanvas = hudCanvasRef.current;
    if (dragStateRef.current.target !== "none") {
      dragStateRef.current.target = "none";
      if (hudCanvas && hudCanvas.hasPointerCapture(ev.pointerId)) {
        hudCanvas.releasePointerCapture(ev.pointerId);
      }
      if (hudCanvas) {
        hudCanvas.style.cursor = cursorForHit(hoverHandleRef.current);
      }
      dirtyRef.current = true;
    }
  }, []);

  const handleDragOver = (ev: DragEvent<HTMLDivElement>) => {
    if (ev.dataTransfer.types.includes("application/x-ddscope-channel")) {
      ev.preventDefault();
      ev.dataTransfer.dropEffect = "copy";
      if (!dropActive) setDropActive(true);
    }
  };

  const handleDragLeave = () => {
    if (dropActive) setDropActive(false);
  };

  const handleDrop = (ev: DragEvent<HTMLDivElement>) => {
    ev.preventDefault();
    setDropActive(false);
    const raw = ev.dataTransfer.getData("application/x-ddscope-channel");
    if (!raw) return;
    try {
      const parsed = JSON.parse(raw) as { channelId?: number };
      if (typeof parsed.channelId === "number") {
        callbacks.onDropChannel(config.scopeId, parsed.channelId);
      }
    } catch {
      // Ignore malformed drop payload
    }
  };

  const statusMeta = triggerStateLabel(runtimeTriggerState);
  const analogTracks = config.tracks.filter((t) => t.lane === "analog");

  return (
    <div
      className={`scope-widget ${isFocused ? "scope-widget-focused" : ""} ${dropActive ? "scope-widget-drop-active" : ""}`}
      data-testid={`scope-widget-${config.scopeId}`}
      onMouseDown={() => onFocusScope?.(config.scopeId)}
      onDragOver={handleDragOver}
      onDragLeave={handleDragLeave}
      onDrop={handleDrop}
    >
      {/* Top Instrument Control Bar */}
      <div className="scope-toolbar">
        <div className="scope-toolbar-group">
          <div className="scope-segmented" role="group" aria-label="Sweep Mode">
            {SWEEP_MODES.map((m) => (
              <button
                key={m.id}
                type="button"
                className={`scope-seg-btn ${config.sweepMode === m.id ? "active" : ""}`}
                onClick={() =>
                  callbacks.onUpdateScope(config.scopeId, (prev) => ({
                    ...prev,
                    sweepMode: m.id,
                    panOffsetNs: 0,
                  }))
                }
              >
                {m.label}
              </button>
            ))}
          </div>

          <span
            className={`scope-status-pill scope-status-${statusMeta.tone}`}
            data-testid={`scope-status-${config.scopeId}`}
          >
            {statusMeta.text}
          </span>

          {(config.sweepMode === "single" ||
            runtimeTriggerState === TriggerRuntimeStateCode.LatchedPinned) && (
            <button
              type="button"
              className="scope-btn scope-btn-accent"
              data-testid={`scope-rearm-${config.scopeId}`}
              onClick={() => callbacks.onRearmScope(config.scopeId)}
            >
              Re-arm
            </button>
          )}

          <button
            type="button"
            className={`scope-btn ${config.paused ? "scope-btn-warn" : ""}`}
            onClick={() =>
              callbacks.onUpdateScope(config.scopeId, (prev) => ({
                ...prev,
                paused: !prev.paused,
              }))
            }
          >
            {config.paused ? "Resume" : "Pause"}
          </button>

          {config.panOffsetNs !== 0 && (
            <button
              type="button"
              className="scope-btn"
              title="Reset Pan Offset"
              onClick={() =>
                callbacks.onUpdateScope(config.scopeId, (prev) => ({
                  ...prev,
                  panOffsetNs: 0,
                }))
              }
            >
              Zero Pan
            </button>
          )}
        </div>

        <div className="scope-toolbar-group">
          <button
            type="button"
            className={`scope-btn ${config.analogLayout === "stacked" ? "active" : ""}`}
            data-testid={`scope-layout-${config.scopeId}`}
            onClick={() =>
              callbacks.onUpdateScope(config.scopeId, (prev) => ({
                ...prev,
                analogLayout: prev.analogLayout === "overlay" ? "stacked" : "overlay",
              }))
            }
          >
            {config.analogLayout === "overlay" ? "Overlay" : "Stacked"}
          </button>

          <button
            type="button"
            className={`scope-btn ${config.cursorsEnabled ? "active" : ""}`}
            data-testid={`scope-cursors-${config.scopeId}`}
            onClick={() =>
              callbacks.onUpdateScope(config.scopeId, (prev) => ({
                ...prev,
                cursorsEnabled: !prev.cursorsEnabled,
              }))
            }
          >
            Cursors A/B
          </button>

          <select
            className="scope-select"
            aria-label="Sync Group"
            data-testid={`scope-sync-${config.scopeId}`}
            value={config.syncGroupId === null ? "" : String(config.syncGroupId)}
            onChange={(ev) => {
              const raw = ev.target.value;
              const nextGroup = raw === "" ? null : Number(raw);
              callbacks.onUpdateScope(config.scopeId, (prev) => ({
                ...prev,
                syncGroupId: nextGroup,
              }));
            }}
          >
            {SYNC_GROUP_OPTIONS.map((opt) => (
              <option key={String(opt.id)} value={opt.id === null ? "" : String(opt.id)}>
                {opt.label}
              </option>
            ))}
          </select>

          <select
            className="scope-select"
            aria-label="Timebase Span"
            value={String(config.timeSpanNs)}
            onChange={(ev) => {
              const nextSpan = Number(ev.target.value);
              callbacks.onUpdateScope(config.scopeId, (prev) => ({
                ...prev,
                timeSpanNs: nextSpan,
              }));
              if (config.syncGroupId !== null) {
                callbacks.onSyncGroupBroadcast(config.syncGroupId, config.scopeId, {
                  timeSpanNs: nextSpan,
                });
              }
            }}
          >
            {[
              10_000,
              50_000,
              200_000,
              1_000_000,
              5_000_000,
              10_000_000,
              20_000_000,
              50_000_000,
              200_000_000,
              1_000_000_000,
              5_000_000_000,
            ].map((span) => (
              <option key={span} value={String(span)}>
                Span: {formatTimeNs(span)}
              </option>
            ))}
          </select>
        </div>
      </div>

      {/* Trigger & Bound Channel Strip */}
      <div className="scope-subbar">
        {config.sweepMode !== "roll" && (
          <div className="scope-trigger-controls">
            <span className="scope-sublabel">Trig:</span>
            <select
              className="scope-select-sm"
              aria-label="Trigger Channel"
              value={config.trigger ? String(config.trigger.channel_id) : ""}
              onChange={(ev) => {
                const chId = Number(ev.target.value);
                const ch = channelsById[chId];
                callbacks.onUpdateScope(config.scopeId, (prev) => ({
                  ...prev,
                  trigger:
                    ch?.modality === "state_machine"
                      ? { kind: "state_equals", channel_id: chId, state_code: 3 }
                      : {
                          kind: "edge",
                          channel_id: chId,
                          level: ch?.local_path === "v_bus" ? 85 : 0,
                          hysteresis: 1.0,
                          slope: "rising",
                        },
                }));
              }}
            >
              {config.tracks.map((t) => {
                const ch = channelsById[t.channelId];
                return (
                  <option key={t.channelId} value={String(t.channelId)}>
                    {ch ? ch.full_path : `ch${t.channelId}`}
                  </option>
                );
              })}
            </select>

            {config.trigger?.kind === "edge" && (
              <>
                <select
                  className="scope-select-sm"
                  aria-label="Trigger Slope"
                  value={config.trigger.slope}
                  onChange={(ev) => {
                    const slope = ev.target.value as EdgeSlopeDto;
                    callbacks.onUpdateScope(config.scopeId, (prev) => ({
                      ...prev,
                      trigger:
                        prev.trigger?.kind === "edge"
                          ? { ...prev.trigger, slope }
                          : prev.trigger,
                    }));
                  }}
                >
                  <option value="rising">↑ Rising</option>
                  <option value="falling">↓ Falling</option>
                  <option value="either">↕ Either</option>
                </select>
                <label className="scope-inline-num">
                  Lvl
                  <input
                    type="number"
                    step="0.5"
                    value={config.trigger.level ?? 0}
                    onChange={(ev) => {
                      const level = Number.parseFloat(ev.target.value);
                      if (!Number.isFinite(level)) return;
                      callbacks.onUpdateScope(config.scopeId, (prev) => ({
                        ...prev,
                        trigger:
                          prev.trigger?.kind === "edge"
                            ? { ...prev.trigger, level }
                            : prev.trigger,
                      }));
                    }}
                  />
                </label>
                <label className="scope-inline-num">
                  Hys
                  <input
                    type="number"
                    min="0"
                    step="0.2"
                    value={config.trigger.hysteresis ?? 0.5}
                    onChange={(ev) => {
                      const hysteresis = Math.max(0, Number.parseFloat(ev.target.value) || 0);
                      callbacks.onUpdateScope(config.scopeId, (prev) => ({
                        ...prev,
                        trigger:
                          prev.trigger?.kind === "edge"
                            ? { ...prev.trigger, hysteresis }
                            : prev.trigger,
                      }));
                    }}
                  />
                </label>
              </>
            )}

            {config.trigger?.kind === "state_equals" && (
              <label className="scope-inline-num">
                State
                <input
                  type="number"
                  step="1"
                  value={config.trigger.state_code}
                  onChange={(ev) => {
                    const state_code = Math.round(Number.parseFloat(ev.target.value) || 0);
                    callbacks.onUpdateScope(config.scopeId, (prev) => ({
                      ...prev,
                      trigger:
                        prev.trigger?.kind === "state_equals"
                          ? { ...prev.trigger, state_code }
                          : prev.trigger,
                    }));
                  }}
                />
              </label>
            )}
          </div>
        )}

        <div className="scope-channel-chips">
          {config.tracks.map((t) => {
            const ch = channelsById[t.channelId];
            const label = ch ? ch.full_path : `ch${t.channelId}`;
            const isActive =
              t.lane === "analog" &&
              (config.activeAnalogChannelId === t.channelId ||
                (config.activeAnalogChannelId === null && analogTracks[0]?.channelId === t.channelId));
            return (
              <div
                key={t.channelId}
                className={`scope-ch-chip ${!t.visible ? "muted" : ""} ${isActive ? "active" : ""}`}
                onClick={() => {
                  if (t.lane === "analog") {
                    callbacks.onUpdateScope(config.scopeId, (prev) => ({
                      ...prev,
                      activeAnalogChannelId: t.channelId,
                    }));
                  }
                }}
              >
                <span
                  className="scope-ch-dot"
                  style={{ backgroundColor: rgbaNumberToCss(t.colorRgba, 1) }}
                />
                <span className="scope-ch-name">{label}</span>
                {t.lane === "analog" && (
                  <span className="scope-ch-scale">
                    <button
                      type="button"
                      title="Zoom Y In"
                      onClick={(ev) => {
                        ev.stopPropagation();
                        callbacks.onUpdateScope(config.scopeId, (prev) => ({
                          ...prev,
                          tracks: prev.tracks.map((tr) =>
                            tr.channelId === t.channelId
                              ? { ...tr, yScale: clamp(tr.yScale * 0.75, 0.01, 5000) }
                              : tr,
                          ),
                        }));
                      }}
                    >
                      +
                    </button>
                    <span>{t.yScale.toFixed(t.yScale >= 10 ? 0 : 1)}/d</span>
                    <button
                      type="button"
                      title="Zoom Y Out"
                      onClick={(ev) => {
                        ev.stopPropagation();
                        callbacks.onUpdateScope(config.scopeId, (prev) => ({
                          ...prev,
                          tracks: prev.tracks.map((tr) =>
                            tr.channelId === t.channelId
                              ? { ...tr, yScale: clamp(tr.yScale * 1.33, 0.01, 5000) }
                              : tr,
                          ),
                        }));
                      }}
                    >
                      −
                    </button>
                  </span>
                )}
                <button
                  type="button"
                  className="scope-ch-vis"
                  title={t.visible ? "Hide Track" : "Show Track"}
                  onClick={(ev) => {
                    ev.stopPropagation();
                    callbacks.onUpdateScope(config.scopeId, (prev) => ({
                      ...prev,
                      tracks: prev.tracks.map((tr) =>
                        tr.channelId === t.channelId ? { ...tr, visible: !tr.visible } : tr,
                      ),
                    }));
                  }}
                >
                  {t.visible ? "◉" : "○"}
                </button>
                <button
                  type="button"
                  className="scope-ch-del"
                  title="Unbind Track"
                  onClick={(ev) => {
                    ev.stopPropagation();
                    callbacks.onUpdateScope(config.scopeId, (prev) => ({
                      ...prev,
                      tracks: prev.tracks.filter((tr) => tr.channelId !== t.channelId),
                    }));
                  }}
                >
                  ×
                </button>
              </div>
            );
          })}
          {config.tracks.length === 0 && (
            <span className="scope-empty-hint">
              Drag channels from the left Channel Tree here to bind MSO tracks
            </span>
          )}
        </div>
      </div>

      {/* Stacked GPU + Canvas 2D HUD Viewport Stage */}
      <div className="scope-canvas-stage" ref={stageRef}>
        <canvas ref={gpuCanvasRef} className="scope-canvas-layer" />
        <canvas
          ref={hudCanvasRef}
          className="scope-canvas-layer scope-hud-canvas"
          data-testid={`scope-canvas-${config.scopeId}`}
          data-backend={backendKind}
          onPointerDown={handlePointerDown}
          onPointerMove={handlePointerMove}
          onPointerUp={handlePointerUp}
          onPointerCancel={handlePointerUp}
        />
        {dropActive && (
          <div className="scope-drop-banner">Drop Channel to Bind MSO Track</div>
        )}
      </div>

      {/* Bottom Hardware Auto-Measurement Bar (DOM-ref updated at 15Hz) */}
      <div className="scope-meas-bar" data-testid={`scope-meas-${config.scopeId}`}>
        <span className="scope-meas-ch" ref={measChLabelRef}>
          --
        </span>
        <span className="scope-meas-item">
          <em>Vpp</em> <strong ref={measVppRef}>--</strong>
        </span>
        <span className="scope-meas-item">
          <em>Vmax</em> <strong ref={measVmaxRef}>--</strong>
        </span>
        <span className="scope-meas-item">
          <em>Vmin</em> <strong ref={measVminRef}>--</strong>
        </span>
        <span className="scope-meas-item">
          <em>Vrms</em> <strong ref={measVrmsRef}>--</strong>
        </span>
        <span className="scope-meas-item">
          <em>Mean</em> <strong ref={measMeanRef}>--</strong>
        </span>
        <span className="scope-meas-item">
          <em>Freq</em> <strong ref={measFreqRef}>--</strong>
        </span>
        <span className="scope-meas-item">
          <em>Duty</em> <strong ref={measDutyRef}>--</strong>
        </span>
      </div>
    </div>
  );
}
