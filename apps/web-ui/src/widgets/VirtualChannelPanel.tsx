import { useMemo, useRef, useState, type ChangeEvent, type DragEvent } from "react";
import type {
  ChannelModalityDto,
  ChannelNodeDto_Serialize,
  PowerOutputFlagsDto,
  VirtualNodeConfigDto_Serialize,
  VirtualOperatorDto_Serialize,
  WindowMetricKindDto,
} from "../bridge/bindings.ts";
import {
  VIRTUAL_SOURCE_ID,
  expandVirtualOutputSpecs,
  normalizePowerFlags,
  normalizeVirtualNodeConfig,
  validateVirtualNodeConfig,
  type PowerMetricKey,
} from "../bridge/virtual-eval.ts";
import { translate, type MessageKey, type UiLocale } from "../i18n/index.ts";
import { readJsonFileFromInput, saveJsonFile } from "../workspace/file-dialog.ts";
import { isPointerDragArmed, startPointerDrag } from "../workspace/pointer-dnd.ts";

export interface VirtualChannelPanelProps {
  locale: UiLocale;
  virtualChannels: VirtualNodeConfigDto_Serialize[];
  nextVirtualSeq: number;
  channelsById: Record<number, ChannelNodeDto_Serialize>;
  targetScopeId: number | null;
  onUpsertVirtualChannel: (cfg: VirtualNodeConfigDto_Serialize) => Promise<unknown>;
  onRemoveVirtualChannel: (nodeId: string) => Promise<void>;
  onClearVirtualChannels: () => Promise<void>;
  onExportVirtualChannelsConfig: () => string;
  onImportVirtualChannelsConfig: (jsonText: string) => Promise<{ importedCount: number }>;
  onBindChannelToScope: (scopeId: number, channelId: number) => void;
  onPlaceWatchTable: (nodeId: string) => string;
  onEnsureScope?: () => number;
}

type OperatorKind = VirtualOperatorDto_Serialize["kind"];

const POWER_METRIC_KEYS: ReadonlyArray<PowerMetricKey> = [
  "freq",
  "u_rms",
  "i_rms",
  "p",
  "q1",
  "q_tot",
  "s",
  "pf",
  "phi_deg",
  "u_pk",
  "i_pk",
  "energy_wh",
];

const CORE_POWER_METRICS: ReadonlySet<PowerMetricKey> = new Set([
  "freq",
  "u_rms",
  "i_rms",
  "p",
  "q1",
  "pf",
]);

const WINDOW_METRICS: ReadonlyArray<WindowMetricKindDto> = [
  "rms",
  "mean",
  "peak_to_peak",
  "min",
  "max",
  "std_dev",
];

function modalityBadge(modality: ChannelModalityDto): { labelKey: MessageKey; cls: string } {
  switch (modality) {
    case "digital_bool":
      return { labelKey: "sidebar.mod.dig", cls: "mod-dig" };
    case "state_machine":
      return { labelKey: "sidebar.mod.fsm", cls: "mod-fsm" };
    case "frame":
      return { labelKey: "sidebar.mod.fft", cls: "mod-fft" };
    case "scalar":
    default:
      return { labelKey: "sidebar.mod.ana", cls: "mod-ana" };
  }
}

function uniqueLocalName(
  base: string,
  usedOutputs: ReadonlySet<string>,
  virtualChannels: ReadonlyArray<VirtualNodeConfigDto_Serialize>,
): string {
  const taken = new Set<string>(usedOutputs);
  for (const node of virtualChannels) {
    for (const spec of expandVirtualOutputSpecs(node)) {
      taken.add(spec.localPath);
    }
    if (node.operator.kind === "single_phase_power") {
      taken.add(node.operator.output_prefix);
    }
  }
  if (!taken.has(base)) return base;
  let idx = 2;
  while (taken.has(`${base}_${idx}`)) idx += 1;
  return `${base}_${idx}`;
}

function pickDefaultChannels(allChannels: ReadonlyArray<ChannelNodeDto_Serialize>) {
  const physical = allChannels.filter((c) => c.source_id !== VIRTUAL_SOURCE_ID);
  const pool = physical.length > 0 ? physical : allChannels;
  const scalars = pool.filter((c) => c.modality === "scalar");
  const digitals = pool.filter((c) => c.modality === "digital_bool");
  const fsms = pool.filter((c) => c.modality === "state_machine");

  const firstScalar = scalars[0]?.full_path ?? pool[0]?.full_path ?? "sim0/ch0";
  const firstDigital = digitals[0]?.full_path ?? firstScalar;
  const firstFsm = fsms[0]?.full_path ?? pool[0]?.full_path ?? "sim0/fsm";

  const voltageCh =
    scalars.find((c) => /(^|_)(v_bus|u_grid|u_ac|va|v_?in|u\d*|v\d*)$/i.test(c.local_path))
      ?.full_path ?? firstScalar;
  const currentCh =
    scalars.find(
      (c) =>
        c.full_path !== voltageCh &&
        /(^|_)(i_q|i_d|ia|i_grid|i_load|curr|i\d*)$/i.test(c.local_path),
    )?.full_path ??
    scalars.find((c) => c.full_path !== voltageCh)?.full_path ??
    firstScalar;

  return {
    firstScalar,
    firstDigital,
    firstFsm,
    voltageCh,
    currentCh,
    secondBitCh: digitals[1]?.full_path ?? firstDigital,
  };
}

