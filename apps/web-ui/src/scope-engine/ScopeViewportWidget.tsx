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
import type { DdsfFrameHeaderView, DdsfScopeView } from "../bridge/ddsf-parser.ts";
import { DdsfSnapshotKind, TriggerRuntimeStateCode } from "../bridge/ddsf-spec.ts";
import { MOCK_BASE_HOST_NS } from "../bridge/mock-ddsf-core.ts";
import { translate, type MessageKey, type UiLocale } from "../i18n/index.ts";
import {
  DDSCOPE_POINTER_DROP_EVENT,
  type PointerDropEventDetail,
} from "../workspace/pointer-dnd.ts";
import type { RenderBackendKind, ScopeRenderBackend } from "./backend.ts";
import { createScopeRenderBackend } from "./create-backend.ts";
import { ScopeGeometryBuilder } from "./geometry.ts";
import {
  hitTestHudHandle,
  renderScopeHud,
  type HudHitTarget,
} from "./hud-overlay.ts";
import {
  DEFAULT_Y_AXIS_CONFIG,
  channelValueToAxis,
  computeHorizontalPanBounds,
  computeMsoLayout,
  yToChannelValue,
  zoomXAxisAtCursor,
  zoomYAxisAtCursor,
  type Rect,
  type ScopeTrackRenderConfig,
} from "./layout.ts";
import {
  formatFrequencyHz,
  formatMetricValue,
  formatTimeNs,
  hexToRgbaNumber,
  rgbaNumberToCss,
  rgbaNumberToHex,
} from "./time-format.ts";
import type {
  ScopeCallbacks,
  ScopeChannelLookup,
  ScopeUiConfig,
  WheelZoomTarget,
} from "./types.ts";

export interface ScopeViewportWidgetProps {
  config: ScopeUiConfig;
  channelsById: ScopeChannelLookup;
  callbacks: ScopeCallbacks;
  locale?: UiLocale;
  isFocused?: boolean;
  onFocusScope?: (scopeId: number) => void;
  onCanvasWidthChanged?: (scopeId: number, widthPx: number) => void;
}

const SWEEP_MODES: ReadonlyArray<{ id: TriggerSweepModeDto; labelKey: MessageKey }> = [
  { id: "roll", labelKey: "scope.sweep.roll" },
  { id: "auto", labelKey: "scope.sweep.auto" },
  { id: "normal", labelKey: "scope.sweep.normal" },
  { id: "single", labelKey: "scope.sweep.single" },
];

const SYNC_GROUP_OPTIONS: ReadonlyArray<{ id: number | null; labelKey: MessageKey }> = [
  { id: null, labelKey: "scope.syncOff" },
  { id: 1, labelKey: "scope.syncA" },
  { id: 2, labelKey: "scope.syncB" },
];

function triggerStateLabel(code: number): {
  labelKey: MessageKey;
  tone: "roll" | "armed" | "auto" | "trig" | "latched";
} {
  switch (code) {
    case TriggerRuntimeStateCode.Roll:
      return { labelKey: "scope.status.roll", tone: "roll" };
    case TriggerRuntimeStateCode.Armed:
      return { labelKey: "scope.status.armed", tone: "armed" };
    case TriggerRuntimeStateCode.AutoFreerun:
      return { labelKey: "scope.status.auto", tone: "auto" };
    case TriggerRuntimeStateCode.Triggered:
      return { labelKey: "scope.status.triggered", tone: "trig" };
    case TriggerRuntimeStateCode.LatchedPinned:
      return { labelKey: "scope.status.latched", tone: "latched" };
    default:
      return { labelKey: "scope.status.idle", tone: "roll" };
  }
}

const CHIP_TRACE_COLORS = [
  0x38bdf8ff,
  0x34d399ff,
  0xf59e0bff,
  0xfb7185ff,
  0xa78bfaff,
  0x22d3eeff,
  0xf97316ff,
  0xe879f9ff,
  0x84cc16ff,
  0xfacc15ff,
];

function cursorForHit(hit: HudHitTarget): string {
  switch (hit) {
    case "cursorA":
    case "cursorB":
    case "preTrigger":
    case "xGutter":
      return "ew-resize";
    case "triggerLevel":
    case "yGutter":
      return "ns-resize";
    case "plot":
      return "grab";
    default:
      return "default";
  }
}

function clamp(v: number, min: number, max: number): number {
  return Math.max(min, Math.min(max, v));
}

function formatConditionSummary(scale: number, offset: number): string {
  const s = Number.isInteger(scale) ? String(scale) : scale.toFixed(2).replace(/\.?0+$/, "");
  if (Math.abs(offset) < 1e-6) {
    return `×${s}`;
  }
  const o = Number.isInteger(offset) ? String(offset) : offset.toFixed(1);
  return offset >= 0 ? `×${s} +${o}` : `×${s} ${o}`;
}

function findAnalogRectForY(
  layout: ReturnType<typeof computeMsoLayout>,
  cssY: number,
): Rect {
  const exact = layout.analogSubLanes.find(
    (l) => cssY >= l.rect.y && cssY <= l.rect.y + l.rect.height,
  );
  return exact?.rect ?? layout.analogRegion;
}

