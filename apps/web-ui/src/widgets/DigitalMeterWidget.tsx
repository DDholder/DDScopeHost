import { useEffect, useRef, useState } from "react";
import type { ChannelNodeDto_Serialize } from "../bridge/bindings.ts";
import { bridgeClient } from "../bridge/control-client.ts";
import { DdsfSnapshotKind } from "../bridge/ddsf-spec.ts";
import type { UiLocale } from "../i18n/index.ts";
import { translate } from "../i18n/index.ts";
import { channelValueToAxis } from "../scope-engine/layout.ts";
import { formatMetricValue } from "../scope-engine/time-format.ts";
import { ChannelBindSelect } from "./ChannelBindSelect.tsx";
import type { DashboardWidgetItem, WidgetChannelBinding } from "./types.ts";
import { WidgetConfigModal } from "./WidgetConfigModal.tsx";
import { WidgetHeaderActions } from "./WidgetHeaderSlot.tsx";

export interface DigitalMeterWidgetProps {
  widget: DashboardWidgetItem;
  channelsById: Record<number, ChannelNodeDto_Serialize>;
  locale: UiLocale;
  onUpdateWidget: (updater: (prev: DashboardWidgetItem) => DashboardWidgetItem) => void;
  onBindChannel?: (channelId: number) => void;
}