function createPresetNode(
  kind: OperatorKind,
  nextSeqSeed: number,
  virtualChannels: ReadonlyArray<VirtualNodeConfigDto_Serialize>,
  allChannels: ReadonlyArray<ChannelNodeDto_Serialize>,
  locale: UiLocale,
): VirtualNodeConfigDto_Serialize {
  const usedIds = new Set(virtualChannels.map((n) => n.node_id));
  let seq = nextSeqSeed;
  while (usedIds.has(`v_${seq}`)) seq += 1;
  const nodeId = `v_${seq}`;

  const usedVirtualLocals = new Set(
    allChannels.filter((c) => c.source_id === VIRTUAL_SOURCE_ID).map((c) => c.local_path),
  );
  const ch = pickDefaultChannels(allChannels);
  const zh = locale === "zh-CN";

  switch (kind) {
    case "analog_to_digital": {
      const output_path = uniqueLocalName(`cmp_${seq}`, usedVirtualLocals, virtualChannels);
      return {
        node_id: nodeId,
        name: zh ? `模数比较 ${seq}` : `A->D ${seq}`,
        enabled: true,
        operator: {
          kind: "analog_to_digital",
          input_channel: ch.firstScalar,
          output_path,
          mode: {
            mode: "schmitt",
            threshold: 0,
            hysteresis: 0.25,
            invert: false,
          },
        },
      };
    }
    case "to_state_machine": {
      const output_path = uniqueLocalName(`fsm_${seq}`, usedVirtualLocals, virtualChannels);
      return {
        node_id: nodeId,
        name: zh ? `区间状态机 ${seq}` : `State Map ${seq}`,
        enabled: true,
        operator: {
          kind: "to_state_machine",
          output_path,
          mode: {
            mode: "range_map",
            input_channel: ch.firstScalar,
            hysteresis: 0.2,
            rules: [
              { min: null, max: -5, code: 1, name: "LOW", color_rgba: 0x38bdf8ff },
              { min: -5, max: 5, code: 2, name: "NORM", color_rgba: 0x34d399ff },
              { min: 5, max: null, code: 3, name: "HIGH", color_rgba: 0xfb7185ff },
            ],
            default_state: { code: 0, name: "IDLE", color_rgba: 0x94a3b8ff },
          },
        },
      };
    }
    case "to_analog": {
      const output_path = uniqueLocalName(`dac_${seq}`, usedVirtualLocals, virtualChannels);
      return {
        node_id: nodeId,
        name: zh ? `数模转换 ${seq}` : `D->A ${seq}`,
        enabled: true,
        operator: {
          kind: "to_analog",
          output_path,
          unit: "V",
          mode: {
            mode: "level_map",
            input_channel: ch.firstDigital,
            low_value: 0,
            high_value: 5,
            threshold: 0.5,
            smoothing_tau_ms: 0,
          },
        },
      };
    }
    case "formula": {
      const output_path = uniqueLocalName(`expr_${seq}`, usedVirtualLocals, virtualChannels);
      return {
        node_id: nodeId,
        name: zh ? `公式计算 ${seq}` : `Formula ${seq}`,
        enabled: true,
        operator: {
          kind: "formula",
          expression: `[${ch.firstScalar}] * 1.0`,
          output_path,
          unit: null,
          output_digital: false,
        },
      };
    }
    case "window_stats": {
      const output_path = uniqueLocalName(`rms_${seq}`, usedVirtualLocals, virtualChannels);
      return {
        node_id: nodeId,
        name: zh ? `滑窗统计 ${seq}` : `Window Stats ${seq}`,
        enabled: true,
        operator: {
          kind: "window_stats",
          input_channel: ch.firstScalar,
          output_path,
          unit: null,
          metric: "rms",
          window_ms: 20,
        },
      };
    }
    case "calculus": {
      const output_path = uniqueLocalName(`diff_${seq}`, usedVirtualLocals, virtualChannels);
      return {
        node_id: nodeId,
        name: zh ? `微分/积分 ${seq}` : `Calculus ${seq}`,
        enabled: true,
        operator: {
          kind: "calculus",
          input_channel: ch.firstScalar,
          output_path,
          unit: null,
          mode: {
            mode: "derivative",
            scale: 1,
            filter_tau_ms: 0,
          },
        },
      };
    }
    case "single_phase_power": {
      const output_prefix = uniqueLocalName(
        `pwr_${seq}`,
        usedVirtualLocals,
        virtualChannels,
      );
      return {
        node_id: nodeId,
        name: zh ? `单相电力分析 ${seq}` : `1Φ Power ${seq}`,
        enabled: true,
        operator: {
          kind: "single_phase_power",
          voltage_channel: ch.voltageCh,
          current_channel: ch.currentCh,
          output_prefix,
          voltage_scale: 1,
          voltage_offset: 0,
          current_scale: 1,
          current_offset: 0,
          nominal_freq_hz: 50,
          zero_cross_hysteresis_v: 0.2,
          min_freq_hz: 5,
          fallback_window_ms: 100,
          outputs: normalizePowerFlags(),
        },
      };
    }
  }
}

function parseFiniteInput(raw: string, fallback: number): number {
  const parsed = Number(raw);
  return Number.isFinite(parsed) ? parsed : fallback;
}

function parseOptionalInput(raw: string): number | null {
  const trimmed = raw.trim();
  if (!trimmed) return null;
  const parsed = Number(trimmed);
  return Number.isFinite(parsed) ? parsed : null;
}

interface ChannelSelectProps {
  value: string;
  channels: ReadonlyArray<ChannelNodeDto_Serialize>;
  onChange: (nextPath: string) => void;
  testId?: string;
}

function ChannelPathSelect({ value, channels, onChange, testId }: ChannelSelectProps) {
  const hasMatch = channels.some((c) => c.full_path === value);
  return (
    <select
      className="virtual-input"
      value={value}
      data-testid={testId}
      onChange={(e) => onChange(e.target.value)}
    >
      {!hasMatch && value && <option value={value}>{value}</option>}
      {channels.map((ch) => (
        <option key={ch.channel_id} value={ch.full_path}>
          {ch.full_path}
        </option>
      ))}
    </select>
  );
}