export function ScopeViewportWidget({
  config,
  channelsById,
  callbacks,
  locale = "en",
  isFocused = false,
  onFocusScope,
  onCanvasWidthChanged,
}: ScopeViewportWidgetProps) {
  const t = (key: MessageKey, params?: Record<string, string | number>) =>
    translate(locale, key, params);
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
  const rootRef = useRef<HTMLDivElement | null>(null);
  const [dropActive, setDropActive] = useState(false);
  const [editingChannelId, setEditingChannelId] = useState<number | null>(null);

  const configRef = useRef(config);
  configRef.current = config;

  const localeRef = useRef<UiLocale>(locale);
  localeRef.current = locale;

  const channelsRef = useRef(channelsById);
  channelsRef.current = channelsById;

  const callbacksRef = useRef(callbacks);
  callbacksRef.current = callbacks;

  useEffect(() => {
    const el = rootRef.current;
    if (!el) return;
    const onPointerDrop = (ev: Event) => {
      const detail = (ev as CustomEvent<PointerDropEventDetail>).detail;
      if (!detail || detail.payload.kind !== "channel") return;
      ev.stopPropagation();
      setDropActive(false);
      callbacksRef.current.onDropChannel(
        configRef.current.scopeId,
        detail.payload.channelId,
      );
    };
    el.addEventListener(DDSCOPE_POINTER_DROP_EVENT, onPointerDrop);
    return () => el.removeEventListener(DDSCOPE_POINTER_DROP_EVENT, onPointerDrop);
  }, []);

  const initialFrame = bridgeClient.topicBus.getLatestScopeFrame(config.scopeId);
  const pendingScopeRef = useRef<DdsfScopeView | null>(initialFrame?.scope ?? null);
  const latestHeaderRef = useRef<DdsfFrameHeaderView | null>(initialFrame?.header ?? null);
  const pausedHostNowNsRef = useRef<number | null>(null);
  const chipValueRefs = useRef<Map<number, HTMLSpanElement>>(new Map());
  const latestSnapValsRef = useRef<Map<number, { kind: number; val: number }>>(new Map());
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
    /** Whole-axis Y center offset at pointer-down (null when Y-drag is inactive). */
    yAxisActive: boolean;
    initialYAxisOffset: number;
    /** Coordinate axis units per CSS pixel for the dragged analog region/lane. */
    yUnitsPerPx: number;
  }>({
    target: "none",
    startCssX: 0,
    startCssY: 0,
    initialPanNs: 0,
    yAxisActive: false,
    initialYAxisOffset: 0,
    yUnitsPerPx: 0,
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
  }, [config, trackRenderConfigs, locale]);

  // Subscribe to zero-copy DDSF frames for this scope without triggering React re-renders
  useEffect(() => {
    const unsub = bridgeClient.topicBus.subscribeScope(config.scopeId, (scopeView, header) => {
      pendingScopeRef.current = scopeView;
      latestHeaderRef.current = header;
      dirtyRef.current = true;

      if (scopeView.triggerState !== perfStatsRef.current.lastTriggerCode) {
        perfStatsRef.current.lastTriggerCode = scopeView.triggerState;
        setRuntimeTriggerState(scopeView.triggerState);
      }
    });
    return unsub;
  }, [config.scopeId]);

  const flushChipValues = useCallback(() => {
    const curLocale = localeRef.current;
    const vals = latestSnapValsRef.current;
    chipValueRefs.current.forEach((el, chId) => {
      const snap = vals.get(chId);
      if (!snap) return;
      const ch = channelsRef.current[chId];
      if (snap.kind === DdsfSnapshotKind.Bool) {
        el.textContent =
          snap.val > 0.5
            ? translate(curLocale, "sidebar.valHigh")
            : translate(curLocale, "sidebar.valLow");
        el.className = snap.val > 0.5 ? "scope-ch-live val-high" : "scope-ch-live val-low";
      } else if (snap.kind === DdsfSnapshotKind.StateEnum) {
        const code = Math.round(snap.val);
        const lbl = ch?.state_labels.find((l) => l.code === code)?.name ?? `S${code}`;
        el.textContent = lbl;
        el.className = code === 3 ? "scope-ch-live val-fault" : "scope-ch-live val-state";
      } else {
        el.textContent = formatMetricValue(snap.val, ch?.unit ?? null);
        el.className = "scope-ch-live";
      }
    });
  }, []);

  // 15 Hz live value badges on bound channel chips
  useEffect(() => {
    for (const tr of configRef.current.tracks) {
      const snap = bridgeClient.topicBus.getLatestSnapshot(tr.channelId);
      if (snap) {
        latestSnapValsRef.current.set(snap.channelNumId, { kind: snap.kind, val: snap.valF64 });
      }
    }
    flushChipValues();

    let lastFlushMs = 0;
    const unsub = bridgeClient.topicBus.subscribeSnapshots((snapshots) => {
      for (let i = 0; i < snapshots.length; i += 1) {
        const s = snapshots[i];
        latestSnapValsRef.current.set(s.channelNumId, { kind: s.kind, val: s.valF64 });
      }
      const now = performance.now();
      if (now - lastFlushMs < 65) return;
      lastFlushMs = now;
      flushChipValues();
    });
    return unsub;
  }, [flushChipValues]);

  useEffect(() => {
    for (const tr of config.tracks) {
      const snap = bridgeClient.topicBus.getLatestSnapshot(tr.channelId);
      if (snap) {
        latestSnapValsRef.current.set(snap.channelNumId, { kind: snap.kind, val: snap.valF64 });
      }
    }
    flushChipValues();
  }, [config.tracks, channelsById, locale, flushChipValues]);

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
        const physPlotW = Math.min(4096, Math.max(128, Math.round(plotW * dpr)));
        onCanvasWidthChanged?.(configRef.current.scopeId, physPlotW);
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
        const axisCfg = {
          yAxisScale: cfg.yAxisScale ?? DEFAULT_Y_AXIS_CONFIG.yAxisScale,
          yAxisOffset: cfg.yAxisOffset ?? DEFAULT_Y_AXIS_CONFIG.yAxisOffset,
        };
        const built = geometryRef.current.build(layout, dpr, scopeData, tracks, axisCfg);

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
            locale: localeRef.current,
            sweepMode: cfg.sweepMode,
            timeSpanNs: cfg.timeSpanNs,
            preTriggerRatio: cfg.preTriggerRatio,
            panOffsetNs: cfg.panOffsetNs,
            yAxisScale: axisCfg.yAxisScale,
            yAxisOffset: axisCfg.yAxisOffset,
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

  const computePanBoundsForConfig = useCallback(
    (cfg: ScopeUiConfig, targetSpanNs?: number) => {
      const scopeData = pendingScopeRef.current;
      const header = latestHeaderRef.current;
      const hasVisibleTracks = cfg.tracks.some((t) => t.visible);
      if (!scopeData || !hasVisibleTracks) {
        return computeHorizontalPanBounds({
          hasScopeData: false,
          hasVisibleTracks,
          triggerState: 0,
          totalTrackElements: 0,
          sweepMode: cfg.sweepMode,
          preTriggerRatio: cfg.preTriggerRatio,
          paused: cfg.paused,
          timeSpanNs: cfg.timeSpanNs,
          targetSpanNs,
          currentPanNs: cfg.panOffsetNs,
          winTStartNs: 0,
          winTSpanNs: cfg.timeSpanNs,
          hostNowNs: 0,
          floorWinStartNs: 0,
        });
      }
      let totalTrackElements = 0;
      for (let i = 0; i < scopeData.tracks.length; i += 1) {
        totalTrackElements += scopeData.tracks[i].elementCount;
      }
      const floorWinStartNs =
        scopeData.winTStartNs >= MOCK_BASE_HOST_NS - 1_000_000_000 ? MOCK_BASE_HOST_NS : 0;
      const liveHostNowNs = header?.hostNowNs ?? scopeData.winTStartNs + scopeData.winTSpanNs;
      const hostNowNs = cfg.paused
        ? (pausedHostNowNsRef.current ?? scopeData.winTStartNs + scopeData.winTSpanNs)
        : liveHostNowNs;
      return computeHorizontalPanBounds({
        hasScopeData: true,
        hasVisibleTracks,
        triggerState: scopeData.triggerState,
        totalTrackElements,
        sweepMode: cfg.sweepMode,
        preTriggerRatio: cfg.preTriggerRatio,
        paused: cfg.paused,
        timeSpanNs: cfg.timeSpanNs,
        targetSpanNs,
        currentPanNs: cfg.panOffsetNs,
        winTStartNs: scopeData.winTStartNs,
        winTSpanNs: scopeData.winTSpanNs,
        hostNowNs,
        floorWinStartNs,
      });
    },
    [],
  );

  // Attach non-passive wheel listener for smooth cursor-centered X & Y coordinate zoom
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

      const onLeftGutter = cssX < layout.plotLeft;
      const onBottomGutter = cssY > layout.plotBottom;
      const wheelMode = cfg.wheelZoomTarget ?? "x";

      const zoomY =
        onLeftGutter ||
        ev.shiftKey ||
        ev.ctrlKey ||
        ev.metaKey ||
        ev.altKey ||
        (!onBottomGutter && (wheelMode === "y" || wheelMode === "xy"));

      const zoomX =
        !onLeftGutter &&
        (onBottomGutter ||
          (!ev.shiftKey && !ev.ctrlKey && !ev.metaKey && (wheelMode === "x" || wheelMode === "xy" || ev.altKey)));

      const yFactor = ev.deltaY > 0 ? 1.2 : 0.82;
      const xFactor = ev.deltaY > 0 ? 1.25 : 0.8;
      const targetRect = findAnalogRectForY(layout, cssY);
      const zeroRatio = cfg.sweepMode === "roll" ? 1.0 : cfg.preTriggerRatio;

      let nextSpanNs = cfg.timeSpanNs;
      let nextPanNs = cfg.panOffsetNs;
      let xChanged = false;

      if (zoomX) {
        const zoomedX = zoomXAxisAtCursor(
          cssX,
          layout.plotLeft,
          layout.plotWidth,
          cfg.timeSpanNs,
          cfg.panOffsetNs,
          zeroRatio,
          xFactor,
          (newSpan) => computePanBoundsForConfig(cfg, newSpan),
        );
        nextSpanNs = zoomedX.timeSpanNs;
        nextPanNs = zoomedX.panOffsetNs;
        xChanged = nextSpanNs !== cfg.timeSpanNs || nextPanNs !== cfg.panOffsetNs;
      }

      if (!zoomY && !xChanged) return;

      callbacksRef.current.onUpdateScope(cfg.scopeId, (prev) => {
        let nextAxis = {
          yAxisScale: prev.yAxisScale ?? DEFAULT_Y_AXIS_CONFIG.yAxisScale,
          yAxisOffset: prev.yAxisOffset ?? DEFAULT_Y_AXIS_CONFIG.yAxisOffset,
        };
        if (zoomY) {
          nextAxis = zoomYAxisAtCursor(
            cssY,
            targetRect,
            nextAxis.yAxisScale,
            nextAxis.yAxisOffset,
            yFactor,
          );
        }
        return {
          ...prev,
          yAxisScale: nextAxis.yAxisScale,
          yAxisOffset: nextAxis.yAxisOffset,
          timeSpanNs: zoomX ? nextSpanNs : prev.timeSpanNs,
          panOffsetNs: zoomX ? nextPanNs : prev.panOffsetNs,
        };
      });

      if (zoomX && xChanged && cfg.syncGroupId !== null) {
        callbacksRef.current.onSyncGroupBroadcast(cfg.syncGroupId, cfg.scopeId, {
          timeSpanNs: nextSpanNs,
          panOffsetNs: nextPanNs,
        });
      }
    };

    hudCanvas.addEventListener("wheel", handleWheel, { passive: false });
    return () => {
      hudCanvas.removeEventListener("wheel", handleWheel);
    };
  }, [computePanBoundsForConfig]);

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
      yAxisScale: cfg.yAxisScale,
      yAxisOffset: cfg.yAxisOffset,
    });

    if (target === "none") return;

    // Resolve whole-axis Y-drag metrics across the hovered analog coordinate region/lane.
    let yAxisActive = false;
    const initialYAxisOffset = cfg.yAxisOffset ?? DEFAULT_Y_AXIS_CONFIG.yAxisOffset;
    const curYAxisScale = cfg.yAxisScale ?? DEFAULT_Y_AXIS_CONFIG.yAxisScale;
    let yUnitsPerPx = 0;
    if (target === "plot" || target === "yGutter") {
      const region = layout.analogRegion;
      const inAnalogRegion = cssY >= region.y && cssY <= region.y + region.height;
      if (layout.analogSubLanes.length > 0 && (inAnalogRegion || target === "yGutter")) {
        const laneRect = findAnalogRectForY(layout, cssY);
        yAxisActive = true;
        // 8 vertical divisions span the analog coordinate axis height.
        yUnitsPerPx = (8 * Math.max(1e-4, curYAxisScale)) / Math.max(1, laneRect.height);
      }
    }

    hudCanvas.setPointerCapture(ev.pointerId);
    dragStateRef.current = {
      target,
      startCssX: cssX,
      startCssY: cssY,
      initialPanNs: cfg.panOffsetNs,
      yAxisActive,
      initialYAxisOffset,
      yUnitsPerPx,
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
        yAxisScale: cfg.yAxisScale,
        yAxisOffset: cfg.yAxisOffset,
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
      const axisCfg = {
        yAxisScale: cfg.yAxisScale ?? DEFAULT_Y_AXIS_CONFIG.yAxisScale,
        yAxisOffset: cfg.yAxisOffset ?? DEFAULT_Y_AXIS_CONFIG.yAxisOffset,
      };
      const nextLevel = Number(
        yToChannelValue(cssY, lane.rect, axisCfg, lane.track.yScale, lane.track.yOffset).toFixed(2),
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

    if (dragging.target === "yGutter") {
      if (!dragging.yAxisActive) return;
      hudCanvas.style.cursor = "ns-resize";
      const nextYAxisOffset =
        dragging.initialYAxisOffset + (cssY - dragging.startCssY) * dragging.yUnitsPerPx;
      callbacksRef.current.onUpdateScope(cfg.scopeId, (prev) => ({
        ...prev,
        yAxisOffset: nextYAxisOffset,
      }));
      return;
    }

    if (dragging.target === "xGutter" || dragging.target === "plot") {
      hudCanvas.style.cursor = dragging.target === "xGutter" ? "ew-resize" : "grabbing";
      const dxPx = cssX - dragging.startCssX;
      const dyPx = cssY - dragging.startCssY;

      const bounds = computePanBoundsForConfig(cfg);
      let nextPanNs = cfg.panOffsetNs;
      if (!bounds.canPan) {
        dragging.startCssX = cssX;
        dragging.initialPanNs = cfg.panOffsetNs;
      } else {
        const dNs = -(dxPx / Math.max(1, layout.plotWidth)) * cfg.timeSpanNs;
        const rawNextPanNs = Math.round(dragging.initialPanNs + dNs);
        nextPanNs = Math.round(clamp(rawNextPanNs, bounds.minPanNs, bounds.maxPanNs));
        if (nextPanNs !== rawNextPanNs) {
          dragging.startCssX = cssX;
          dragging.initialPanNs = nextPanNs;
        }
      }

      const updateYAxis = dragging.target === "plot" && dragging.yAxisActive;
      const nextYAxisOffset = updateYAxis
        ? dragging.initialYAxisOffset + dyPx * dragging.yUnitsPerPx
        : cfg.yAxisOffset;

      if (nextPanNs === cfg.panOffsetNs && (!updateYAxis || nextYAxisOffset === cfg.yAxisOffset)) {
        return;
      }

      callbacksRef.current.onUpdateScope(cfg.scopeId, (prev) => ({
        ...prev,
        panOffsetNs: nextPanNs,
        ...(updateYAxis ? { yAxisOffset: nextYAxisOffset } : {}),
      }));
      if (cfg.syncGroupId !== null && nextPanNs !== cfg.panOffsetNs) {
        callbacksRef.current.onSyncGroupBroadcast(cfg.syncGroupId, cfg.scopeId, {
          panOffsetNs: nextPanNs,
        });
      }
    }
  }, [computePanBoundsForConfig]);

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
    const types = Array.from(ev.dataTransfer.types);
    if (
      !types.includes("application/x-ddscope-widget") &&
      (types.includes("application/x-ddscope-channel") || types.includes("text/plain"))
    ) {
      ev.preventDefault();
      ev.dataTransfer.dropEffect = "copy";
      if (!dropActive) setDropActive(true);
    }
  };

  const handleDragLeave = (ev: DragEvent<HTMLDivElement>) => {
    if (!ev.currentTarget.contains(ev.relatedTarget as Node | null) && dropActive) {
      setDropActive(false);
    }
  };

  const handleDrop = (ev: DragEvent<HTMLDivElement>) => {
    setDropActive(false);
    let droppedChannelId: number | null = null;
    const raw = ev.dataTransfer.getData("application/x-ddscope-channel");
    if (raw) {
      try {
        const parsed = JSON.parse(raw) as { channelId?: number };
        if (typeof parsed.channelId === "number" && Number.isFinite(parsed.channelId)) {
          droppedChannelId = parsed.channelId;
        }
      } catch {
        // Ignore malformed drop payload
      }
    }
    if (droppedChannelId === null) {
      const plain = ev.dataTransfer.getData("text/plain").trim();
      if (plain.startsWith("channel:")) {
        const num = Number(plain.slice("channel:".length));
        if (Number.isFinite(num)) droppedChannelId = num;
      } else if (/^\d+$/.test(plain)) {
        droppedChannelId = Number(plain);
      }
    }
    if (droppedChannelId !== null) {
      ev.preventDefault();
      ev.stopPropagation();
      callbacks.onDropChannel(config.scopeId, droppedChannelId);
    }
  };

  const statusMeta = triggerStateLabel(runtimeTriggerState);
  const analogTracks = config.tracks.filter((t) => t.lane === "analog");
  const wheelMode = config.wheelZoomTarget ?? "x";
  const editingTrack =
    editingChannelId !== null
      ? config.tracks.find((tr) => tr.channelId === editingChannelId) ?? null
      : null;

  const updateTrackProps = (
    channelId: number,
    patch: { yScale?: number; yOffset?: number; colorRgba?: number; visible?: boolean },
  ) => {
    callbacks.onUpdateScope(config.scopeId, (prev) => ({
      ...prev,
      tracks: prev.tracks.map((tr) =>
        tr.channelId === channelId ? { ...tr, ...patch } : tr,
      ),
    }));
  };

  const handleAutoFitY = () => {
    const scopeData = pendingScopeRef.current;
    let minAxis = Infinity;
    let maxAxis = -Infinity;
    for (const tr of analogTracks) {
      if (!tr.visible) continue;
      const blk = scopeData?.tracks.find((b) => b.channelId === tr.channelId);
      if (blk?.stats) {
        const aMin = channelValueToAxis(blk.stats.min, tr.yScale, tr.yOffset);
        const aMax = channelValueToAxis(blk.stats.max, tr.yScale, tr.yOffset);
        minAxis = Math.min(minAxis, aMin, aMax);
        maxAxis = Math.max(maxAxis, aMin, aMax);
      } else {
        const snap = latestSnapValsRef.current.get(tr.channelId);
        if (snap) {
          const aVal = channelValueToAxis(snap.val, tr.yScale, tr.yOffset);
          minAxis = Math.min(minAxis, aVal - 5);
          maxAxis = Math.max(maxAxis, aVal + 5);
        }
      }
    }
    if (!Number.isFinite(minAxis) || !Number.isFinite(maxAxis)) return;
    const span = Math.max(0.5, maxAxis - minAxis);
    const nextOffset = Number(((minAxis + maxAxis) * 0.5).toFixed(2));
    const nextScale = Number(clamp(span / 6.2, 0.01, 100_000).toFixed(2));
    callbacks.onUpdateScope(config.scopeId, (prev) => ({
      ...prev,
      yAxisScale: nextScale,
      yAxisOffset: nextOffset,
    }));
  };

  return (
    <div
      ref={rootRef}
      className={`scope-widget ${isFocused ? "scope-widget-focused" : ""} ${dropActive ? "scope-widget-drop-active" : ""}`}
      data-drop-scope-id={config.scopeId}
      data-testid={`scope-widget-${config.scopeId}`}
      onMouseDown={() => onFocusScope?.(config.scopeId)}
      onDragOver={handleDragOver}
      onDragLeave={handleDragLeave}
      onDrop={handleDrop}
    >
      {/* Top Instrument Control Bar */}
      <div className="scope-toolbar">
        <div className="scope-toolbar-group">
          <div className="scope-segmented" role="group" aria-label={t("scope.sweepModeAria")}>
            {SWEEP_MODES.map((m) => (
              <button
                key={m.id}
                type="button"
                className={`scope-seg-btn ${config.sweepMode === m.id ? "active" : ""}`}
                onClick={() => {
                  pausedHostNowNsRef.current = null;
                  callbacks.onUpdateScope(config.scopeId, (prev) => ({
                    ...prev,
                    sweepMode: m.id,
                    panOffsetNs: 0,
                  }));
                }}
              >
                {t(m.labelKey)}
              </button>
            ))}
          </div>

          <span
            className={`scope-status-pill scope-status-${statusMeta.tone}`}
            data-testid={`scope-status-${config.scopeId}`}
          >
            {t(statusMeta.labelKey)}
          </span>

          {(config.sweepMode === "single" ||
            runtimeTriggerState === TriggerRuntimeStateCode.LatchedPinned) && (
            <button
              type="button"
              className="scope-btn scope-btn-accent"
              data-testid={`scope-rearm-${config.scopeId}`}
              onClick={() => {
                pausedHostNowNsRef.current = null;
                callbacks.onRearmScope(config.scopeId);
              }}
            >
              {t("scope.rearm")}
            </button>
          )}

          <button
            type="button"
            className={`scope-btn ${config.paused ? "scope-btn-warn" : ""}`}
            onClick={() => {
              const nextPaused = !config.paused;
              if (nextPaused) {
                const sd = pendingScopeRef.current;
                pausedHostNowNsRef.current =
                  latestHeaderRef.current?.hostNowNs ??
                  (sd ? sd.winTStartNs + sd.winTSpanNs : null);
              } else {
                pausedHostNowNsRef.current = null;
              }
              callbacks.onUpdateScope(config.scopeId, (prev) => ({
                ...prev,
                paused: nextPaused,
              }));
            }}
          >
            {config.paused ? t("scope.resume") : t("scope.pause")}
          </button>

          {config.panOffsetNs !== 0 && (
            <button
              type="button"
              className="scope-btn"
              title={t("scope.zeroPanTitle")}
              onClick={() => {
                callbacks.onUpdateScope(config.scopeId, (prev) => ({
                  ...prev,
                  panOffsetNs: 0,
                }));
                if (config.syncGroupId !== null) {
                  callbacks.onSyncGroupBroadcast(config.syncGroupId, config.scopeId, {
                    panOffsetNs: 0,
                  });
                }
              }}
            >
              {t("scope.zeroPan")}
            </button>
          )}
        </div>

        <div className="scope-toolbar-group">
          <div
            className="scope-segmented"
            role="group"
            title={t("scope.wheelTargetTitle")}
            aria-label={t("scope.wheelTargetAria")}
          >
            {(["x", "y", "xy"] as ReadonlyArray<WheelZoomTarget>).map((mode) => (
              <button
                key={mode}
                type="button"
                className={`scope-seg-btn ${wheelMode === mode ? "active" : ""}`}
                data-testid={`scope-wheel-${config.scopeId}-${mode}`}
                onClick={() =>
                  callbacks.onUpdateScope(config.scopeId, (prev) => ({
                    ...prev,
                    wheelZoomTarget: mode,
                  }))
                }
              >
                {mode.toUpperCase()}
              </button>
            ))}
          </div>

          <button
            type="button"
            className="scope-btn"
            title={t("scope.autoYTitle")}
            data-testid={`scope-auto-y-${config.scopeId}`}
            onClick={handleAutoFitY}
          >
            {t("scope.autoY")}
          </button>

          {Math.abs(config.yAxisOffset) > 1e-4 && (
            <button
              type="button"
              className="scope-btn"
              title={t("scope.zeroYTitle")}
              onClick={() =>
                callbacks.onUpdateScope(config.scopeId, (prev) => ({
                  ...prev,
                  yAxisOffset: 0,
                }))
              }
            >
              {t("scope.zeroY")}
            </button>
          )}

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
            {config.analogLayout === "overlay" ? t("scope.overlay") : t("scope.stacked")}
          </button>

          <button
            type="button"
            className={`scope-btn ${config.cursorsEnabled ? "active" : ""}`}
            data-testid={`scope-cursors-${config.scopeId}`}
            onClick={() => {
              const nextEnabled = !config.cursorsEnabled;
              callbacks.onUpdateScope(config.scopeId, (prev) => ({
                ...prev,
                cursorsEnabled: nextEnabled,
              }));
              if (config.syncGroupId !== null) {
                callbacks.onSyncGroupBroadcast(config.syncGroupId, config.scopeId, {
                  cursorsEnabled: nextEnabled,
                  cursorARatio: config.cursorARatio,
                  cursorBRatio: config.cursorBRatio,
                });
              }
            }}
          >
            {t("scope.cursors")}
          </button>

          <select
            className="scope-select"
            aria-label={t("scope.syncAria")}
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
                {t(opt.labelKey)}
              </option>
            ))}
          </select>

          <select
            className="scope-select"
            aria-label={t("scope.spanAria")}
            value={String(config.timeSpanNs)}
            onChange={(ev) => {
              const nextSpan = Number(ev.target.value);
              const bounds = computePanBoundsForConfig(config, nextSpan);
              const clampedPan = Math.round(
                clamp(config.panOffsetNs, bounds.minPanNs, bounds.maxPanNs),
              );
              callbacks.onUpdateScope(config.scopeId, (prev) => ({
                ...prev,
                timeSpanNs: nextSpan,
                panOffsetNs: clampedPan,
              }));
              if (config.syncGroupId !== null) {
                callbacks.onSyncGroupBroadcast(config.syncGroupId, config.scopeId, {
                  timeSpanNs: nextSpan,
                  panOffsetNs: clampedPan,
                });
              }
            }}
          >
            {Array.from(
              new Set([
                1_000,
                2_000,
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
                config.timeSpanNs,
              ]),
            )
              .sort((a, b) => a - b)
              .map((span) => (
              <option key={span} value={String(span)}>
                {t("scope.spanPrefix")}: {formatTimeNs(span)}
              </option>
            ))}
          </select>
        </div>
      </div>

      {/* Trigger & Bound Channel Strip */}
      <div className="scope-subbar">
        {config.sweepMode !== "roll" && (
          <div className="scope-trigger-controls">
            <span className="scope-sublabel">{t("scope.trigLabel")}</span>
            <select
              className="scope-select-sm"
              aria-label={t("scope.trigChannelAria")}
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
                          level:
                            ch?.local_path === "v_bus" ||
                            (ch?.source_id === "sim0" && ch?.local_path === "ch3")
                              ? 85
                              : 0,
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
                  aria-label={t("scope.trigSlopeAria")}
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
                  <option value="rising">{t("scope.slopeRising")}</option>
                  <option value="falling">{t("scope.slopeFalling")}</option>
                  <option value="either">{t("scope.slopeEither")}</option>
                </select>
                <label className="scope-inline-num">
                  {t("scope.lvl")}
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
                  {t("scope.hys")}
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
                {t("scope.stateCode")}
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
          {config.tracks.map((trTrack) => {
            const ch = channelsById[trTrack.channelId];
            const label = ch ? ch.full_path : `ch${trTrack.channelId}`;
            const isActive =
              trTrack.lane === "analog" &&
              (config.activeAnalogChannelId === trTrack.channelId ||
                (config.activeAnalogChannelId === null && analogTracks[0]?.channelId === trTrack.channelId));
            const isEditing = editingChannelId === trTrack.channelId;
            return (
              <div
                key={trTrack.channelId}
                className={`scope-ch-chip ${!trTrack.visible ? "muted" : ""} ${isActive ? "active" : ""} ${isEditing ? "editing" : ""}`}
                onClick={() => {
                  if (trTrack.lane === "analog") {
                    callbacks.onUpdateScope(config.scopeId, (prev) => ({
                      ...prev,
                      activeAnalogChannelId: trTrack.channelId,
                    }));
                  }
                }}
              >
                <button
                  type="button"
                  className={`scope-ch-dot scope-ch-dot-btn ${isEditing ? "active" : ""}`}
                  style={{ backgroundColor: rgbaNumberToCss(trTrack.colorRgba, 1) }}
                  title={t("scope.channelColorTitle")}
                  aria-label={t("scope.channelColorTitle")}
                  data-testid={`ch-color-btn-${config.scopeId}-${trTrack.channelId}`}
                  onClick={(ev) => {
                    ev.stopPropagation();
                    setEditingChannelId((prev) =>
                      prev === trTrack.channelId ? null : trTrack.channelId,
                    );
                  }}
                />
                <span className="scope-ch-name">{label}</span>
                <span
                  className="scope-ch-live"
                  data-testid={`scope-ch-live-${config.scopeId}-${trTrack.channelId}`}
                  ref={(el) => {
                    if (el) chipValueRefs.current.set(trTrack.channelId, el);
                    else chipValueRefs.current.delete(trTrack.channelId);
                  }}
                >
                  --
                </span>
                {trTrack.lane === "analog" && (
                  <button
                    type="button"
                    className={`scope-ch-cond-btn ${isEditing ? "active" : ""}`}
                    title={t("scope.channelCondTitle")}
                    data-testid={`ch-cond-btn-${config.scopeId}-${trTrack.channelId}`}
                    onClick={(ev) => {
                      ev.stopPropagation();
                      setEditingChannelId((prev) =>
                        prev === trTrack.channelId ? null : trTrack.channelId,
                      );
                    }}
                  >
                    {formatConditionSummary(trTrack.yScale, trTrack.yOffset)}
                  </button>
                )}
                <button
                  type="button"
                  className="scope-ch-vis"
                  title={trTrack.visible ? t("scope.hideTrack") : t("scope.showTrack")}
                  onClick={(ev) => {
                    ev.stopPropagation();
                    callbacks.onUpdateScope(config.scopeId, (prev) => ({
                      ...prev,
                      tracks: prev.tracks.map((tr) =>
                        tr.channelId === trTrack.channelId ? { ...tr, visible: !tr.visible } : tr,
                      ),
                    }));
                  }}
                >
                  {trTrack.visible ? "◉" : "○"}
                </button>
                <button
                  type="button"
                  className="scope-ch-del"
                  title={t("scope.unbindTrack")}
                  onClick={(ev) => {
                    ev.stopPropagation();
                    if (editingChannelId === trTrack.channelId) {
                      setEditingChannelId(null);
                    }
                    callbacks.onUpdateScope(config.scopeId, (prev) => ({
                      ...prev,
                      tracks: prev.tracks.filter((tr) => tr.channelId !== trTrack.channelId),
                    }));
                  }}
                >
                  ×
                </button>
              </div>
            );
          })}
          {config.tracks.length === 0 && (
            <span className="scope-empty-hint">{t("scope.emptyTracksHint")}</span>
          )}
        </div>

        {/* Per-channel independent Scale / Offset & Color editor bar */}
        {editingTrack && (
          <div
            className="scope-ch-cond-bar"
            data-testid={`scope-ch-cond-bar-${config.scopeId}-${editingTrack.channelId}`}
          >
            <span className="scope-sublabel">
              {channelsById[editingTrack.channelId]?.full_path ?? `ch${editingTrack.channelId}`} ·{" "}
              {editingTrack.lane === "analog"
                ? t("scope.channelCondLabel")
                : t("scope.channelColorLabel")}
            </span>
            {editingTrack.lane === "analog" && (
              <>
                <label className="scope-inline-num">
                  {t("scope.chScale")}
                  <input
                    type="number"
                    step="0.1"
                    value={editingTrack.yScale}
                    onChange={(ev) => {
                      const val = Number.parseFloat(ev.target.value);
                      if (!Number.isFinite(val)) return;
                      updateTrackProps(editingTrack.channelId, { yScale: val });
                    }}
                  />
                </label>
                <button
                  type="button"
                  className="scope-btn-xs"
                  onClick={() =>
                    updateTrackProps(editingTrack.channelId, {
                      yScale: Number((editingTrack.yScale * 0.5).toFixed(4)),
                    })
                  }
                >
                  ×0.5
                </button>
                <button
                  type="button"
                  className="scope-btn-xs"
                  onClick={() =>
                    updateTrackProps(editingTrack.channelId, {
                      yScale: Number((editingTrack.yScale * 2).toFixed(4)),
                    })
                  }
                >
                  ×2
                </button>

                <label className="scope-inline-num">
                  {t("scope.chOffset")}
                  <input
                    type="number"
                    step="1"
                    value={editingTrack.yOffset}
                    onChange={(ev) => {
                      const val = Number.parseFloat(ev.target.value);
                      if (!Number.isFinite(val)) return;
                      updateTrackProps(editingTrack.channelId, { yOffset: val });
                    }}
                  />
                </label>
                <button
                  type="button"
                  className="scope-btn-xs"
                  onClick={() =>
                    updateTrackProps(editingTrack.channelId, { yScale: 1, yOffset: 0 })
                  }
                >
                  {t("scope.chReset")}
                </button>
              </>
            )}

            <div className="scope-color-swatches">
              {CHIP_TRACE_COLORS.map((c) => (
                <button
                  key={c}
                  type="button"
                  className={`scope-color-swatch ${(editingTrack.colorRgba >>> 0) === c ? "active" : ""}`}
                  style={{ backgroundColor: rgbaNumberToCss(c, 1) }}
                  aria-label="Trace color"
                  data-testid={`ch-swatch-${config.scopeId}-${editingTrack.channelId}-${rgbaNumberToHex(c).slice(1)}`}
                  onClick={() => updateTrackProps(editingTrack.channelId, { colorRgba: c })}
                />
              ))}
            </div>

            <label className="scope-custom-color-picker">
              <span>{t("scope.customColor")}</span>
              <input
                type="color"
                className="scope-color-input"
                data-testid={`ch-color-input-${config.scopeId}-${editingTrack.channelId}`}
                value={rgbaNumberToHex(editingTrack.colorRgba)}
                onChange={(ev) =>
                  updateTrackProps(editingTrack.channelId, {
                    colorRgba: hexToRgbaNumber(ev.target.value),
                  })
                }
              />
              <span className="scope-color-hex">
                {rgbaNumberToHex(editingTrack.colorRgba).toUpperCase()}
              </span>
            </label>

            <button
              type="button"
              className="scope-btn-xs"
              onClick={() => setEditingChannelId(null)}
            >
              {t("scope.closeCond")}
            </button>
          </div>
        )}
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
          <div className="scope-drop-banner">{t("scope.dropBanner")}</div>
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
          <em>{t("scope.meas.mean")}</em> <strong ref={measMeanRef}>--</strong>
        </span>
        <span className="scope-meas-item">
          <em>{t("scope.meas.freq")}</em> <strong ref={measFreqRef}>--</strong>
        </span>
        <span className="scope-meas-item">
          <em>{t("scope.meas.duty")}</em> <strong ref={measDutyRef}>--</strong>
        </span>
      </div>
    </div>
  );
}
