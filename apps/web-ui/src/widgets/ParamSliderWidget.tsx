import { useState } from "react";
import type {
  DownlinkReceiptDto,
  SendDownlinkRequestDto_Deserialize,
} from "../bridge/bindings.ts";
import type { UiLocale } from "../i18n/index.ts";
import { translate } from "../i18n/index.ts";
import {
  buildEffectiveCommandArgs,
  compileCommandPayload,
  extractCommandPlaceholders,
  type CommandDefinition,
} from "../workspace/command-system.ts";
import type { DashboardWidgetItem, WidgetSliderConfig } from "./types.ts";
import { WidgetConfigModal } from "./WidgetConfigModal.tsx";
import { WidgetHeaderActions } from "./WidgetHeaderSlot.tsx";

export interface ParamSliderWidgetProps {
  widget: DashboardWidgetItem;
  sourceIds: string[];
  commands: CommandDefinition[];
  locale: UiLocale;
  onUpdateWidget: (updater: (prev: DashboardWidgetItem) => DashboardWidgetItem) => void;
  onBindCommand?: (commandId: string | null, argIndex?: number) => void;
  onSetCommandArgValue?: (
    commandId: string,
    argIndex: number,
    value: number,
    sourceWidgetId?: string,
  ) => void;
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
      commandId: null,
    }
  );
}

