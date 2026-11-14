import { useEffect, useRef, useState } from "react";
import type { ChannelNodeDto_Serialize } from "../bridge/bindings.ts";
import { bridgeClient } from "../bridge/control-client.ts";
import type { UiLocale } from "../i18n/index.ts";
import { translate } from "../i18n/index.ts";
import { channelValueToAxis } from "../scope-engine/layout.ts";
import { formatMetricValue } from "../scope-engine/time-format.ts";
import { ChannelBindSelect } from "./ChannelBindSelect.tsx";
import type { DashboardWidgetItem } from "./types.ts";

export interface RadialGaugeWidgetProps {
  widget: DashboardWidgetItem;
  channelsById: Record<number, ChannelNodeDto_Serialize>;
  locale: UiLocale;
  onUpdateWidget: (updater: (prev: DashboardWidgetItem) => DashboardWidgetItem) => void;
  onBindChannel?: (channelId: number) => void;
}

const ARC_CIRCUMFERENCE = 2 * Math.PI * 40;
const ARC_SWEEP_FRAC = 240 / 360;
const ARC_MAX_LEN = ARC_CIRCUMFERENCE * ARC_SWEEP_FRAC;

export function RadialGaugeWidget({
  widget,
  channelsById,
  locale,
  onUpdateWidget,
  onBindChannel,
}: RadialGaugeWidgetProps) {
  const t = (key: Parameters<typeof translate>[1], params?: Record<string, string | number>) =>
    translate(locale, key, params);

  const [configOpen, setConfigOpen] = useState(false);
  const progressArcRef = useRef<SVGCircleElement | null>(null);
  const valueTextRef = useRef<SVGTextElement | null>(null);

  const binding = widget.primaryBinding;
  const gaugeCfg = widget.gaugeConfig ?? { min: -20, max: 20, warnHigh: 12 };
  const ch = binding ? channelsById[binding.channelId] : undefined;

  const stateRef = useRef({ binding, gaugeCfg, ch });
  stateRef.current = { binding, gaugeCfg, ch };

  useEffect(() => {
    const chId = binding?.channelId;
    if (chId === undefined) return;

    const applySample = (rawVal: number) => {
      const { binding: curB, gaugeCfg: cfg, ch: curCh } = stateRef.current;
      if (!curB) return;
      const val = channelValueToAxis(rawVal, curB.scale, curB.offset);
      const span = Math.max(1e-6, cfg.max - cfg.min);
      const norm = Math.max(0, Math.min(1, (val - cfg.min) / span));
      const dashLen = norm * ARC_MAX_LEN;
      const isWarn = val >= cfg.warnHigh || val <= cfg.min + span * 0.02;

      if (progressArcRef.current) {
        progressArcRef.current.setAttribute(
          "stroke-dasharray",
          `${dashLen.toFixed(2)} ${ARC_CIRCUMFERENCE.toFixed(2)}`,
        );
        progressArcRef.current.setAttribute("stroke", isWarn ? "#f43f5e" : "#38bdf8");
      }
      if (valueTextRef.current) {
        valueTextRef.current.textContent = formatMetricValue(val, curCh?.unit ?? null);
      }
    };

    const seed = bridgeClient.topicBus.getLatestSnapshot(chId);
    if (seed) {
      applySample(seed.valF64);
    }

    let lastFlushMs = 0;
    let latestVal = seed?.valF64 ?? 0;
    let pending = false;

    const unsub = bridgeClient.topicBus.subscribeSnapshots((snaps) => {
      for (let i = 0; i < snaps.length; i += 1) {
        if (snaps[i].channelNumId === chId) {
          latestVal = snaps[i].valF64;
          pending = true;
          break;
        }
      }
      if (!pending) return;
      const now = performance.now();
      if (now - lastFlushMs < 55) return;
      lastFlushMs = now;
      pending = false;
      applySample(latestVal);
    });
    return unsub;
  }, [binding?.channelId, binding?.scale, binding?.offset, gaugeCfg.min, gaugeCfg.max, gaugeCfg.warnHigh]);

  if (!binding) {
    return (
      <div className="widget-unbound-box">
        <span>{t("widget.unbound")}</span>
        <ChannelBindSelect
          channelsById={channelsById}
          placeholder={t("widget.selectChannel")}
          onBindChannel={onBindChannel}
        />
      </div>
    );
  }

  return (
    <div className="widget-gauge-body" data-testid={`widget-gauge-${widget.id}`}>
      <div className="widget-meter-top">
        <span className="widget-ch-badge">{ch ? ch.full_path : `ch${binding.channelId}`}</span>
        <button type="button" className="scope-btn-xs" onClick={() => setConfigOpen((v) => !v)}>
          {t("widget.configToggle")}
        </button>
      </div>

      <svg className="widget-gauge-svg" viewBox="0 0 110 86">
        <circle
          cx="55"
          cy="52"
          r="40"
          fill="none"
          stroke="#1e293b"
          strokeWidth="8"
          strokeLinecap="round"
          strokeDasharray={`${ARC_MAX_LEN.toFixed(2)} ${ARC_CIRCUMFERENCE.toFixed(2)}`}
          transform="rotate(150 55 52)"
        />
        <circle
          ref={progressArcRef}
          cx="55"
          cy="52"
          r="40"
          fill="none"
          stroke="#38bdf8"
          strokeWidth="8"
          strokeLinecap="round"
          strokeDasharray={`0 ${ARC_CIRCUMFERENCE.toFixed(2)}`}
          transform="rotate(150 55 52)"
        />
        <text
          ref={valueTextRef}
          x="55"
          y="53"
          textAnchor="middle"
          fill="#f8fafc"
          fontSize="13"
          fontWeight="700"
          fontFamily="JetBrains Mono, ui-monospace, monospace"
        >
          --
        </text>
        <text
          x="18"
          y="80"
          textAnchor="start"
          fill="#64748b"
          fontSize="8.5"
          fontFamily="JetBrains Mono, ui-monospace, monospace"
        >
          {gaugeCfg.min}
        </text>
        <text
          x="92"
          y="80"
          textAnchor="end"
          fill="#64748b"
          fontSize="8.5"
          fontFamily="JetBrains Mono, ui-monospace, monospace"
        >
          {gaugeCfg.max}
        </text>
      </svg>

      {configOpen && (
        <div className="widget-inline-config">
          <label>
            {t("widget.rangeMin")}
            <input
              type="number"
              value={gaugeCfg.min}
              onChange={(ev) => {
                const min = Number.parseFloat(ev.target.value);
                if (!Number.isFinite(min)) return;
                onUpdateWidget((prev) => ({
                  ...prev,
                  gaugeConfig: { ...(prev.gaugeConfig ?? gaugeCfg), min },
                }));
              }}
            />
          </label>
          <label>
            {t("widget.rangeMax")}
            <input
              type="number"
              value={gaugeCfg.max}
              onChange={(ev) => {
                const max = Number.parseFloat(ev.target.value);
                if (!Number.isFinite(max)) return;
                onUpdateWidget((prev) => ({
                  ...prev,
                  gaugeConfig: { ...(prev.gaugeConfig ?? gaugeCfg), max },
                }));
              }}
            />
          </label>
          <label>
            {t("widget.warnHigh")}
            <input
              type="number"
              value={gaugeCfg.warnHigh}
              onChange={(ev) => {
                const warnHigh = Number.parseFloat(ev.target.value);
                if (!Number.isFinite(warnHigh)) return;
                onUpdateWidget((prev) => ({
                  ...prev,
                  gaugeConfig: { ...(prev.gaugeConfig ?? gaugeCfg), warnHigh },
                }));
              }}
            />
          </label>
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
                  primaryBinding: prev.primaryBinding ? { ...prev.primaryBinding, scale } : null,
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
                  primaryBinding: prev.primaryBinding ? { ...prev.primaryBinding, offset } : null,
                }));
              }}
            />
          </label>
        </div>
      )}
    </div>
  );
}
