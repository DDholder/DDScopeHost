import { useState } from "react";
import type {
  DownlinkReceiptDto,
  SendDownlinkRequestDto_Deserialize,
} from "../bridge/bindings.ts";
import type { UiLocale } from "../i18n/index.ts";
import { translate } from "../i18n/index.ts";
import {
  commandUsesValue,
  compileCommandPayload,
  type CommandDefinition,
} from "../workspace/command-system.ts";
import type { DashboardWidgetItem, WidgetButtonConfig } from "./types.ts";

export interface CmdButtonWidgetProps {
  widget: DashboardWidgetItem;
  sourceIds: string[];
  commands: CommandDefinition[];
  locale: UiLocale;
  onUpdateWidget: (updater: (prev: DashboardWidgetItem) => DashboardWidgetItem) => void;
  onSendDownlink: (
    req: SendDownlinkRequestDto_Deserialize,
    labelSummary?: string,
    isSliderEvent?: boolean,
  ) => Promise<DownlinkReceiptDto>;
}

function getButtonCfg(widget: DashboardWidgetItem, sourceIds: string[]): WidgetButtonConfig {
  return (
    widget.buttonConfig ?? {
      sourceId: sourceIds[0] ?? "sim0",
      mode: "ascii",
      command: "SET_MODE=FOC_RUN",
      kvKey: "kp",
      kvValue: 1.5,
      priority: "high",
      commandId: null,
      commandValue: 0,
    }
  );
}