export function VirtualChannelPanel({
  locale,
  virtualChannels,
  nextVirtualSeq,
  channelsById,
  targetScopeId,
  onUpsertVirtualChannel,
  onRemoveVirtualChannel,
  onClearVirtualChannels,
  onExportVirtualChannelsConfig,
  onImportVirtualChannelsConfig,
  onBindChannelToScope,
  onPlaceWatchTable,
  onEnsureScope,
}: VirtualChannelPanelProps) {
  const t = (key: MessageKey, params?: Record<string, string | number>) =>
    translate(locale, key, params);

  const importInputRef = useRef<HTMLInputElement | null>(null);
  const [expandedIds, setExpandedIds] = useState<Record<string, boolean>>({});
  const [drafts, setDrafts] = useState<Record<string, VirtualNodeConfigDto_Serialize>>({});
  const [nodeErrors, setNodeErrors] = useState<Record<string, string>>({});
  const [notice, setNotice] = useState<{ kind: "ok" | "err"; text: string } | null>(null);

  const allChannels = useMemo(
    () =>
      Object.values(channelsById).sort((a, b) => a.full_path.localeCompare(b.full_path)),
    [channelsById],
  );

  const channelByFullPath = useMemo(() => {
    const map = new Map<string, ChannelNodeDto_Serialize>();
    for (const ch of allChannels) {
      map.set(ch.full_path, ch);
    }
    return map;
  }, [allChannels]);

  const setDraftFor = (
    node: VirtualNodeConfigDto_Serialize,
    updater: (prev: VirtualNodeConfigDto_Serialize) => VirtualNodeConfigDto_Serialize,
  ) => {
    setDrafts((prev) => {
      const base = prev[node.node_id] ?? node;
      return { ...prev, [node.node_id]: updater(base) };
    });
  };

  const applyNode = async (candidate: VirtualNodeConfigDto_Serialize) => {
    try {
      const normalized = normalizeVirtualNodeConfig(candidate);
      validateVirtualNodeConfig(normalized);
      await onUpsertVirtualChannel(normalized);
      setDrafts((prev) => {
        const next = { ...prev };
        delete next[candidate.node_id];
        return next;
      });
      setNodeErrors((prev) => {
        const next = { ...prev };
        delete next[candidate.node_id];
        return next;
      });
      const outCount = normalized.enabled ? expandVirtualOutputSpecs(normalized).length : 0;
      setNotice({
        kind: "ok",
        text: t("virtual.saved", { name: normalized.name, outputs: outCount }),
      });
    } catch (err: unknown) {
      const msg = err instanceof Error ? err.message : String(err);
      setNodeErrors((prev) => ({ ...prev, [candidate.node_id]: msg }));
      setNotice({ kind: "err", text: t("virtual.saveError", { error: msg }) });
    }
  };

  const handleAddPreset = async (kind: OperatorKind) => {
    const node = createPresetNode(
      kind,
      nextVirtualSeq,
      virtualChannels,
      allChannels,
      locale,
    );
    try {
      await onUpsertVirtualChannel(node);
      setExpandedIds((prev) => ({ ...prev, [node.node_id]: true }));
      setNotice({
        kind: "ok",
        text: t("virtual.saved", {
          name: node.name,
          outputs: expandVirtualOutputSpecs(node).length,
        }),
      });
    } catch (err: unknown) {
      setNotice({
        kind: "err",
        text: t("virtual.saveError", {
          error: err instanceof Error ? err.message : String(err),
        }),
      });
    }
  };

  const handleExport = async () => {
    try {
      const json = onExportVirtualChannelsConfig();
      const saved = await saveJsonFile(json, "ddscope-virtual-channels.json");
      if (saved) {
        setNotice({
          kind: "ok",
          text: t("virtual.exported", { count: virtualChannels.length }),
        });
      }
    } catch (err: unknown) {
      setNotice({
        kind: "err",
        text: t("virtual.importError", {
          error: err instanceof Error ? err.message : String(err),
        }),
      });
    }
  };

  const handleImportChange = async (ev: ChangeEvent<HTMLInputElement>) => {
    try {
      const text = await readJsonFileFromInput(ev.currentTarget);
      if (text === null) return;
      const res = await onImportVirtualChannelsConfig(text);
      setDrafts({});
      setNodeErrors({});
      setNotice({
        kind: "ok",
        text: t("virtual.imported", { count: res.importedCount }),
      });
    } catch (err: unknown) {
      setNotice({
        kind: "err",
        text: t("virtual.importError", {
          error: err instanceof Error ? err.message : String(err),
        }),
      });
    }
  };

  const handleBindOutput = (channelId: number) => {
    const scopeId = targetScopeId ?? onEnsureScope?.() ?? null;
    if (scopeId !== null) {
      onBindChannelToScope(scopeId, channelId);
    }
  };

  const handleDragStartOutput = (
    ev: DragEvent<HTMLDivElement>,
    ch: ChannelNodeDto_Serialize,
  ) => {
    if (isPointerDragArmed()) {
      ev.preventDefault();
      return;
    }
    ev.dataTransfer.setData(
      "application/x-ddscope-channel",
      JSON.stringify({
        channelId: ch.channel_id,
        modality: ch.modality,
        fullPath: ch.full_path,
      }),
    );
    ev.dataTransfer.setData("text/plain", `channel:${ch.channel_id}`);
    ev.dataTransfer.effectAllowed = "copy";
  };

  const renderOperatorFields = (
    node: VirtualNodeConfigDto_Serialize,
    draft: VirtualNodeConfigDto_Serialize,
  ) => {
    const op = draft.operator;
    const defaults = pickDefaultChannels(allChannels);
    const updateOp = (nextOp: VirtualOperatorDto_Serialize) => {
      setDraftFor(node, (prev) => ({ ...prev, operator: nextOp }));
    };

    switch (op.kind) {
      case "analog_to_digital":
        return (
          <div className="virtual-form-grid">
            <label className="virtual-field">
              <span>{t("virtual.inputChannel")}</span>
              <ChannelPathSelect
                value={op.input_channel}
                channels={allChannels}
                onChange={(v) => updateOp({ ...op, input_channel: v })}
              />
            </label>
            <label className="virtual-field">
              <span>{t("virtual.outputPath")}</span>
              <input
                className="virtual-input"
                type="text"
                value={op.output_path}
                onChange={(e) => updateOp({ ...op, output_path: e.target.value })}
              />
            </label>
            <label className="virtual-field">
              <span>{t("virtual.mode")}</span>
              <select
                className="virtual-input"
                value={op.mode.mode}
                onChange={(e) =>
                  updateOp({
                    ...op,
                    mode:
                      e.target.value === "window"
                        ? { mode: "window", low: -1, high: 1, invert: op.mode.invert }
                        : {
                            mode: "schmitt",
                            threshold: 0,
                            hysteresis: 0.25,
                            invert: op.mode.invert,
                          },
                  })
                }
              >
                <option value="schmitt">{t("virtual.mode.schmitt")}</option>
                <option value="window">{t("virtual.mode.window")}</option>
              </select>
            </label>
            {op.mode.mode === "schmitt" ? (
              <div className="virtual-two-col">
                <label className="virtual-field">
                  <span>{t("virtual.threshold")}</span>
                  <input
                    className="virtual-input"
                    type="number"
                    step="0.1"
                    value={op.mode.threshold ?? 0}
                    onChange={(e) =>
                      op.mode.mode === "schmitt" &&
                      updateOp({
                        ...op,
                        mode: {
                          ...op.mode,
                          threshold: parseFiniteInput(e.target.value, 0),
                        },
                      })
                    }
                  />
                </label>
                <label className="virtual-field">
                  <span>{t("virtual.hysteresis")}</span>
                  <input
                    className="virtual-input"
                    type="number"
                    step="0.05"
                    min="0"
                    value={op.mode.hysteresis ?? 0.25}
                    onChange={(e) =>
                      op.mode.mode === "schmitt" &&
                      updateOp({
                        ...op,
                        mode: {
                          ...op.mode,
                          hysteresis: Math.max(0, parseFiniteInput(e.target.value, 0)),
                        },
                      })
                    }
                  />
                </label>
              </div>
            ) : (
              <div className="virtual-two-col">
                <label className="virtual-field">
                  <span>{t("virtual.low")}</span>
                  <input
                    className="virtual-input"
                    type="number"
                    step="0.1"
                    value={op.mode.low ?? -1}
                    onChange={(e) =>
                      op.mode.mode === "window" &&
                      updateOp({
                        ...op,
                        mode: { ...op.mode, low: parseFiniteInput(e.target.value, -1) },
                      })
                    }
                  />
                </label>
                <label className="virtual-field">
                  <span>{t("virtual.high")}</span>
                  <input
                    className="virtual-input"
                    type="number"
                    step="0.1"
                    value={op.mode.high ?? 1}
                    onChange={(e) =>
                      op.mode.mode === "window" &&
                      updateOp({
                        ...op,
                        mode: { ...op.mode, high: parseFiniteInput(e.target.value, 1) },
                      })
                    }
                  />
                </label>
              </div>
            )}
            <label className="virtual-check-row">
              <input
                type="checkbox"
                checked={op.mode.invert}
                onChange={(e) =>
                  updateOp({ ...op, mode: { ...op.mode, invert: e.target.checked } })
                }
              />
              <span>{t("virtual.invert")}</span>
            </label>
          </div>
        );

      case "to_state_machine":
        return (
          <div className="virtual-form-grid">
            <label className="virtual-field">
              <span>{t("virtual.outputPath")}</span>
              <input
                className="virtual-input"
                type="text"
                value={op.output_path}
                onChange={(e) => updateOp({ ...op, output_path: e.target.value })}
              />
            </label>
            <label className="virtual-field">
              <span>{t("virtual.mode")}</span>
              <select
                className="virtual-input"
                value={op.mode.mode}
                onChange={(e) =>
                  updateOp({
                    ...op,
                    mode:
                      e.target.value === "bit_combine"
                        ? {
                            mode: "bit_combine",
                            bit_channels: [defaults.firstDigital, defaults.secondBitCh],
                            threshold: 0.5,
                            labels: [],
                          }
                        : {
                            mode: "range_map",
                            input_channel: defaults.firstScalar,
                            hysteresis: 0.2,
                            rules: [
                              { min: null, max: -5, code: 1, name: "LOW", color_rgba: 0x38bdf8ff },
                              { min: -5, max: 5, code: 2, name: "NORM", color_rgba: 0x34d399ff },
                              { min: 5, max: null, code: 3, name: "HIGH", color_rgba: 0xfb7185ff },
                            ],
                            default_state: { code: 0, name: "IDLE", color_rgba: 0x94a3b8ff },
                          },
                  })
                }
              >
                <option value="range_map">{t("virtual.mode.range_map")}</option>
                <option value="bit_combine">{t("virtual.mode.bit_combine")}</option>
              </select>
            </label>

            {op.mode.mode === "range_map" ? (
              <>
                <div className="virtual-two-col">
                  <label className="virtual-field">
                    <span>{t("virtual.inputChannel")}</span>
                    <ChannelPathSelect
                      value={op.mode.input_channel}
                      channels={allChannels}
                      onChange={(v) =>
                        op.mode.mode === "range_map" &&
                        updateOp({ ...op, mode: { ...op.mode, input_channel: v } })
                      }
                    />
                  </label>
                  <label className="virtual-field">
                    <span>{t("virtual.hysteresis")}</span>
                    <input
                      className="virtual-input"
                      type="number"
                      step="0.05"
                      min="0"
                      value={op.mode.hysteresis ?? 0}
                      onChange={(e) =>
                        op.mode.mode === "range_map" &&
                        updateOp({
                          ...op,
                          mode: {
                            ...op.mode,
                            hysteresis: Math.max(0, parseFiniteInput(e.target.value, 0)),
                          },
                        })
                      }
                    />
                  </label>
                </div>

                <div className="virtual-subsection-header">
                  <span>{t("virtual.rules")}</span>
                  <button
                    type="button"
                    className="scope-btn-xs"
                    onClick={() => {
                      if (op.mode.mode !== "range_map") return;
                      const nextCode = op.mode.rules.length + 1;
                      updateOp({
                        ...op,
                        mode: {
                          ...op.mode,
                          rules: [
                            ...op.mode.rules,
                            {
                              min: 0,
                              max: 10,
                              code: nextCode,
                              name: `S${nextCode}`,
                              color_rgba: null,
                            },
                          ],
                        },
                      });
                    }}
                  >
                    {t("virtual.addRule")}
                  </button>
                </div>

                <div className="virtual-list-editor">
                  {op.mode.rules.map((rule, idx) => (
                    <div key={idx} className="virtual-rule-row">
                      <input
                        className="virtual-input"
                        type="number"
                        placeholder="-∞"
                        value={rule.min ?? ""}
                        onChange={(e) => {
                          if (op.mode.mode !== "range_map") return;
                          const rules = op.mode.rules.map((r, ri) =>
                            ri === idx ? { ...r, min: parseOptionalInput(e.target.value) } : r,
                          );
                          updateOp({ ...op, mode: { ...op.mode, rules } });
                        }}
                      />
                      <input
                        className="virtual-input"
                        type="number"
                        placeholder="+∞"
                        value={rule.max ?? ""}
                        onChange={(e) => {
                          if (op.mode.mode !== "range_map") return;
                          const rules = op.mode.rules.map((r, ri) =>
                            ri === idx ? { ...r, max: parseOptionalInput(e.target.value) } : r,
                          );
                          updateOp({ ...op, mode: { ...op.mode, rules } });
                        }}
                      />
                      <input
                        className="virtual-input"
                        type="number"
                        step="1"
                        value={rule.code}
                        onChange={(e) => {
                          if (op.mode.mode !== "range_map") return;
                          const rules = op.mode.rules.map((r, ri) =>
                            ri === idx
                              ? { ...r, code: Math.trunc(parseFiniteInput(e.target.value, r.code)) }
                              : r,
                          );
                          updateOp({ ...op, mode: { ...op.mode, rules } });
                        }}
                      />
                      <input
                        className="virtual-input"
                        type="text"
                        value={rule.name}
                        onChange={(e) => {
                          if (op.mode.mode !== "range_map") return;
                          const rules = op.mode.rules.map((r, ri) =>
                            ri === idx ? { ...r, name: e.target.value } : r,
                          );
                          updateOp({ ...op, mode: { ...op.mode, rules } });
                        }}
                      />
                      <button
                        type="button"
                        className="scope-btn-xs"
                        onClick={() => {
                          if (op.mode.mode !== "range_map") return;
                          updateOp({
                            ...op,
                            mode: {
                              ...op.mode,
                              rules: op.mode.rules.filter((_, ri) => ri !== idx),
                            },
                          });
                        }}
                      >
                        ×
                      </button>
                    </div>
                  ))}
                </div>

                <div className="virtual-two-col">
                  <label className="virtual-field">
                    <span>{t("virtual.defaultState")} Code</span>
                    <input
                      className="virtual-input"
                      type="number"
                      step="1"
                      value={op.mode.default_state.code}
                      onChange={(e) =>
                        op.mode.mode === "range_map" &&
                        updateOp({
                          ...op,
                          mode: {
                            ...op.mode,
                            default_state: {
                              ...op.mode.default_state,
                              code: Math.trunc(parseFiniteInput(e.target.value, 0)),
                            },
                          },
                        })
                      }
                    />
                  </label>
                  <label className="virtual-field">
                    <span>{t("virtual.defaultState")}</span>
                    <input
                      className="virtual-input"
                      type="text"
                      value={op.mode.default_state.name}
                      onChange={(e) =>
                        op.mode.mode === "range_map" &&
                        updateOp({
                          ...op,
                          mode: {
                            ...op.mode,
                            default_state: { ...op.mode.default_state, name: e.target.value },
                          },
                        })
                      }
                    />
                  </label>
                </div>
              </>
            ) : (
              <>
                <label className="virtual-field">
                  <span>{t("virtual.threshold")}</span>
                  <input
                    className="virtual-input"
                    type="number"
                    step="0.1"
                    value={op.mode.threshold ?? 0.5}
                    onChange={(e) =>
                      op.mode.mode === "bit_combine" &&
                      updateOp({
                        ...op,
                        mode: {
                          ...op.mode,
                          threshold: parseFiniteInput(e.target.value, 0.5),
                        },
                      })
                    }
                  />
                </label>
                <div className="virtual-subsection-header">
                  <span>{t("virtual.bitChannels")}</span>
                  <button
                    type="button"
                    className="scope-btn-xs"
                    disabled={op.mode.bit_channels.length >= 8}
                    onClick={() => {
                      if (op.mode.mode !== "bit_combine") return;
                      updateOp({
                        ...op,
                        mode: {
                          ...op.mode,
                          bit_channels: [...op.mode.bit_channels, defaults.firstDigital],
                        },
                      });
                    }}
                  >
                    {t("virtual.addBit")}
                  </button>
                </div>
                <div className="virtual-list-editor">
                  {op.mode.bit_channels.map((bCh, bi, bitArr) => (
                    <div key={bi} className="virtual-bit-row">
                      <span className="virtual-bit-idx">b{bi}</span>
                      <ChannelPathSelect
                        value={bCh}
                        channels={allChannels}
                        onChange={(nextPath) => {
                          if (op.mode.mode !== "bit_combine") return;
                          updateOp({
                            ...op,
                            mode: {
                              ...op.mode,
                              bit_channels: op.mode.bit_channels.map((p, pi) =>
                                pi === bi ? nextPath : p,
                              ),
                            },
                          });
                        }}
                      />
                      <button
                        type="button"
                        className="scope-btn-xs"
                        disabled={bitArr.length <= 1}
                        onClick={() => {
                          if (op.mode.mode !== "bit_combine") return;
                          updateOp({
                            ...op,
                            mode: {
                              ...op.mode,
                              bit_channels: op.mode.bit_channels.filter((_, pi) => pi !== bi),
                            },
                          });
                        }}
                      >
                        ×
                      </button>
                    </div>
                  ))}
                </div>
              </>
            )}
          </div>
        );

      case "to_analog":
        return (
          <div className="virtual-form-grid">
            <div className="virtual-two-col">
              <label className="virtual-field">
                <span>{t("virtual.outputPath")}</span>
                <input
                  className="virtual-input"
                  type="text"
                  value={op.output_path}
                  onChange={(e) => updateOp({ ...op, output_path: e.target.value })}
                />
              </label>
              <label className="virtual-field">
                <span>{t("virtual.unit")}</span>
                <input
                  className="virtual-input"
                  type="text"
                  value={op.unit ?? ""}
                  onChange={(e) => updateOp({ ...op, unit: e.target.value || null })}
                />
              </label>
            </div>
            <label className="virtual-field">
              <span>{t("virtual.mode")}</span>
              <select
                className="virtual-input"
                value={op.mode.mode}
                onChange={(e) => {
                  const nextKind = e.target.value;
                  if (nextKind === "bit_weighted_dac") {
                    updateOp({
                      ...op,
                      mode: {
                        mode: "bit_weighted_dac",
                        bit_channels: [defaults.firstDigital, defaults.secondBitCh],
                        scale: 1,
                        offset: 0,
                        threshold: 0.5,
                      },
                    });
                  } else if (nextKind === "state_value_map") {
                    updateOp({
                      ...op,
                      mode: {
                        mode: "state_value_map",
                        input_channel: defaults.firstFsm,
                        entries: [
                          { code: 0, value: 0 },
                          { code: 1, value: 3.3 },
                        ],
                        default_value: 0,
                      },
                    });
                  } else {
                    updateOp({
                      ...op,
                      mode: {
                        mode: "level_map",
                        input_channel: defaults.firstDigital,
                        low_value: 0,
                        high_value: 5,
                        threshold: 0.5,
                        smoothing_tau_ms: 0,
                      },
                    });
                  }
                }}
              >
                <option value="level_map">{t("virtual.mode.level_map")}</option>
                <option value="bit_weighted_dac">{t("virtual.mode.bit_weighted_dac")}</option>
                <option value="state_value_map">{t("virtual.mode.state_value_map")}</option>
              </select>
            </label>

            {op.mode.mode === "level_map" && (
              <>
                <label className="virtual-field">
                  <span>{t("virtual.inputChannel")}</span>
                  <ChannelPathSelect
                    value={op.mode.input_channel}
                    channels={allChannels}
                    onChange={(v) =>
                      op.mode.mode === "level_map" &&
                      updateOp({ ...op, mode: { ...op.mode, input_channel: v } })
                    }
                  />
                </label>
                <div className="virtual-two-col">
                  <label className="virtual-field">
                    <span>{t("virtual.lowValue")}</span>
                    <input
                      className="virtual-input"
                      type="number"
                      step="0.1"
                      value={op.mode.low_value ?? 0}
                      onChange={(e) =>
                        op.mode.mode === "level_map" &&
                        updateOp({
                          ...op,
                          mode: { ...op.mode, low_value: parseFiniteInput(e.target.value, 0) },
                        })
                      }
                    />
                  </label>
                  <label className="virtual-field">
                    <span>{t("virtual.highValue")}</span>
                    <input
                      className="virtual-input"
                      type="number"
                      step="0.1"
                      value={op.mode.high_value ?? 5}
                      onChange={(e) =>
                        op.mode.mode === "level_map" &&
                        updateOp({
                          ...op,
                          mode: { ...op.mode, high_value: parseFiniteInput(e.target.value, 5) },
                        })
                      }
                    />
                  </label>
                  <label className="virtual-field">
                    <span>{t("virtual.threshold")}</span>
                    <input
                      className="virtual-input"
                      type="number"
                      step="0.1"
                      value={op.mode.threshold ?? 0.5}
                      onChange={(e) =>
                        op.mode.mode === "level_map" &&
                        updateOp({
                          ...op,
                          mode: {
                            ...op.mode,
                            threshold: parseFiniteInput(e.target.value, 0.5),
                          },
                        })
                      }
                    />
                  </label>
                  <label className="virtual-field">
                    <span>{t("virtual.smoothingTauMs")}</span>
                    <input
                      className="virtual-input"
                      type="number"
                      min="0"
                      step="0.5"
                      value={op.mode.smoothing_tau_ms ?? 0}
                      onChange={(e) =>
                        op.mode.mode === "level_map" &&
                        updateOp({
                          ...op,
                          mode: {
                            ...op.mode,
                            smoothing_tau_ms: Math.max(0, parseFiniteInput(e.target.value, 0)),
                          },
                        })
                      }
                    />
                  </label>
                </div>
              </>
            )}

            {op.mode.mode === "bit_weighted_dac" && (
              <>
                <div className="virtual-two-col">
                  <label className="virtual-field">
                    <span>{t("virtual.scale")}</span>
                    <input
                      className="virtual-input"
                      type="number"
                      step="0.1"
                      value={op.mode.scale ?? 1}
                      onChange={(e) =>
                        op.mode.mode === "bit_weighted_dac" &&
                        updateOp({
                          ...op,
                          mode: { ...op.mode, scale: parseFiniteInput(e.target.value, 1) },
                        })
                      }
                    />
                  </label>
                  <label className="virtual-field">
                    <span>{t("virtual.offset")}</span>
                    <input
                      className="virtual-input"
                      type="number"
                      step="0.1"
                      value={op.mode.offset ?? 0}
                      onChange={(e) =>
                        op.mode.mode === "bit_weighted_dac" &&
                        updateOp({
                          ...op,
                          mode: { ...op.mode, offset: parseFiniteInput(e.target.value, 0) },
                        })
                      }
                    />
                  </label>
                </div>
                <div className="virtual-subsection-header">
                  <span>{t("virtual.bitChannels")}</span>
                  <button
                    type="button"
                    className="scope-btn-xs"
                    disabled={op.mode.bit_channels.length >= 16}
                    onClick={() => {
                      if (op.mode.mode !== "bit_weighted_dac") return;
                      updateOp({
                        ...op,
                        mode: {
                          ...op.mode,
                          bit_channels: [...op.mode.bit_channels, defaults.firstDigital],
                        },
                      });
                    }}
                  >
                    {t("virtual.addBit")}
                  </button>
                </div>
                <div className="virtual-list-editor">
                  {op.mode.bit_channels.map((bCh, bi, bitArr) => (
                    <div key={bi} className="virtual-bit-row">
                      <span className="virtual-bit-idx">b{bi}</span>
                      <ChannelPathSelect
                        value={bCh}
                        channels={allChannels}
                        onChange={(nextPath) => {
                          if (op.mode.mode !== "bit_weighted_dac") return;
                          updateOp({
                            ...op,
                            mode: {
                              ...op.mode,
                              bit_channels: op.mode.bit_channels.map((p, pi) =>
                                pi === bi ? nextPath : p,
                              ),
                            },
                          });
                        }}
                      />
                      <button
                        type="button"
                        className="scope-btn-xs"
                        disabled={bitArr.length <= 1}
                        onClick={() => {
                          if (op.mode.mode !== "bit_weighted_dac") return;
                          updateOp({
                            ...op,
                            mode: {
                              ...op.mode,
                              bit_channels: op.mode.bit_channels.filter((_, pi) => pi !== bi),
                            },
                          });
                        }}
                      >
                        ×
                      </button>
                    </div>
                  ))}
                </div>
              </>
            )}

            {op.mode.mode === "state_value_map" && (
              <>
                <div className="virtual-two-col">
                  <label className="virtual-field">
                    <span>{t("virtual.inputChannel")}</span>
                    <ChannelPathSelect
                      value={op.mode.input_channel}
                      channels={allChannels}
                      onChange={(v) =>
                        op.mode.mode === "state_value_map" &&
                        updateOp({ ...op, mode: { ...op.mode, input_channel: v } })
                      }
                    />
                  </label>
                  <label className="virtual-field">
                    <span>{t("virtual.defaultValue")}</span>
                    <input
                      className="virtual-input"
                      type="number"
                      step="0.1"
                      value={op.mode.default_value ?? 0}
                      onChange={(e) =>
                        op.mode.mode === "state_value_map" &&
                        updateOp({
                          ...op,
                          mode: {
                            ...op.mode,
                            default_value: parseFiniteInput(e.target.value, 0),
                          },
                        })
                      }
                    />
                  </label>
                </div>
                <div className="virtual-subsection-header">
                  <span>{t("virtual.stateValueEntries")}</span>
                  <button
                    type="button"
                    className="scope-btn-xs"
                    onClick={() => {
                      if (op.mode.mode !== "state_value_map") return;
                      updateOp({
                        ...op,
                        mode: {
                          ...op.mode,
                          entries: [
                            ...op.mode.entries,
                            { code: op.mode.entries.length, value: 0 },
                          ],
                        },
                      });
                    }}
                  >
                    {t("virtual.addEntry")}
                  </button>
                </div>
                <div className="virtual-list-editor">
                  {op.mode.entries.map((entry, ei) => (
                    <div key={ei} className="virtual-two-col">
                      <input
                        className="virtual-input"
                        type="number"
                        step="1"
                        value={entry.code}
                        onChange={(e) => {
                          if (op.mode.mode !== "state_value_map") return;
                          const entries = op.mode.entries.map((it, idx) =>
                            idx === ei
                              ? { ...it, code: Math.trunc(parseFiniteInput(e.target.value, it.code)) }
                              : it,
                          );
                          updateOp({ ...op, mode: { ...op.mode, entries } });
                        }}
                      />
                      <div className="virtual-bit-row">
                        <input
                          className="virtual-input"
                          type="number"
                          step="0.1"
                          value={entry.value ?? 0}
                          onChange={(e) => {
                            if (op.mode.mode !== "state_value_map") return;
                            const entries = op.mode.entries.map((it, idx) =>
                              idx === ei
                                ? { ...it, value: parseFiniteInput(e.target.value, 0) }
                                : it,
                            );
                            updateOp({ ...op, mode: { ...op.mode, entries } });
                          }}
                        />
                        <button
                          type="button"
                          className="scope-btn-xs"
                          onClick={() => {
                            if (op.mode.mode !== "state_value_map") return;
                            updateOp({
                              ...op,
                              mode: {
                                ...op.mode,
                                entries: op.mode.entries.filter((_, idx) => idx !== ei),
                              },
                            });
                          }}
                        >
                          ×
                        </button>
                      </div>
                    </div>
                  ))}
                </div>
              </>
            )}
          </div>
        );

      case "formula":
        return (
          <div className="virtual-form-grid">
            <div className="virtual-two-col">
              <label className="virtual-field">
                <span>{t("virtual.outputPath")}</span>
                <input
                  className="virtual-input"
                  type="text"
                  value={op.output_path}
                  onChange={(e) => updateOp({ ...op, output_path: e.target.value })}
                />
              </label>
              <label className="virtual-field">
                <span>{t("virtual.unit")}</span>
                <input
                  className="virtual-input"
                  type="text"
                  value={op.unit ?? ""}
                  onChange={(e) => updateOp({ ...op, unit: e.target.value || null })}
                />
              </label>
            </div>
            <label className="virtual-field">
              <span>{t("virtual.expression")}</span>
              <textarea
                className="virtual-input virtual-textarea"
                rows={2}
                value={op.expression}
                onChange={(e) => updateOp({ ...op, expression: e.target.value })}
              />
            </label>
            <div className="virtual-hint">{t("virtual.formulaHint")}</div>
            <div className="virtual-hint">{t("virtual.insertChannel")}</div>
            <div className="virtual-formula-pills">
              {allChannels.slice(0, 12).map((ch) => (
                <button
                  key={ch.channel_id}
                  type="button"
                  className="scope-btn-xs"
                  onClick={() =>
                    updateOp({
                      ...op,
                      expression: op.expression
                        ? `${op.expression} [${ch.full_path}]`
                        : `[${ch.full_path}]`,
                    })
                  }
                >
                  [{ch.full_path}]
                </button>
              ))}
            </div>
            <label className="virtual-check-row">
              <input
                type="checkbox"
                checked={op.output_digital}
                onChange={(e) => updateOp({ ...op, output_digital: e.target.checked })}
              />
              <span>{t("virtual.outputDigital")}</span>
            </label>
          </div>
        );

      case "window_stats":
        return (
          <div className="virtual-form-grid">
            <label className="virtual-field">
              <span>{t("virtual.inputChannel")}</span>
              <ChannelPathSelect
                value={op.input_channel}
                channels={allChannels}
                onChange={(v) => updateOp({ ...op, input_channel: v })}
              />
            </label>
            <div className="virtual-two-col">
              <label className="virtual-field">
                <span>{t("virtual.outputPath")}</span>
                <input
                  className="virtual-input"
                  type="text"
                  value={op.output_path}
                  onChange={(e) => updateOp({ ...op, output_path: e.target.value })}
                />
              </label>
              <label className="virtual-field">
                <span>{t("virtual.unit")}</span>
                <input
                  className="virtual-input"
                  type="text"
                  value={op.unit ?? ""}
                  onChange={(e) => updateOp({ ...op, unit: e.target.value || null })}
                />
              </label>
            </div>
            <div className="virtual-two-col">
              <label className="virtual-field">
                <span>{t("virtual.metric")}</span>
                <select
                  className="virtual-input"
                  value={op.metric}
                  onChange={(e) =>
                    updateOp({ ...op, metric: e.target.value as WindowMetricKindDto })
                  }
                >
                  {WINDOW_METRICS.map((mk) => (
                    <option key={mk} value={mk}>
                      {t(`virtual.metric.${mk}` as MessageKey)}
                    </option>
                  ))}
                </select>
              </label>
              <label className="virtual-field">
                <span>{t("virtual.windowMs")}</span>
                <input
                  className="virtual-input"
                  type="number"
                  step="1"
                  min="0.1"
                  value={op.window_ms ?? 20}
                  onChange={(e) =>
                    updateOp({
                      ...op,
                      window_ms: Math.max(0.1, parseFiniteInput(e.target.value, 20)),
                    })
                  }
                />
              </label>
            </div>
          </div>
        );

      case "calculus":
        return (
          <div className="virtual-form-grid">
            <label className="virtual-field">
              <span>{t("virtual.inputChannel")}</span>
              <ChannelPathSelect
                value={op.input_channel}
                channels={allChannels}
                onChange={(v) => updateOp({ ...op, input_channel: v })}
              />
            </label>
            <div className="virtual-two-col">
              <label className="virtual-field">
                <span>{t("virtual.outputPath")}</span>
                <input
                  className="virtual-input"
                  type="text"
                  value={op.output_path}
                  onChange={(e) => updateOp({ ...op, output_path: e.target.value })}
                />
              </label>
              <label className="virtual-field">
                <span>{t("virtual.unit")}</span>
                <input
                  className="virtual-input"
                  type="text"
                  value={op.unit ?? ""}
                  onChange={(e) => updateOp({ ...op, unit: e.target.value || null })}
                />
              </label>
            </div>
            <label className="virtual-field">
              <span>{t("virtual.mode")}</span>
              <select
                className="virtual-input"
                value={op.mode.mode}
                onChange={(e) =>
                  updateOp({
                    ...op,
                    mode:
                      e.target.value === "integral"
                        ? {
                            mode: "integral",
                            scale: op.mode.scale ?? 1,
                            dc_offset: 0,
                            initial_value: 0,
                            clamp_min: null,
                            clamp_max: null,
                          }
                        : {
                            mode: "derivative",
                            scale: op.mode.scale ?? 1,
                            filter_tau_ms: 0,
                          },
                  })
                }
              >
                <option value="derivative">{t("virtual.mode.derivative")}</option>
                <option value="integral">{t("virtual.mode.integral")}</option>
              </select>
            </label>
            {op.mode.mode === "derivative" ? (
              <div className="virtual-two-col">
                <label className="virtual-field">
                  <span>{t("virtual.scale")}</span>
                  <input
                    className="virtual-input"
                    type="number"
                    step="0.1"
                    value={op.mode.scale ?? 1}
                    onChange={(e) =>
                      op.mode.mode === "derivative" &&
                      updateOp({
                        ...op,
                        mode: { ...op.mode, scale: parseFiniteInput(e.target.value, 1) },
                      })
                    }
                  />
                </label>
                <label className="virtual-field">
                  <span>{t("virtual.filterTauMs")}</span>
                  <input
                    className="virtual-input"
                    type="number"
                    min="0"
                    step="0.5"
                    value={op.mode.filter_tau_ms ?? 0}
                    onChange={(e) =>
                      op.mode.mode === "derivative" &&
                      updateOp({
                        ...op,
                        mode: {
                          ...op.mode,
                          filter_tau_ms: Math.max(0, parseFiniteInput(e.target.value, 0)),
                        },
                      })
                    }
                  />
                </label>
              </div>
            ) : (
              <div className="virtual-two-col">
                <label className="virtual-field">
                  <span>{t("virtual.scale")}</span>
                  <input
                    className="virtual-input"
                    type="number"
                    step="0.1"
                    value={op.mode.scale ?? 1}
                    onChange={(e) =>
                      op.mode.mode === "integral" &&
                      updateOp({
                        ...op,
                        mode: { ...op.mode, scale: parseFiniteInput(e.target.value, 1) },
                      })
                    }
                  />
                </label>
                <label className="virtual-field">
                  <span>{t("virtual.dcOffset")}</span>
                  <input
                    className="virtual-input"
                    type="number"
                    step="0.1"
                    value={op.mode.dc_offset ?? 0}
                    onChange={(e) =>
                      op.mode.mode === "integral" &&
                      updateOp({
                        ...op,
                        mode: { ...op.mode, dc_offset: parseFiniteInput(e.target.value, 0) },
                      })
                    }
                  />
                </label>
                <label className="virtual-field">
                  <span>{t("virtual.initialValue")}</span>
                  <input
                    className="virtual-input"
                    type="number"
                    step="0.1"
                    value={op.mode.initial_value ?? 0}
                    onChange={(e) =>
                      op.mode.mode === "integral" &&
                      updateOp({
                        ...op,
                        mode: {
                          ...op.mode,
                          initial_value: parseFiniteInput(e.target.value, 0),
                        },
                      })
                    }
                  />
                </label>
                <label className="virtual-field">
                  <span>{t("virtual.clampMin")}</span>
                  <input
                    className="virtual-input"
                    type="number"
                    placeholder="-"
                    value={op.mode.clamp_min ?? ""}
                    onChange={(e) =>
                      op.mode.mode === "integral" &&
                      updateOp({
                        ...op,
                        mode: { ...op.mode, clamp_min: parseOptionalInput(e.target.value) },
                      })
                    }
                  />
                </label>
                <label className="virtual-field">
                  <span>{t("virtual.clampMax")}</span>
                  <input
                    className="virtual-input"
                    type="number"
                    placeholder="-"
                    value={op.mode.clamp_max ?? ""}
                    onChange={(e) =>
                      op.mode.mode === "integral" &&
                      updateOp({
                        ...op,
                        mode: { ...op.mode, clamp_max: parseOptionalInput(e.target.value) },
                      })
                    }
                  />
                </label>
              </div>
            )}
          </div>
        );

      case "single_phase_power": {
        const updateFlags = (patch: Partial<PowerOutputFlagsDto>) => {
          updateOp({ ...op, outputs: { ...op.outputs, ...patch } });
        };
        return (
          <div className="virtual-form-grid">
            <div className="virtual-two-col">
              <label className="virtual-field">
                <span>{t("virtual.power.voltageChannel")}</span>
                <ChannelPathSelect
                  value={op.voltage_channel}
                  channels={allChannels}
                  onChange={(v) => updateOp({ ...op, voltage_channel: v })}
                />
              </label>
              <label className="virtual-field">
                <span>{t("virtual.power.currentChannel")}</span>
                <ChannelPathSelect
                  value={op.current_channel}
                  channels={allChannels}
                  onChange={(v) => updateOp({ ...op, current_channel: v })}
                />
              </label>
            </div>
            <label className="virtual-field">
              <span>{t("virtual.outputPrefix")}</span>
              <input
                className="virtual-input"
                type="text"
                value={op.output_prefix}
                onChange={(e) => updateOp({ ...op, output_prefix: e.target.value })}
              />
            </label>
            <div className="virtual-two-col">
              <label className="virtual-field">
                <span>{t("virtual.power.vScale")}</span>
                <input
                  className="virtual-input"
                  type="number"
                  step="0.1"
                  value={op.voltage_scale ?? 1}
                  onChange={(e) =>
                    updateOp({ ...op, voltage_scale: parseFiniteInput(e.target.value, 1) })
                  }
                />
              </label>
              <label className="virtual-field">
                <span>{t("virtual.power.vOffset")}</span>
                <input
                  className="virtual-input"
                  type="number"
                  step="0.1"
                  value={op.voltage_offset ?? 0}
                  onChange={(e) =>
                    updateOp({ ...op, voltage_offset: parseFiniteInput(e.target.value, 0) })
                  }
                />
              </label>
              <label className="virtual-field">
                <span>{t("virtual.power.iScale")}</span>
                <input
                  className="virtual-input"
                  type="number"
                  step="0.1"
                  value={op.current_scale ?? 1}
                  onChange={(e) =>
                    updateOp({ ...op, current_scale: parseFiniteInput(e.target.value, 1) })
                  }
                />
              </label>
              <label className="virtual-field">
                <span>{t("virtual.power.iOffset")}</span>
                <input
                  className="virtual-input"
                  type="number"
                  step="0.1"
                  value={op.current_offset ?? 0}
                  onChange={(e) =>
                    updateOp({ ...op, current_offset: parseFiniteInput(e.target.value, 0) })
                  }
                />
              </label>
              <label className="virtual-field">
                <span>{t("virtual.power.nominalFreqHz")}</span>
                <input
                  className="virtual-input"
                  type="number"
                  min="1"
                  step="1"
                  value={op.nominal_freq_hz ?? 50}
                  onChange={(e) =>
                    updateOp({
                      ...op,
                      nominal_freq_hz: Math.max(1, parseFiniteInput(e.target.value, 50)),
                    })
                  }
                />
              </label>
              <label className="virtual-field">
                <span>{t("virtual.power.zeroCrossHyst")}</span>
                <input
                  className="virtual-input"
                  type="number"
                  min="0"
                  step="0.05"
                  value={op.zero_cross_hysteresis_v ?? 0.2}
                  onChange={(e) =>
                    updateOp({
                      ...op,
                      zero_cross_hysteresis_v: Math.max(
                        0,
                        parseFiniteInput(e.target.value, 0.2),
                      ),
                    })
                  }
                />
              </label>
              <label className="virtual-field">
                <span>{t("virtual.power.minFreqHz")}</span>
                <input
                  className="virtual-input"
                  type="number"
                  min="0.5"
                  step="0.5"
                  value={op.min_freq_hz ?? 5}
                  onChange={(e) =>
                    updateOp({
                      ...op,
                      min_freq_hz: Math.max(0.5, parseFiniteInput(e.target.value, 5)),
                    })
                  }
                />
              </label>
              <label className="virtual-field">
                <span>{t("virtual.power.fallbackWindowMs")}</span>
                <input
                  className="virtual-input"
                  type="number"
                  min="5"
                  step="5"
                  value={op.fallback_window_ms ?? 100}
                  onChange={(e) =>
                    updateOp({
                      ...op,
                      fallback_window_ms: Math.max(5, parseFiniteInput(e.target.value, 100)),
                    })
                  }
                />
              </label>
            </div>

            <div className="virtual-subsection-header">
              <span>{t("virtual.power.metricsTitle")}</span>
              <div className="virtual-mini-actions">
                <button
                  type="button"
                  className="scope-btn-xs"
                  onClick={() => updateFlags(normalizePowerFlags())}
                >
                  {t("virtual.power.selectAll")}
                </button>
                <button
                  type="button"
                  className="scope-btn-xs"
                  onClick={() => {
                    const nextFlags = {} as PowerOutputFlagsDto;
                    for (const k of POWER_METRIC_KEYS) {
                      nextFlags[k] = CORE_POWER_METRICS.has(k);
                    }
                    updateFlags(nextFlags);
                  }}
                >
                  {t("virtual.power.selectCore")}
                </button>
              </div>
            </div>

            <div className="virtual-power-flags">
              {POWER_METRIC_KEYS.map((mk) => (
                <label key={mk} className="virtual-power-flag-item">
                  <input
                    type="checkbox"
                    checked={op.outputs[mk]}
                    onChange={(e) => updateFlags({ [mk]: e.target.checked })}
                  />
                  <span>{t(`virtual.power.m.${mk}` as MessageKey)}</span>
                </label>
              ))}
            </div>
          </div>
        );
      }
    }
  };

  return (
    <div className="virtual-panel-wrap" data-testid="virtual-channel-panel">
      <div className="sidebar-config-bar">
        <button
          type="button"
          className="scope-btn-xs"
          title={t("virtual.exportTitle")}
          data-testid="export-virtual-btn"
          disabled={virtualChannels.length === 0}
          onClick={() => void handleExport()}
        >
          {t("virtual.export")}
        </button>
        <button
          type="button"
          className="scope-btn-xs"
          title={t("virtual.importTitle")}
          data-testid="import-virtual-btn"
          onClick={() => importInputRef.current?.click()}
        >
          {t("virtual.import")}
        </button>
        <button
          type="button"
          className="scope-btn-xs"
          data-testid="clear-virtual-btn"
          disabled={virtualChannels.length === 0}
          onClick={() => void onClearVirtualChannels()}
        >
          {t("virtual.clearAll")}
        </button>
        <input
          ref={importInputRef}
          type="file"
          accept=".json,application/json"
          style={{ display: "none" }}
          onChange={handleImportChange}
        />
      </div>

      {notice && (
        <div
          className={`config-io-notice ${notice.kind === "err" ? "err" : "ok"}`}
          data-testid="virtual-io-notice"
        >
          <span>{notice.text}</span>
          <button
            type="button"
            className="config-io-notice-close"
            onClick={() => setNotice(null)}
          >
            ×
          </button>
        </div>
      )}

      <div className="virtual-presets-section">
        <div className="virtual-presets-title">{t("virtual.presetsTitle")}</div>
        <div className="virtual-preset-grid">
          {(
            [
              "analog_to_digital",
              "to_state_machine",
              "to_analog",
              "formula",
              "window_stats",
              "calculus",
              "single_phase_power",
            ] as const
          ).map((kind) => (
            <button
              key={kind}
              type="button"
              className={`scope-btn-xs ${kind === "single_phase_power" ? "scope-btn-accent" : ""}`}
              data-testid={`add-virtual-${kind}`}
              onClick={() => void handleAddPreset(kind)}
            >
              {t(`virtual.preset.${kind}` as MessageKey)}
            </button>
          ))}
        </div>
      </div>

      <div className="virtual-cards-scroll">
        {virtualChannels.length === 0 ? (
          <div className="sidebar-empty-state" data-testid="virtual-empty-state">
            <h4>{t("virtual.emptyTitle")}</h4>
            <p>{t("virtual.emptyDesc")}</p>
          </div>
        ) : (
          virtualChannels.map((node) => {
            const draft = drafts[node.node_id] ?? node;
            const isExpanded = Boolean(expandedIds[node.node_id]);
            const hasUnsaved = Boolean(drafts[node.node_id]);
            const outputSpecs = node.enabled ? expandVirtualOutputSpecs(node) : [];
            const liveOutputs = outputSpecs
              .map((spec) => channelByFullPath.get(spec.fullPath))
              .filter((c): c is ChannelNodeDto_Serialize => Boolean(c));
            const errMsg = nodeErrors[node.node_id];

            return (
              <section
                key={node.node_id}
                className="virtual-card"
                data-testid={`virtual-card-${node.node_id}`}
              >
                <div className="virtual-card-header">
                  <label className="virtual-enable-toggle" title={t("virtual.enabled")}>
                    <input
                      type="checkbox"
                      checked={draft.enabled}
                      onChange={(e) => {
                        const checked = e.target.checked;
                        const nextCfg = { ...draft, enabled: checked };
                        if (isExpanded && hasUnsaved) {
                          setDraftFor(node, (prev) => ({ ...prev, enabled: checked }));
                        } else {
                          void applyNode(nextCfg);
                        }
                      }}
                    />
                  </label>
                  <button
                    type="button"
                    className="virtual-card-title-btn"
                    onClick={() =>
                      setExpandedIds((prev) => ({
                        ...prev,
                        [node.node_id]: !prev[node.node_id],
                      }))
                    }
                  >
                    <span className="virtual-card-name">{node.name}</span>
                    <span className="virtual-kind-badge">
                      {t(`virtual.op.${node.operator.kind}` as MessageKey)}
                    </span>
                  </button>
                  {node.operator.kind === "single_phase_power" && (
                    <button
                      type="button"
                      className="scope-btn-xs scope-btn-accent"
                      data-testid={`place-power-watch-${node.node_id}`}
                      onClick={() => {
                        const widgetId = onPlaceWatchTable(node.node_id);
                        if (widgetId) {
                          setNotice({
                            kind: "ok",
                            text: t("virtual.power.watchTablePlaced", { name: node.name }),
                          });
                        }
                      }}
                    >
                      {t("virtual.power.watchTable")}
                    </button>
                  )}
                  <button
                    type="button"
                    className="scope-btn-xs"
                    data-testid={`del-virtual-${node.node_id}`}
                    title={t("virtual.delete")}
                    onClick={() => void onRemoveVirtualChannel(node.node_id)}
                  >
                    ×
                  </button>
                </div>

                {errMsg && <div className="virtual-error-banner">{errMsg}</div>}

                {isExpanded && (
                  <div className="virtual-card-body">
                    <label className="virtual-field">
                      <span>{t("virtual.name")}</span>
                      <input
                        className="virtual-input"
                        type="text"
                        value={draft.name}
                        onChange={(e) =>
                          setDraftFor(node, (prev) => ({ ...prev, name: e.target.value }))
                        }
                      />
                    </label>
                    {renderOperatorFields(node, draft)}
                    <div className="virtual-card-footer">
                      <button
                        type="button"
                        className="scope-btn-xs scope-btn-accent"
                        data-testid={`apply-virtual-${node.node_id}`}
                        onClick={() => void applyNode(draft)}
                      >
                        {t("virtual.apply")}
                        {hasUnsaved ? " *" : ""}
                      </button>
                    </div>
                  </div>
                )}

                {liveOutputs.length > 0 && (
                  <div className="virtual-output-list">
                    <div className="virtual-outputs-caption">
                      {t("virtual.outputsTitle", { count: liveOutputs.length })}
                    </div>
                    {liveOutputs.map((ch) => {
                      const badge = modalityBadge(ch.modality);
                      return (
                        <div
                          key={ch.channel_id}
                          className="channel-row"
                          draggable
                          data-testid={`virtual-out-${ch.full_path}`}
                          onPointerDown={(ev) =>
                            startPointerDrag(ev, {
                              kind: "channel",
                              channelId: ch.channel_id,
                              modality: ch.modality,
                              fullPath: ch.full_path,
                              label: ch.full_path,
                            })
                          }
                          onDragStart={(ev) => handleDragStartOutput(ev, ch)}
                          title={t("sidebar.dragRowTitle")}
                        >
                          <span className={`ch-mod-badge ${badge.cls}`}>{t(badge.labelKey)}</span>
                          <span className="ch-path">{ch.local_path}</span>
                          {ch.unit && <span className="virtual-out-unit">{ch.unit}</span>}
                          <button
                            type="button"
                            className="ch-bind-btn"
                            title={
                              targetScopeId !== null
                                ? t("sidebar.bindTitle", { id: targetScopeId })
                                : t("stage.addScope")
                            }
                            onClick={() => handleBindOutput(ch.channel_id)}
                          >
                            +
                          </button>
                        </div>
                      );
                    })}
                  </div>
                )}
              </section>
            );
          })
        )}
      </div>
    </div>
  );
}
