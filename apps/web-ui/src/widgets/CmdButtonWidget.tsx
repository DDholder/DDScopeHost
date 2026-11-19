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
import type { DashboardWidgetItem, WidgetButtonConfig } from "./types.ts";
import { WidgetConfigModal } from "./WidgetConfigModal.tsx";
import { WidgetHeaderActions } from "./WidgetHeaderSlot.tsx";

export interface CmdButtonWidgetProps {
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
  onBindCommand,
  onSetCommandArgValue,
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
  const placeholders = boundCommand ? extractCommandPlaceholders(boundCommand) : [];
  const effectiveSlot =
    cfg.commandArgIndex === -1
      ? -1
      : placeholders.length > 0
        ? Math.min(Math.max(0, Math.trunc(cfg.commandArgIndex ?? 0)), placeholders.length - 1)
        : 0;
  const commandValue = cfg.commandValue ?? 0;
  const effectiveTarget =
    boundCommand?.targetSourceId.trim() || cfg.sourceId || sourceIds[0] || "sim0";
  const compiled = boundCommand
    ? compileCommandPayload(
        boundCommand,
        placeholders.length === 0
          ? 0
          : effectiveSlot === -1
            ? buildEffectiveCommandArgs(boundCommand)
            : buildEffectiveCommandArgs(boundCommand, {
                slotIndex: effectiveSlot,
                value: commandValue,
              }),
      )
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

  const handleSlotChange = (nextSlot: number) => {
    if (!boundCommand || !Number.isFinite(nextSlot)) return;
    if (onBindCommand) {
      onBindCommand(boundCommand.id, nextSlot);
    } else {
      updateCfg({ commandArgIndex: nextSlot });
      if (nextSlot >= 0) {
        onSetCommandArgValue?.(boundCommand.id, nextSlot, commandValue, widget.id);
      }
    }
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
        if (placeholders.length > 0 && effectiveSlot >= 0) {
          onSetCommandArgValue?.(boundCommand.id, effectiveSlot, commandValue, widget.id);
        }
        const res = compileCommandPayload(
          boundCommand,
          placeholders.length === 0
            ? 0
            : effectiveSlot === -1
              ? buildEffectiveCommandArgs(boundCommand)
              : buildEffectiveCommandArgs(boundCommand, {
                  slotIndex: effectiveSlot,
                  value: commandValue,
                }),
        );
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

  const handleSelectCommand = (nextId: string | null) => {
    setStatusMsg(null);
    if (onBindCommand) {
      onBindCommand(nextId);
      return;
    }
    const nextCmd = nextId ? commands.find((c) => c.id === nextId) ?? null : null;
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
  };

  return (
    <div className="widget-cmd-body" data-testid={`widget-cmd-${widget.id}`}>
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
            <option value="-1">ALL</option>
          </select>
        )}
        <button
          type="button"
          className={`widget-header-btn ${configOpen ? "active" : ""}`}
          title={t("widget.configToggle")}
          data-testid={`btn-config-btn-${widget.id}`}
          onClick={() => setConfigOpen((v) => !v)}
        >
          ⚙
        </button>
      </WidgetHeaderActions>

      <div className="widget-cmd-action-row">
        {boundCommand && placeholders.length > 0 && effectiveSlot >= 0 && (
          <input
            type="number"
            step="any"
            className="widget-num-input widget-cmd-val-input"
            aria-label={t("cmd.testValue")}
            title={t("cmd.testValue")}
            value={commandValue}
            onChange={(ev) => {
              const v = Number.parseFloat(ev.target.value);
              if (Number.isFinite(v)) {
                updateCfg({ commandValue: v });
                onSetCommandArgValue?.(boundCommand.id, effectiveSlot, v, widget.id);
              }
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
                data-testid={`btn-cmd-select-${widget.id}`}
                value={cfg.commandId ?? ""}
                onChange={(ev) => handleSelectCommand(ev.target.value || null)}
              >
                <option value="">{t("widget.cmdBindNoneCustom")}</option>
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
                  data-testid={`btn-arg-select-${widget.id}`}
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
                  <option value="-1">{t("widget.cmdPlaceholderTriggerAll")}</option>
                </select>
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
      </WidgetConfigModal>
    </div>
  );
}