export function CmdButtonWidget({
  widget,
  sourceIds,
  commands,
  locale,
  onUpdateWidget,
  onSendDownlink,
}: CmdButtonWidgetProps) {
  const t = (key: Parameters<typeof translate>[1], params?: Record<string, string | number>) =>
    translate(locale, key, params);

  const [configOpen, setConfigOpen] = useState(false);
  const [statusMsg, setStatusMsg] = useState<string | null>(null);
  const [sending, setSending] = useState(false);
  const cfg = getButtonCfg(widget, sourceIds);
  const boundCommand = cfg.commandId
    ? commands.find((c) => c.id === cfg.commandId) ?? null
    : null;
  const usesValue = boundCommand ? commandUsesValue(boundCommand) : false;
  const commandValue = cfg.commandValue ?? 0;
  const effectiveTarget =
    boundCommand?.targetSourceId.trim() || cfg.sourceId || sourceIds[0] || "sim0";
  const compiled = boundCommand
    ? compileCommandPayload(boundCommand, commandValue)
    : null;

  const updateCfg = (patch: Partial<WidgetButtonConfig>) => {
    onUpdateWidget((prev) => ({
      ...prev,
      buttonConfig: {
        ...getButtonCfg(prev, sourceIds),
        ...patch,
      },
    }));
  };

  const payloadPreview = boundCommand
    ? compiled?.ok
      ? compiled.preview
      : compiled?.error ?? boundCommand.template
    : cfg.mode === "kv"
      ? `${cfg.kvKey}=${cfg.kvValue}`
      : cfg.mode === "hex"
        ? `HEX: ${cfg.command}`
        : cfg.command;

  const handleFire = async () => {
    setSending(true);
    setStatusMsg(null);
    const target = effectiveTarget;
    try {
      if (boundCommand) {
        const res = compileCommandPayload(boundCommand, cfg.commandValue ?? 0);
        if (!res.ok) {
          setStatusMsg(res.error);
          return;
        }
        const receipt = await onSendDownlink(
          {
            source_id: target,
            priority: boundCommand.priority,
            coalesce_key: null,
            payload: res.payload,
          },
          `${boundCommand.name}: ${res.preview}`,
          false,
        );
        setStatusMsg(`${t("widget.btn.sent")} · ${receipt.result_kind}`);
        return;
      }

      let req: SendDownlinkRequestDto_Deserialize;
      if (cfg.mode === "hex") {
        req = {
          source_id: target,
          priority: cfg.priority,
          coalesce_key: null,
          payload: { kind: "hex_bytes", hex: cfg.command.trim() },
        };
      } else if (cfg.mode === "kv") {
        const key = cfg.kvKey.trim() || "param";
        req = {
          source_id: target,
          priority: cfg.priority,
          coalesce_key: `${target}:${key}`,
          payload: { kind: "key_value", key, value: cfg.kvValue },
        };
      } else {
        req = {
          source_id: target,
          priority: cfg.priority,
          coalesce_key: null,
          payload: { kind: "ascii_text", text: cfg.command, append_newline: true },
        };
      }
      const receipt = await onSendDownlink(req, payloadPreview, false);
      setStatusMsg(`${t("widget.btn.sent")} · ${receipt.result_kind}`);
    } catch (err: unknown) {
      setStatusMsg(err instanceof Error ? err.message : String(err));
    } finally {
      setSending(false);
    }
  };

  return (
    <div className="widget-cmd-body" data-testid={`widget-cmd-${widget.id}`}>
      <div className="widget-cmd-top">
        {boundCommand ? (
          <span className="widget-ch-badge widget-cmd-bound-badge" title={boundCommand.template}>
            ⚡ {boundCommand.name} · {effectiveTarget} ·{" "}
            {boundCommand.format === "hex" ? "HEX" : "STR"}
          </span>
        ) : (
          <span className="widget-ch-badge">
            {cfg.sourceId} · {cfg.mode.toUpperCase()}
          </span>
        )}
        <button
          type="button"
          className="scope-btn-xs"
          onClick={() => setConfigOpen((v) => !v)}
        >
          {t("widget.configToggle")}
        </button>
      </div>

      <div className="widget-cmd-action-row">
        {boundCommand && usesValue && (
          <input
            type="number"
            step="any"
            className="widget-num-input widget-cmd-val-input"
            aria-label={t("cmd.testValue")}
            title={t("cmd.testValue")}
            value={commandValue}
            onChange={(ev) => {
              const v = Number.parseFloat(ev.target.value);
              if (Number.isFinite(v)) updateCfg({ commandValue: v });
            }}
            onKeyDown={(ev) => {
              if (ev.key === "Enter") {
                void handleFire();
              }
            }}
          />
        )}
        <button
          type="button"
          className="scope-btn scope-btn-accent widget-cmd-fire-btn"
          disabled={sending || Boolean(boundCommand && compiled && !compiled.ok)}
          onClick={() => void handleFire()}
        >
          ⚡ {t("widget.btn.send")}: {payloadPreview}
        </button>
        {statusMsg && <span className="widget-cmd-status">{statusMsg}</span>}
      </div>

      {configOpen && (
        <div className="widget-inline-config">
          <label style={{ gridColumn: "span 2" }}>
            {t("widget.cmdBindLabel")}
            <select
              data-testid={`btn-cmd-select-${widget.id}`}
              value={cfg.commandId ?? ""}
              onChange={(ev) => {
                const nextId = ev.target.value || null;
                const nextCmd = nextId ? commands.find((c) => c.id === nextId) ?? null : null;
                setStatusMsg(null);
                onUpdateWidget((prev) => ({
                  ...prev,
                  ...(nextCmd ? { title: nextCmd.name } : {}),
                  buttonConfig: {
                    ...getButtonCfg(prev, sourceIds),
                    commandId: nextId,
                    ...(nextCmd?.targetSourceId ? { sourceId: nextCmd.targetSourceId } : {}),
                    ...(nextCmd ? { priority: nextCmd.priority } : {}),
                  },
                }));
              }}
            >
              <option value="">{t("widget.cmdBindNoneCustom")}</option>
              {commands.map((cmd) => (
                <option key={cmd.id} value={cmd.id}>
                  {cmd.name} ({cmd.format.toUpperCase()})
                </option>
              ))}
            </select>
          </label>
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
            <>
              <label>
                {t("widget.btn.mode")}
                <select
                  value={cfg.mode}
                  onChange={(ev) =>
                    updateCfg({ mode: ev.target.value as WidgetButtonConfig["mode"] })
                  }
                >
                  <option value="ascii">ASCII</option>
                  <option value="hex">HEX</option>
                  <option value="kv">KV</option>
                </select>
              </label>
              {cfg.mode === "kv" ? (
                <>
                  <label>
                    {t("widget.slider.key")}
                    <input
                      type="text"
                      value={cfg.kvKey}
                      onChange={(ev) => updateCfg({ kvKey: ev.target.value })}
                    />
                  </label>
                  <label>
                    {t("widget.btn.cmd")}
                    <input
                      type="number"
                      step="any"
                      value={cfg.kvValue}
                      onChange={(ev) => {
                        const v = Number.parseFloat(ev.target.value);
                        if (Number.isFinite(v)) updateCfg({ kvValue: v });
                      }}
                    />
                  </label>
                </>
              ) : (
                <label style={{ gridColumn: "span 2" }}>
                  {t("widget.btn.cmd")}
                  <input
                    type="text"
                    value={cfg.command}
                    onChange={(ev) => updateCfg({ command: ev.target.value })}
                  />
                </label>
              )}
            </>
          )}
        </div>
      )}
    </div>
  );
}
