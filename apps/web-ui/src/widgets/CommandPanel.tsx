import { useRef, useState, type ChangeEvent, type DragEvent } from "react";
import type { DownlinkReceiptDto } from "../bridge/bindings.ts";
import { translate, type MessageKey, type UiLocale } from "../i18n/index.ts";
import {
  commandUsesValue,
  compileCommandPayload,
  type ByteEndian,
  type CommandDefinition,
  type CommandFormat,
} from "../workspace/command-system.ts";
import { readJsonFileFromInput, saveJsonFile } from "../workspace/file-dialog.ts";
import { isPointerDragArmed, startPointerDrag } from "../workspace/pointer-dnd.ts";
import type { DashboardCanvasConfig } from "./types.ts";

export interface CommandPanelProps {
  locale: UiLocale;
  commands: CommandDefinition[];
  canvases: Record<number, DashboardCanvasConfig>;
  sourceIds: string[];
  onAddCommand: (partial?: Partial<Omit<CommandDefinition, "id">>) => string;
  onUpdateCommand: (commandId: string, patch: Partial<Omit<CommandDefinition, "id">>) => void;
  onRemoveCommand: (commandId: string) => void;
  onPlaceCommandOnCanvas: (commandId: string) => void;
  onExecuteCommand: (
    commandId: string,
    opts?: { value?: number },
  ) => Promise<DownlinkReceiptDto>;
  onExportCommandsConfig: () => string;
  onImportCommandsConfig: (jsonText: string) => { importedCount: number };
}

