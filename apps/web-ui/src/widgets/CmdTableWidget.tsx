import { useState } from "react";
import type {
  DownlinkReceiptDto,
  SendDownlinkRequestDto_Deserialize,
} from "../bridge/bindings.ts";
import type { UiLocale } from "../i18n/index.ts";
import { translate } from "../i18n/index.ts";
import {
  buildEffectiveCommandArgs,
  compileCmdTableRow,
  createCmdTableRow,
  extractCommandPlaceholders,
  updateCmdTableRowValue,
  type CommandDefinition,
} from "../workspace/command-system.ts";
import type {
  DashboardWidgetItem,
  WidgetCmdTableConfig,
  WidgetCmdTableRow,
} from "./types.ts";

export interface CmdTableWidgetProps {
  widget: DashboardWidgetItem;
  sourceIds: string[];
  commands: CommandDefinition[];
  locale: UiLocale;
  onUpdateWidget: (updater: (prev: DashboardWidgetItem) => DashboardWidgetItem) => void;
  onBindCommand?: (commandId: string | null) => void;
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

function getCmdTableCfg(
  widget: DashboardWidgetItem,
  sourceIds: string[],
): WidgetCmdTableConfig {
  return (
    widget.cmdTableConfig ?? {
      sourceId: sourceIds[0] ?? "sim0",
      commandId: null,
      rows: [],
    }
  );
}

export function CmdTableWidget({
  widget,
  sourceIds,
  commands,
  locale,
  onUpdateWidget,
  onBindCommand,
  onSetCommandArgValue,
  onSendDownlink,
}: CmdTableWidgetProps) {
  const t = (key: Parameters<typeof translate>[1], params?: Record<string, string | number>) =>
    translate(locale, key, params);

  const [configOpen, setConfigOpen] = useState(false);
  const [sendingRowId, setSendingRowId] = useState<string | null>(null);
  const [status, setStatus] = useState<{ ok: boolean; text: string } | null>(null);

  const cfg = getCmdTableCfg(widget, sourceIds);
  const boundCommand = cfg.commandId
    ? commands.find((c) => c.id === cfg.commandId) ?? null
    : null;
  const placeholders = boundCommand ? extractCommandPlaceholders(boundCommand) : [];
  const effectiveTarget =
    boundCommand?.targetSourceId.trim() || cfg.sourceId || sourceIds[0] || "sim0";

  const updateCfg = (updater: (prev: WidgetCmdTableConfig) => WidgetCmdTableConfig) => {
    onUpdateWidget((prev) => ({
      ...prev,
      cmdTableConfig: updater(getCmdTableCfg(prev, sourceIds)),
    }));
  };

  const handleSelectCommand = (nextId: string | null) => {
    setStatus(null);
    if (onBindCommand) {
      onBindCommand(nextId);
      return;
    }
    const nextCmd = nextId ? commands.find((c) => c.id === nextId) ?? null : null;
    const nextPlaceholders = nextCmd ? extractCommandPlaceholders(nextCmd) : [];
    onUpdateWidget((prev) => {
      const prevCfg = getCmdTableCfg(prev, sourceIds);
      const nextRows =
        nextCmd && prevCfg.rows.length === 0
          ? [
              createCmdTableRow(
                [],
                nextPlaceholders.length,
                buildEffectiveCommandArgs(nextCmd),
                locale,
              ),
            ]
          : prevCfg.rows;
      return {
        ...prev,
        ...(nextCmd ? { title: nextCmd.name } : {}),
        cmdTableConfig: {
          sourceId: nextCmd?.targetSourceId || prevCfg.sourceId || sourceIds[0] || "sim0",
          commandId: nextId,
          rows: nextRows,
        },
      };
    });
  };

  const handleAddRow = () => {
    if (!boundCommand) return;
    const seed = buildEffectiveCommandArgs(boundCommand);
    updateCfg((prev) => ({
      ...prev,
      rows: [
        ...prev.rows,
        createCmdTableRow(prev.rows, placeholders.length, seed, locale),
      ],
    }));
  };

  const handleUpdateRow = (
    rowId: string,
    updater: (row: WidgetCmdTableRow) => WidgetCmdTableRow,
  ) => {
    updateCfg((prev) => ({
      ...prev,
      rows: prev.rows.map((r) => (r.id === rowId ? updater(r) : r)),
    }));
  };

  const handleDeleteRow = (rowId: string) => {
    updateCfg((prev) => ({
      ...prev,
      rows: prev.rows.filter((r) => r.id !== rowId),
    }));
  };

  const handleFireRow = async (row: WidgetCmdTableRow, rowIdx: number) => {
    if (!boundCommand || sendingRowId !== null) return;
    const { args, compiled } = compileCmdTableRow(boundCommand, row);
    if (!compiled.ok) {
      setStatus({ ok: false, text: compiled.error });
      return;
    }

    setSendingRowId(row.id);
    setStatus(null);
    try {
      for (let i = 0; i < args.length; i += 1) {
        onSetCommandArgValue?.(boundCommand.id, i, args[i], widget.id);
      }
      const rowLabel = row.label.trim() || `#${rowIdx + 1}`;
      const receipt = await onSendDownlink(
        {
          source_id: effectiveTarget,
          priority: boundCommand.priority,
          coalesce_key: null,
          payload: compiled.payload,
        },
        `${boundCommand.name} [${rowLabel}]: ${compiled.preview}`,
        false,
      );
      setStatus({
        ok: true,
        text: `${rowLabel}: ${t("widget.btn.sent")} · ${receipt.result_kind}`,
      });
    } catch (err: unknown) {
      setStatus({
        ok: false,
        text: err instanceof Error ? err.message : String(err),
      });
    } finally {
      setSendingRowId(null);
    }
  };

  if (!boundCommand) {
    return (
      <div
        className="widget-unbound-box"
        data-testid={`widget-cmd-table-${widget.id}`}
      >
        <span>{t("widget.cmdTable.unbound")}</span>
        <select
          className="widget-bind-select"
          data-testid={`cmd-table-bind-select-${widget.id}`}
          value=""
          onChange={(ev) => {
            const nextId = ev.target.value || null;
            if (nextId) handleSelectCommand(nextId);
          }}
        >
          <option value="">{t("widget.cmdTable.selectCmd")}</option>
          {commands.map((cmd) => (
            <option key={cmd.id} value={cmd.id}>
              {cmd.name} ({cmd.format.toUpperCase()})
            </option>
          ))}
        </select>
      </div>
    );
  }

  return (
    <div
      className="widget-cmd-table-body"
      data-testid={`widget-cmd-table-${widget.id}`}
    >
      <div className="widget-cmd-top">
        <span
          className="widget-ch-badge widget-cmd-bound-badge"
          title={boundCommand.template}
        >
          ⚡ {boundCommand.name} · {effectiveTarget} ·{" "}
          {boundCommand.format === "hex" ? "HEX" : "STR"}
        </span>
        {status && (
          <span
            className={`widget-cmd-table-status ${status.ok ? "ok" : "err"}`}
            title={status.text}
          >
            {status.text}
          </span>
        )}
        <button
          type="button"
          className="scope-btn-xs scope-btn-accent"
          data-testid={`cmd-table-add-row-${widget.id}`}
          onClick={handleAddRow}
        >
          {t("widget.cmdTable.addRow")}
        </button>
        <button
          type="button"
          className="scope-btn-xs"
          onClick={() => setConfigOpen((v) => !v)}
        >
          {t("widget.configToggle")}
        </button>
      </div>

      {configOpen && (
        <div className="widget-inline-config">
          <label style={{ gridColumn: "span 2" }}>
            {t("widget.cmdBindLabel")}
            <select
              data-testid={`cmd-table-cmd-select-${widget.id}`}
              value={cfg.commandId ?? ""}
              onChange={(ev) => handleSelectCommand(ev.target.value || null)}
            >
              <option value="">{t("widget.cmdTable.selectCmd")}</option>
              {commands.map((cmd) => (
                <option key={cmd.id} value={cmd.id}>
                  {cmd.name} ({cmd.format.toUpperCase()})
                </option>
              ))}
            </select>
          </label>
          {!boundCommand.targetSourceId && (
            <label>
              {t("widget.slider.source")}
              <select
                value={cfg.sourceId}
                onChange={(ev) =>
                  updateCfg((prev) => ({ ...prev, sourceId: ev.target.value }))
                }
              >
                {(sourceIds.length > 0 ? sourceIds : [cfg.sourceId]).map((id) => (
                  <option key={id} value={id}>
                    {id}
                  </option>
                ))}
              </select>
            </label>
          )}
        </div>
      )}

      <div className="widget-cmd-table-wrap">
        <table className="widget-watch-table widget-cmd-table">
          <thead>
            <tr>
              <th>{t("widget.cmdTable.colLabel")}</th>
              {placeholders.length > 0 ? (
                placeholders.map((ph) => (
                  <th key={ph.index}>
                    <span>{ph.label}</span>
                    <span className="cmd-table-th-token">{ph.token}</span>
                  </th>
                ))
              ) : (
                <th>{t("widget.btn.cmd")}</th>
              )}
              <th className="cmd-table-th-action">{t("widget.cmdTable.colAction")}</th>
            </tr>
          </thead>
          <tbody>
            {cfg.rows.length === 0 ? (
              <tr>
                <td
                  colSpan={Math.max(1, placeholders.length) + 2}
                  className="cmd-table-empty-cell"
                >
                  {t("widget.cmdTable.emptyRows")}
                </td>
              </tr>
            ) : (
              cfg.rows.map((row, rowIdx) => {
                const { args, compiled } = compileCmdTableRow(boundCommand, row);
                return (
                  <tr
                    key={row.id}
                    data-testid={`cmd-table-row-${widget.id}-${row.id}`}
                  >
                    <td>
                      <input
                        type="text"
                        className="cmd-table-label-input"
                        data-testid={`cmd-table-label-${widget.id}-${row.id}`}
                        value={row.label}
                        placeholder={t("widget.cmdTable.rowPlaceholder", {
                          num: rowIdx + 1,
                        })}
                        onChange={(ev) =>
                          handleUpdateRow(row.id, (prev) => ({
                            ...prev,
                            label: ev.target.value,
                          }))
                        }
                        onKeyDown={(ev) => {
                          if (ev.key === "Enter") {
                            void handleFireRow(row, rowIdx);
                          }
                        }}
                      />
                    </td>
                    {placeholders.length > 0 ? (
                      placeholders.map((ph) => (
                        <td key={ph.index}>
                          <input
                            type="number"
                            step="any"
                            className="watch-num-input cmd-table-val-input"
                            data-testid={`cmd-table-val-${widget.id}-${row.id}-${ph.index}`}
                            value={args[ph.index] ?? 0}
                            onChange={(ev) => {
                              const nextVal = Number.parseFloat(ev.target.value);
                              if (!Number.isFinite(nextVal)) return;
                              handleUpdateRow(row.id, (prev) => ({
                                ...prev,
                                values: updateCmdTableRowValue(
                                  prev.values,
                                  ph.index,
                                  nextVal,
                                ),
                              }));
                            }}
                            onKeyDown={(ev) => {
                              if (ev.key === "Enter") {
                                void handleFireRow(row, rowIdx);
                              }
                            }}
                          />
                        </td>
                      ))
                    ) : (
                      <td className="cmd-table-static-cell">
                        {compiled.ok ? compiled.preview : compiled.error}
                      </td>
                    )}
                    <td className="cmd-table-action-cell">
                      <button
                        type="button"
                        className="scope-btn-xs scope-btn-accent"
                        data-testid={`cmd-table-send-${widget.id}-${row.id}`}
                        title={compiled.ok ? compiled.preview : compiled.error}
                        disabled={sendingRowId !== null || !compiled.ok}
                        onClick={() => void handleFireRow(row, rowIdx)}
                      >
                        ⚡ {t("widget.cmdTable.sendRow")}
                      </button>
                      <button
                        type="button"
                        className="scope-ch-del"
                        data-testid={`cmd-table-del-row-${widget.id}-${row.id}`}
                        title={t("widget.cmdTable.deleteRow")}
                        onClick={() => handleDeleteRow(row.id)}
                      >
                        ×
                      </button>
                    </td>
                  </tr>
                );
              })
            )}
          </tbody>
        </table>
      </div>
    </div>
  );
}
