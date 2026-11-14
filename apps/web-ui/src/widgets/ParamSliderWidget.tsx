import { useState } from "react";
import type {
  DownlinkReceiptDto,
  SendDownlinkRequestDto_Deserialize,
} from "../bridge/bindings.ts";
import type { UiLocale } from "../i18n/index.ts";
import { translate } from "../i18n/index.ts";
import type { DashboardWidgetItem, WidgetSliderConfig } from "./types.ts";

export interface ParamSliderWidgetProps {
  widget: DashboardWidgetItem;
  sourceIds: string[];
  locale: UiLocale;
  onUpdateWidget: (updater: (prev: DashboardWidgetItem) => DashboardWidgetItem) => void;
  onSendDownlink: (
    req: SendDownlinkRequestDto_Deserialize,
    labelSummary?: string,
    isSliderEvent?: boolean,
  ) => Promise<DownlinkReceiptDto>;
}

function getSliderCfg(widget: DashboardWidgetItem, sourceIds: string[]): WidgetSliderConfig {
  return (
    widget.sliderConfig ?? {
      sourceId: sourceIds[0] ?? "sim0",
      key: "kp",
      min: 0.2,
      max: 6.0,
      step: 0.05,
      value: 1.2,
    }
  );
}

export function ParamSliderWidget({
  widget,
  sourceIds,
  locale,
  onUpdateWidget,
  onSendDownlink,
}: ParamSliderWidgetProps) {
  const t = (key: Parameters<typeof translate>[1], params?: Record<string, string | number>) =>
    translate(locale, key, params);

  const [configOpen, setConfigOpen] = useState(false);
  const cfg = getSliderCfg(widget, sourceIds);

  const updateCfg = (patch: Partial<WidgetSliderConfig>) => {
    onUpdateWidget((prev) => ({
      ...prev,
      sliderConfig: {
        ...getSliderCfg(prev, sourceIds),
        ...patch,
      },
    }));
  };

  const commitValue = (nextVal: number) => {
    if (!Number.isFinite(nextVal)) return;
    const clamped = Math.max(cfg.min, Math.min(cfg.max, nextVal));
    updateCfg({ value: clamped });
    const key = cfg.key.trim() || "kp";
    void onSendDownlink(
      {
        source_id: cfg.sourceId || sourceIds[0] || "sim0",
        priority: "normal",
        coalesce_key: `${cfg.sourceId}:${key}`,
        payload: { kind: "key_value", key, value: clamped },
      },
      `${key}=${clamped}`,
      true,
    );
  };

  const decimals = cfg.step < 1 ? Math.min(3, Math.max(1, String(cfg.step).split(".")[1]?.length ?? 2)) : 0;

  return (
    <div className="widget-slider-body" data-testid={`widget-slider-${widget.id}`}>
      <div className="widget-slider-top">
        <span className="widget-ch-badge">
          {cfg.sourceId}:{cfg.key}
        </span>
        <span className="widget-slider-val">{cfg.value.toFixed(decimals)}</span>
        <button
          type="button"
          className="scope-btn-xs"
          onClick={() => setConfigOpen((v) => !v)}
        >
          {t("widget.configToggle")}
        </button>
      </div>

      <div className="widget-slider-controls">
        <input
          type="range"
          min={cfg.min}
          max={cfg.max}
          step={cfg.step}
          value={cfg.value}
          onChange={(ev) => commitValue(Number.parseFloat(ev.target.value))}
        />
        <input
          type="number"
          className="widget-num-input"
          min={cfg.min}
          max={cfg.max}
          step={cfg.step}
          value={cfg.value}
          onChange={(ev) => commitValue(Number.parseFloat(ev.target.value))}
        />
      </div>

      {configOpen && (
        <div className="widget-inline-config">
          <label>
            {t("widget.slider.source")}
            <select
              value={cfg.sourceId}
              onChange={(ev) => updateCfg({ sourceId: ev.target.value })}
            >
              {(sourceIds.length > 0 ? sourceIds : [cfg.sourceId]).map((id) => (
                <option key={id} value={id}>
                  {id}
                </option>
              ))}
            </select>
          </label>
          <label>
            {t("widget.slider.key")}
            <input
              type="text"
              value={cfg.key}
              onChange={(ev) => updateCfg({ key: ev.target.value })}
            />
          </label>
          <label>
            {t("widget.rangeMin")}
            <input
              type="number"
              step="any"
              value={cfg.min}
              onChange={(ev) => {
                const min = Number.parseFloat(ev.target.value);
                if (Number.isFinite(min)) updateCfg({ min });
              }}
            />
          </label>
          <label>
            {t("widget.rangeMax")}
            <input
              type="number"
              step="any"
              value={cfg.max}
              onChange={(ev) => {
                const max = Number.parseFloat(ev.target.value);
                if (Number.isFinite(max)) updateCfg({ max });
              }}
            />
          </label>
        </div>
      )}
    </div>
  );
}