export function CommandPanel({
  locale,
  commands,
  canvases,
  sourceIds,
  onAddCommand,
  onUpdateCommand,
  onRemoveCommand,
  onPlaceCommandOnCanvas,
  onExecuteCommand,
  onExportCommandsConfig,
  onImportCommandsConfig,
}: CommandPanelProps) {
  const t = (key: MessageKey, params?: Record<string, string | number>) =>
    translate(locale, key, params);

  const importInputRef = useRef<HTMLInputElement | null>(null);
  const [editingIds, setEditingIds] = useState<Record<string, boolean>>({});
  const [testValues, setTestValues] = useState<Record<string, number>>({});
  const [sendStatus, setSendStatus] = useState<Record<string, { ok: boolean; text: string }>>({});
  const [ioNotice, setIoNotice] = useState<{ kind: "ok" | "err"; text: string } | null>(null);

  const boundCounts = new Map<string, number>();
  for (const cv of Object.values(canvases)) {
    for (const w of cv.widgets) {
      const cmdId = w.sliderConfig?.commandId ?? w.buttonConfig?.commandId ?? null;
      if (cmdId) {
        boundCounts.set(cmdId, (boundCounts.get(cmdId) ?? 0) + 1);
      }
    }
  }

  const handleAdd = () => {
    const newId = onAddCommand();
    setEditingIds((prev) => ({ ...prev, [newId]: true }));
  };

  const handleExport = async () => {
    try {
      const json = onExportCommandsConfig();
      const saved = await saveJsonFile(json, "ddscope-commands.json");
      if (saved) {
        setIoNotice({
          kind: "ok",
          text: t("cmd.exportedNotice", { count: commands.length }),
        });
      }
    } catch (err: unknown) {
      setIoNotice({
        kind: "err",
        text: t("cmd.importErrorNotice", {
          error: err instanceof Error ? err.message : String(err),
        }),
      });
    }
  };

  const handleImportFileChange = async (ev: ChangeEvent<HTMLInputElement>) => {
    try {
      const text = await readJsonFileFromInput(ev.currentTarget);
      if (text === null) return;
      const result = onImportCommandsConfig(text);
      setIoNotice({
        kind: "ok",
        text: t("cmd.importedNotice", { count: result.importedCount }),
      });
    } catch (err: unknown) {
      setIoNotice({
        kind: "err",
        text: t("cmd.importErrorNotice", {
          error: err instanceof Error ? err.message : String(err),
        }),
      });
    }
  };

  const handleDragStart = (ev: DragEvent<HTMLElement>, cmd: CommandDefinition) => {
    if (isPointerDragArmed()) {
      ev.preventDefault();
      return;
    }
    ev.dataTransfer.setData(
      "application/x-ddscope-command",
      JSON.stringify({ commandId: cmd.id }),
    );
    ev.dataTransfer.setData("text/plain", `command:${cmd.id}`);
    ev.dataTransfer.effectAllowed = "copy";
  };

  const handleTestFire = async (cmd: CommandDefinition, val: number) => {
    try {
      const receipt = await onExecuteCommand(cmd.id, { value: val });
      setSendStatus((prev) => ({
        ...prev,
        [cmd.id]: {
          ok: true,
          text: `${t("widget.btn.sent")} · ${receipt.result_kind}`,
        },
      }));
    } catch (err: unknown) {
      setSendStatus((prev) => ({
        ...prev,
        [cmd.id]: {
          ok: false,
          text: err instanceof Error ? err.message : String(err),
        },
      }));
    }
  };

  return (
    <section className="command-panel-section" data-testid="command-panel">
      <div className="sidebar-config-bar">
        <button
          type="button"
          className="scope-btn scope-btn-accent"
          data-testid="add-command-btn"
          onClick={handleAdd}
        >
          {t("cmd.newCommand")}
        </button>
        <button
          type="button"
          className="scope-btn-xs"
          data-testid="export-commands-btn"
          disabled={commands.length === 0}
          onClick={() => {
            void handleExport();
          }}
        >
          {t("cmd.export")}
        </button>
        <button
          type="button"
          className="scope-btn-xs"
          data-testid="import-commands-btn"
          onClick={() => importInputRef.current?.click()}
        >
          {t("cmd.import")}
        </button>
        <input
          ref={importInputRef}
          type="file"
          accept=".json,application/json"
          style={{ display: "none" }}
          data-testid="import-commands-input"
          onChange={(ev) => {
            void handleImportFileChange(ev);
          }}
        />
      </div>

      {ioNotice && (
        <div
          className={`config-io-notice ${ioNotice.kind}`}
          data-testid="commands-config-notice"
        >
          <span>{ioNotice.text}</span>
          <button
            type="button"
            className="scope-btn-xs"
            onClick={() => setIoNotice(null)}
          >
            ×
          </button>
        </div>
      )}

      <div className="command-panel-hint">{t("cmd.dragHint")}</div>

      <div className="command-list-scroll">
        {commands.length === 0 ? (
          <div className="sidebar-empty-state" data-testid="commands-empty-state">
            <h4>{t("cmd.emptyTitle")}</h4>
            <p>{t("cmd.emptyDesc")}</p>
            <button
              type="button"
              className="scope-btn scope-btn-accent"
              onClick={handleAdd}
            >
              {t("cmd.newCommand")}
            </button>
          </div>
        ) : (
          commands.map((cmd) => {
            const isEditing = Boolean(editingIds[cmd.id]);
            const usesValue = commandUsesValue(cmd);
            const testVal = testValues[cmd.id] ?? 0;
            const compiled = compileCommandPayload(cmd, testVal);
            const boundCount = boundCounts.get(cmd.id) ?? 0;
            const badgeLabel =
              cmd.format === "hex"
                ? "HEX"
                : cmd.appendNewline
                  ? "STR+\\n"
                  : "STR";
            const status = sendStatus[cmd.id];

            return (
              <div
                key={cmd.id}
                className="command-card"
                draggable={!isEditing}
                data-testid={`command-item-${cmd.id}`}
                onPointerDown={(ev) => {
                  if (isEditing) return;
                  startPointerDrag(ev, {
                    kind: "command",
                    commandId: cmd.id,
                    badge: cmd.format === "hex" ? "HEX" : "STR",
                    label: cmd.name,
                  });
                }}
                onDragStart={(ev) => {
                  if (!isEditing) handleDragStart(ev, cmd);
                }}
              >
                <div
                  className="command-card-header"
                  draggable={isEditing}
                  onPointerDown={(ev) => {
                    if (!isEditing) return;
                    startPointerDrag(ev, {
                      kind: "command",
                      commandId: cmd.id,
                      badge: cmd.format === "hex" ? "HEX" : "STR",
                      label: cmd.name,
                    });
                  }}
                  onDragStart={(ev) => {
                    if (isEditing) handleDragStart(ev, cmd);
                  }}
                >
                  <span
                    className={`toolbox-badge ${
                      cmd.format === "hex" ? "cmd-badge-hex" : "cmd-badge-str"
                    }`}
                  >
                    {badgeLabel}
                  </span>
                  <span className="command-card-name" title={cmd.name}>
                    {cmd.name}
                  </span>
                  {boundCount > 0 && (
                    <span className="command-bound-badge">
                      {t("cmd.boundCount", { count: boundCount })}
                    </span>
                  )}
                  <div className="command-card-actions">
                    <button
                      type="button"
                      className="scope-btn-xs scope-btn-accent"
                      title={t("cmd.testSendTitle")}
                      data-testid={`test-send-cmd-${cmd.id}`}
                      disabled={!compiled.ok}
                      onClick={(ev) => {
                        ev.stopPropagation();
                        void handleTestFire(cmd, testVal);
                      }}
                    >
                      ⚡
                    </button>
                    <button
                      type="button"
                      className="toolbox-add-btn"
                      title={t("cmd.placeBtnTitle")}
                      data-testid={`place-cmd-${cmd.id}`}
                      onClick={(ev) => {
                        ev.stopPropagation();
                        onPlaceCommandOnCanvas(cmd.id);
                      }}
                    >
                      +
                    </button>
                    <button
                      type="button"
                      className="scope-btn-xs"
                      title={t("cmd.editBtnTitle")}
                      data-testid={`edit-cmd-${cmd.id}`}
                      onClick={(ev) => {
                        ev.stopPropagation();
                        setEditingIds((prev) => ({
                          ...prev,
                          [cmd.id]: !prev[cmd.id],
                        }));
                      }}
                    >
                      ✎
                    </button>
                    <button
                      type="button"
                      className="scope-btn-xs"
                      title={t("cmd.deleteBtnTitle")}
                      data-testid={`delete-cmd-${cmd.id}`}
                      onClick={(ev) => {
                        ev.stopPropagation();
                        onRemoveCommand(cmd.id);
                      }}
                    >
                      ✕
                    </button>
                  </div>
                </div>

                <div className="command-preview-row">
                  <div className="command-template-line">
                    <span className="command-meta-label">{t("cmd.templateShort")}:</span>
                    <code>{cmd.template || "—"}</code>
                  </div>
                  {usesValue && (
                    <label
                      className="command-test-val-wrap"
                      onPointerDown={(ev) => ev.stopPropagation()}
                    >
                      <span>{t("cmd.testValue")}:</span>
                      <input
                        type="number"
                        step="any"
                        className="widget-num-input"
                        data-testid={`test-val-cmd-${cmd.id}`}
                        value={testVal}
                        onChange={(ev) => {
                          const v = Number.parseFloat(ev.target.value);
                          if (Number.isFinite(v)) {
                            setTestValues((prev) => ({ ...prev, [cmd.id]: v }));
                          }
                        }}
                      />
                    </label>
                  )}
                  <div
                    className={`command-preview-output ${compiled.ok ? "" : "is-error"}`}
                  >
                    {compiled.ok ? compiled.preview : compiled.error}
                  </div>
                  {status && (
                    <div
                      className={`command-send-status ${status.ok ? "ok" : "err"}`}
                    >
                      {status.text}
                    </div>
                  )}
                </div>

                {isEditing && (
                  <div
                    className="command-editor-form"
                    onPointerDown={(ev) => ev.stopPropagation()}
                  >
                    <label>
                      {t("cmd.name")}
                      <input
                        type="text"
                        data-testid={`cmd-name-input-${cmd.id}`}
                        value={cmd.name}
                        onChange={(ev) =>
                          onUpdateCommand(cmd.id, { name: ev.target.value })
                        }
                      />
                    </label>
                    <label>
                      {t("cmd.format")}
                      <select
                        data-testid={`cmd-format-select-${cmd.id}`}
                        value={cmd.format}
                        onChange={(ev) =>
                          onUpdateCommand(cmd.id, {
                            format: ev.target.value as CommandFormat,
                          })
                        }
                      >
                        <option value="string">{t("cmd.formatString")}</option>
                        <option value="hex">{t("cmd.formatHex")}</option>
                      </select>
                    </label>
                    <label style={{ gridColumn: "span 2" }}>
                      {t("cmd.template")}
                      <input
                        type="text"
                        className="mono"
                        data-testid={`cmd-template-input-${cmd.id}`}
                        placeholder={
                          cmd.format === "hex" ? "00 01 02 / AA 55 %f" : "set_num %f / 123"
                        }
                        value={cmd.template}
                        onChange={(ev) =>
                          onUpdateCommand(cmd.id, { template: ev.target.value })
                        }
                      />
                    </label>
                    {cmd.format === "string" ? (
                      <label className="command-check-label">
                        <input
                          type="checkbox"
                          checked={cmd.appendNewline}
                          onChange={(ev) =>
                            onUpdateCommand(cmd.id, {
                              appendNewline: ev.target.checked,
                            })
                          }
                        />
                        <span>{t("cmd.appendNewline")}</span>
                      </label>
                    ) : (
                      <label>
                        {t("cmd.endian")}
                        <select
                          value={cmd.endian}
                          onChange={(ev) =>
                            onUpdateCommand(cmd.id, {
                              endian: ev.target.value as ByteEndian,
                            })
                          }
                        >
                          <option value="little">{t("cmd.endianLittle")}</option>
                          <option value="big">{t("cmd.endianBig")}</option>
                        </select>
                      </label>
                    )}
                    <label>
                      {t("cmd.targetSource")}
                      <select
                        value={cmd.targetSourceId}
                        onChange={(ev) =>
                          onUpdateCommand(cmd.id, { targetSourceId: ev.target.value })
                        }
                      >
                        <option value="">{t("cmd.sourceAuto")}</option>
                        {sourceIds.map((sid) => (
                          <option key={sid} value={sid}>
                            {sid}
                          </option>
                        ))}
                      </select>
                    </label>
                    <label>
                      {t("cmd.priority")}
                      <select
                        value={cmd.priority}
                        onChange={(ev) =>
                          onUpdateCommand(cmd.id, {
                            priority: ev.target.value === "high" ? "high" : "normal",
                          })
                        }
                      >
                        <option value="normal">{t("cmd.prioNormal")}</option>
                        <option value="high">{t("cmd.prioHigh")}</option>
                      </select>
                    </label>
                    <div className="command-format-help" style={{ gridColumn: "span 2" }}>
                      {t("cmd.formatHelp")}
                    </div>
                  </div>
                )}
              </div>
            );
          })
        )}
      </div>
    </section>
  );
}