export function DigitalMeterWidget({
  widget,
  channelsById,
  locale,
  onUpdateWidget,
  onBindChannel,
}: DigitalMeterWidgetProps) {
  const t = (key: Parameters<typeof translate>[1], params?: Record<string, string | number>) =>
    translate(locale, key, params);

  const [configOpen, setConfigOpen] = useState(false);
  const valRef = useRef<HTMLDivElement | null>(null);
  const rawRef = useRef<HTMLSpanElement | null>(null);
  const minRef = useRef<HTMLSpanElement | null>(null);
  const maxRef = useRef<HTMLSpanElement | null>(null);
  const sparkCanvasRef = useRef<HTMLCanvasElement | null>(null);

  const binding: WidgetChannelBinding | null = widget.primaryBinding;
  const bindingRef = useRef(binding);
  bindingRef.current = binding;
  const ch = binding ? channelsById[binding.channelId] : undefined;
  const chRef = useRef(ch);
  chRef.current = ch;
  const localeRef = useRef(locale);
  localeRef.current = locale;

  const peaksRef = useRef<{ min: number; max: number; history: number[] }>({
    min: Infinity,
    max: -Infinity,
    history: [],
  });

  useEffect(() => {
    peaksRef.current = { min: Infinity, max: -Infinity, history: [] };
  }, [binding?.channelId, binding?.scale, binding?.offset]);

  useEffect(() => {
    const chId = binding?.channelId;
    if (chId === undefined) return;

    const applySample = (rawVal: number, kind: number) => {
      const b = bindingRef.current;
      if (!b) return;
      const curCh = chRef.current;
      const scaled = channelValueToAxis(rawVal, b.scale, b.offset);

      const peaks = peaksRef.current;
      if (scaled < peaks.min) peaks.min = scaled;
      if (scaled > peaks.max) peaks.max = scaled;
      peaks.history.push(scaled);
      if (peaks.history.length > 96) {
        peaks.history.shift();
      }

      if (valRef.current) {
        if (kind === DdsfSnapshotKind.Bool) {
          valRef.current.textContent =
            rawVal > 0.5
              ? translate(localeRef.current, "sidebar.valHigh")
              : translate(localeRef.current, "sidebar.valLow");
        } else if (kind === DdsfSnapshotKind.StateEnum) {
          const code = Math.round(rawVal);
          valRef.current.textContent =
            curCh?.state_labels.find((l) => l.code === code)?.name ?? `S${code}`;
        } else {
          valRef.current.textContent = formatMetricValue(scaled, curCh?.unit ?? null);
        }
      }
      if (rawRef.current) {
        rawRef.current.textContent = formatMetricValue(rawVal, curCh?.unit ?? null);
      }
      if (minRef.current && Number.isFinite(peaks.min)) {
        minRef.current.textContent = formatMetricValue(peaks.min, curCh?.unit ?? null);
      }
      if (maxRef.current && Number.isFinite(peaks.max)) {
        maxRef.current.textContent = formatMetricValue(peaks.max, curCh?.unit ?? null);
      }

      const cvs = sparkCanvasRef.current;
      if (cvs && peaks.history.length >= 2) {
        const w = Math.max(40, cvs.clientWidth || 160);
        const h = Math.max(18, cvs.clientHeight || 36);
        if (cvs.width !== w || cvs.height !== h) {
          cvs.width = w;
          cvs.height = h;
        }
        const ctx = cvs.getContext("2d");
        if (ctx) {
          ctx.clearRect(0, 0, w, h);
          let lo = Infinity;
          let hi = -Infinity;
          for (let i = 0; i < peaks.history.length; i += 1) {
            const v = peaks.history[i];
            if (v < lo) lo = v;
            if (v > hi) hi = v;
          }
          const span = Math.max(1e-6, hi - lo);
          ctx.strokeStyle = "#38bdf8";
          ctx.lineWidth = 1.5;
          ctx.beginPath();
          for (let i = 0; i < peaks.history.length; i += 1) {
            const px = (i / (peaks.history.length - 1)) * (w - 4) + 2;
            const py = h - 3 - ((peaks.history[i] - lo) / span) * (h - 6);
            if (i === 0) ctx.moveTo(px, py);
            else ctx.lineTo(px, py);
          }
          ctx.stroke();
        }
      }
    };

    const seed = bridgeClient.topicBus.getLatestSnapshot(chId);
    if (seed) {
      applySample(seed.valF64, seed.kind);
    }

    let lastFlushMs = 0;
    let latestVal = seed?.valF64 ?? 0;
    let latestKind = seed?.kind ?? DdsfSnapshotKind.Scalar;
    let hasPending = false;

    const unsub = bridgeClient.topicBus.subscribeSnapshots((snaps) => {
      for (let i = 0; i < snaps.length; i += 1) {
        if (snaps[i].channelNumId === chId) {
          latestVal = snaps[i].valF64;
          latestKind = snaps[i].kind;
          hasPending = true;
          break;
        }
      }
      if (!hasPending) return;
      const now = performance.now();
      if (now - lastFlushMs < 55) return;
      lastFlushMs = now;
      hasPending = false;
      applySample(latestVal, latestKind);
    });

    return unsub;
  }, [binding?.channelId, locale]);

  const headerControls = (
    <WidgetHeaderActions>
      {binding && (
        <button
          type="button"
          className="widget-header-btn"
          title={t("widget.resetPeaks")}
          onClick={() => {
            peaksRef.current = { min: Infinity, max: -Infinity, history: [] };
            if (minRef.current) minRef.current.textContent = "--";
            if (maxRef.current) maxRef.current.textContent = "--";
          }}
        >
          ↺
        </button>
      )}
      <button
        type="button"
        className={`widget-header-btn ${configOpen ? "active" : ""}`}
        title={t("widget.configToggle")}
        data-testid={`meter-config-btn-${widget.id}`}
        onClick={() => setConfigOpen((v) => !v)}
      >
        ⚙
      </button>
    </WidgetHeaderActions>
  );

  const configOverlay = (
    <WidgetConfigModal
      open={configOpen}
      title={`${widget.title} · ${t("widget.configToggle")}`}
      doneLabel={t("widget.configDone")}
      width="sm"
      onClose={() => setConfigOpen(false)}
    >
      <div className="widget-inline-config">
        <label style={{ gridColumn: "span 2" }}>
          {t("widget.bindChannelLabel")}
          <ChannelBindSelect
            selectedChannelId={binding?.channelId ?? null}
            channelsById={channelsById}
            placeholder={t("widget.selectChannel")}
            title={t("widget.switchChannelTitle")}
            className="widget-config-select"
            dataTestId={`meter-bind-select-${widget.id}`}
            onBindChannel={onBindChannel}
          />
        </label>
        {binding && (
          <>
            <label>
              {t("scope.chScale")}
              <input
                type="number"
                step="0.1"
                value={binding.scale}
                onChange={(ev) => {
                  const scale = Number.parseFloat(ev.target.value);
                  if (!Number.isFinite(scale)) return;
                  onUpdateWidget((prev) => ({
                    ...prev,
                    primaryBinding: prev.primaryBinding
                      ? { ...prev.primaryBinding, scale }
                      : null,
                  }));
                }}
              />
            </label>
            <label>
              {t("scope.chOffset")}
              <input
                type="number"
                step="1"
                value={binding.offset}
                onChange={(ev) => {
                  const offset = Number.parseFloat(ev.target.value);
                  if (!Number.isFinite(offset)) return;
                  onUpdateWidget((prev) => ({
                    ...prev,
                    primaryBinding: prev.primaryBinding
                      ? { ...prev.primaryBinding, offset }
                      : null,
                  }));
                }}
              />
            </label>
          </>
        )}
      </div>
    </WidgetConfigModal>
  );

  if (!binding) {
    return (
      <>
        {headerControls}
        <div className="widget-unbound-box">
          <span>{t("widget.unbound")}</span>
          {onBindChannel && (
            <button
              type="button"
              className="scope-btn-xs"
              onClick={() => setConfigOpen(true)}
            >
              {t("widget.selectChannel")}
            </button>
          )}
        </div>
        {configOverlay}
      </>
    );
  }

  return (
    <div className="widget-meter-body" data-testid={`widget-meter-${widget.id}`}>
      {headerControls}

      <div className="widget-meter-main" ref={valRef}>
        --
      </div>

      <canvas className="widget-sparkline" ref={sparkCanvasRef} />

      <div className="widget-meter-stats">
        <span>
          <em>{t("widget.raw")}:</em> <strong ref={rawRef}>--</strong>
        </span>
        <span>
          <em>{t("widget.min")}:</em> <strong ref={minRef}>--</strong>
        </span>
        <span>
          <em>{t("widget.max")}:</em> <strong ref={maxRef}>--</strong>
        </span>
      </div>

      {configOverlay}
    </div>
  );
}