export function ParamSliderWidget({
  widget,
  sourceIds,
  commands,
  locale,
  onUpdateWidget,
  onBindCommand,
  onSetCommandArgValue,
  onSendDownlink,
}: ParamSliderWidgetProps) {
  const t = (key: Parameters<typeof translate>[1], params?: Record<string, string | number>) =>
    translate(locale, key, params);

  const [configOpen, setConfigOpen] = useState(false);
  const [cmdError, setCmdError] = useState<string | null>(null);
  const cfg = getSliderCfg(widget, sourceIds);
  const boundCommand = cfg.commandId
    ? commands.find((c) => c.id === cfg.commandId) ?? null
    : null;
  const placeholders = boundCommand ? extractCommandPlaceholders(boundCommand) : [];
  const effectiveSlot =
    placeholders.length > 0
      ? Math.min(Math.max(0, Math.trunc(cfg.commandArgIndex ?? 0)), placeholders.length - 1)
      : 0;
  const effectiveTarget =
    boundCommand?.targetSourceId.trim() || cfg.sourceId || sourceIds[0] || "sim0";
  const compiledPreview = boundCommand
    ? compileCommandPayload(
        boundCommand,
        placeholders.length > 0
          ? buildEffectiveCommandArgs(boundCommand, {
              slotIndex: effectiveSlot,
              value: cfg.value,
            })
          : 0,
      )
    : null;

  const updateCfg = (patch: Partial<WidgetSliderConfig>) => {
    onUpdateWidget((prev) => ({
      ...prev,
      sliderConfig: {
        ...getSliderCfg(prev, sourceIds),
        ...patch,
      },
    }));
  };

  const handleSlotChange = (nextSlot: number) => {
    if (!boundCommand || !Number.isFinite(nextSlot)) return;
    if (onBindCommand) {
      onBindCommand(boundCommand.id, nextSlot);
    } else {
      updateCfg({ commandArgIndex: nextSlot });
      onSetCommandArgValue?.(boundCommand.id, nextSlot, cfg.value, widget.id);
    }
  };

  const commitValue = (nextVal: number, forceSend = false) => {
    if (!Number.isFinite(nextVal)) return;
    const clamped = Math.max(cfg.min, Math.min(cfg.max, nextVal));
    updateCfg({ value: clamped });

    if (boundCommand) {
      if (placeholders.length > 0) {
        onSetCommandArgValue?.(boundCommand.id, effectiveSlot, clamped, widget.id);
      }
      const compiled = compileCommandPayload(
        boundCommand,
        placeholders.length > 0
          ? buildEffectiveCommandArgs(boundCommand, {
              slotIndex: effectiveSlot,
              value: clamped,
            })
          : 0,
      );
      if (!compiled.ok) {
        setCmdError(compiled.error);
        return;
      }
      setCmdError(null);
      if (cfg.autoSend === false && !forceSend) {
        return;
      }
      void onSendDownlink(
        {
          source_id: effectiveTarget,
          priority: boundCommand.priority,
          coalesce_key:
            boundCommand.priority === "normal"
              ? `${effectiveTarget}:cmd:${boundCommand.id}`
              : null,
          payload: compiled.payload,
        },
        `${boundCommand.name}: ${compiled.preview}`,
        true,
      );
      return;
    }

    setCmdError(null);
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

  const handleSelectCommand = (nextId: string | null) => {
    setCmdError(null);
    if (onBindCommand) {
      onBindCommand(nextId);
      return;
    }
    const nextCmd = nextId ? commands.find((c) => c.id === nextId) ?? null : null;
    onUpdateWidget((prev) => ({
      ...prev,
      ...(nextCmd ? { title: nextCmd.name } : {}),
      sliderConfig: {
        ...getSliderCfg(prev, sourceIds),
        commandId: nextId,
        ...(nextCmd?.targetSourceId ? { sourceId: nextCmd.targetSourceId } : {}),
      },
    }));
  };

  return (
    <div className="widget-slider-body" data-testid={`widget-slider-${widget.id}`}>
      <WidgetHeaderActions>
        {boundCommand && placeholders.length > 1 && (
          <select
            className="widget-header-slot-select"
            data-testid={`widget-slot-select-${widget.id}`}
            title={t("widget.cmdPlaceholderLabel")}
            value={String(effectiveSlot)}
            onPointerDown={(ev) => ev.stopPropagation()}
            onClick={(ev) => ev.stopPropagation()}
            onChange={(ev) => handleSlotChange(Number.parseInt(ev.target.value, 10))}
          >
            {placeholders.map((ph) => (
              <option key={ph.index} value={String(ph.index)}>
                {ph.label}
              </option>
            ))}
          </select>
        )}
        {boundCommand && (
          <button
            type="button"
            className="widget-header-btn is-accent"
            title={t("widget.btn.send")}
            onClick={() => commitValue(cfg.value, true)}
          >
            ⚡
          </button>
        )}
        <button
          type="button"
          className={`widget-header-btn ${configOpen ? "active" : ""}`}
          title={t("widget.configToggle")}
          data-testid={`slider-config-btn-${widget.id}`}
          onClick={() => setConfigOpen((v) => !v)}
        >
          ⚙
        </button>
      </WidgetHeaderActions>

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

      {boundCommand && (
        <div
          className={`widget-cmd-preview ${
            cmdError || !compiledPreview?.ok ? "is-error" : ""
          }`}
        >
          {cmdError
            ? cmdError
            : compiledPreview?.ok
              ? compiledPreview.preview
              : compiledPreview?.error}
        </div>
      )}

      <WidgetConfigModal
        open={configOpen}
        title={`${widget.title} · ${t("widget.configToggle")}`}
        doneLabel={t("widget.configDone")}
        width="sm"
        onClose={() => setConfigOpen(false)}
      >
        <div className="widget-inline-config">
            <label style={{ gridColumn: "span 2" }}>
              {t("widget.cmdBindLabel")}
              <select
                data-testid={`slider-cmd-select-${widget.id}`}
                value={cfg.commandId ?? ""}
                onChange={(ev) => handleSelectCommand(ev.target.value || null)}
              >
                <option value="">{t("widget.cmdBindNoneKv")}</option>
                {commands.map((cmd) => (
                  <option key={cmd.id} value={cmd.id}>
                    {cmd.name} ({cmd.format.toUpperCase()})
                  </option>
                ))}
              </select>
            </label>
            {boundCommand && placeholders.length > 0 && (
              <label style={{ gridColumn: "span 2" }}>
                {t("widget.cmdPlaceholderLabel")}
                <select
                  data-testid={`slider-arg-select-${widget.id}`}
                  value={String(effectiveSlot)}
                  onChange={(ev) => handleSlotChange(Number.parseInt(ev.target.value, 10))}
                >
                  {placeholders.map((ph) => (
                    <option key={ph.index} value={String(ph.index)}>
                      {t("widget.cmdPlaceholderSlot", {
                        num: ph.index + 1,
                        token: ph.token,
                      })}
                    </option>
                  ))}
                </select>
              </label>
            )}
            {boundCommand && (
              <label style={{ gridColumn: "span 2", flexDirection: "row", alignItems: "center", gap: 6 }}>
                <input
                  type="checkbox"
                  checked={cfg.autoSend !== false}
                  onChange={(ev) => updateCfg({ autoSend: ev.target.checked })}
                />
                <span>{t("widget.slider.autoSend")}</span>
              </label>
            )}
            {(!boundCommand || !boundCommand.targetSourceId) && (
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
            )}
            {!boundCommand && (
              <label>
                {t("widget.slider.key")}
                <input
                  type="text"
                  value={cfg.key}
                  onChange={(ev) => updateCfg({ key: ev.target.value })}
                />
              </label>
            )}
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
            <label>
              {t("widget.slider.step")}
              <input
                type="number"
                step="any"
                min="0.0001"
                value={cfg.step}
                onChange={(ev) => {
                  const step = Number.parseFloat(ev.target.value);
                  if (Number.isFinite(step) && step > 0) updateCfg({ step });
                }}
              />
            </label>
          </div>
      </WidgetConfigModal>
    </div>
  );
}
