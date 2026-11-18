import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import type {
  ChannelNodeDto_Serialize,
  DownlinkReceiptDto,
  PluginParamSpecDto_Deserialize,
  SendDownlinkRequestDto_Deserialize,
} from "../bridge/bindings.ts";
import { bridgeClient } from "../bridge/control-client.ts";
import { DdsfSnapshotKind } from "../bridge/ddsf-spec.ts";
import type { UiLocale } from "../i18n/index.ts";
import { translate } from "../i18n/index.ts";
import { ChannelBindSelect } from "./ChannelBindSelect.tsx";
import {
  getExternalPluginCatalog,
  loadMountedWidgetPluginModule,
  parsePluginParamDefault,
  pluginIdFromWidgetType,
  resolvePluginText,
  subscribeExternalPluginCatalog,
  type MountedWidgetPluginInstance,
  type PluginWidgetType,
} from "./plugin-registry.ts";
import type { DashboardWidgetItem } from "./types.ts";

export interface PluginWidgetHostProps {
  widget: DashboardWidgetItem;
  channelsById: Record<number, ChannelNodeDto_Serialize>;
  locale: UiLocale;
  onUpdateWidget: (updater: (prev: DashboardWidgetItem) => DashboardWidgetItem) => void;
  onBindChannel?: (channelId: number) => void;
  onSendDownlink?: (
    req: SendDownlinkRequestDto_Deserialize,
    labelSummary?: string,
  ) => Promise<DownlinkReceiptDto>;
}

export function PluginWidgetHost({
  widget,
  channelsById,
  locale,
  onUpdateWidget,
  onBindChannel,
  onSendDownlink,
}: PluginWidgetHostProps) {
  const t = (key: Parameters<typeof translate>[1], params?: Record<string, string | number>) =>
    translate(locale, key, params);

  const catalog = useSyncExternalStore(
    subscribeExternalPluginCatalog,
    getExternalPluginCatalog,
    getExternalPluginCatalog,
  );
  const pluginId = pluginIdFromWidgetType(widget.type as PluginWidgetType);
  const pluginInfo = catalog.widgets.find((w) => w.id === pluginId);

  const [configOpen, setConfigOpen] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const mountRef = useRef<HTMLDivElement | null>(null);
  const instanceRef = useRef<MountedWidgetPluginInstance | null>(null);

  const primaryChannelId = widget.primaryBinding?.channelId ?? null;
  const primaryChannel =
    primaryChannelId !== null ? channelsById[primaryChannelId] : undefined;
  const boundChannels = widget.bindings
    .map((b) => channelsById[b.channelId])
    .filter((c): c is ChannelNodeDto_Serialize => Boolean(c));

  const latestValuesRef = useRef<Record<number, { value: number; kind: number }>>({});
  const stateInputsRef = useRef({
    locale,
    config: widget.pluginConfig ?? {},
    primaryChannel: primaryChannel ?? null,
    channels: boundChannels,
  });
  stateInputsRef.current = {
    locale,
    config: widget.pluginConfig ?? {},
    primaryChannel: primaryChannel ?? null,
    channels: boundChannels,
  };

  const pushStateToPlugin = () => {
    const inst = instanceRef.current;
    if (!inst || typeof inst.update !== "function") return;
    const cur = stateInputsRef.current;
    const pId = cur.primaryChannel?.channel_id ?? null;
    const pSnap = pId !== null ? latestValuesRef.current[pId] : undefined;
    const valuesByChannelId: Record<number, number> = {};
    for (const [idStr, snap] of Object.entries(latestValuesRef.current)) {
      valuesByChannelId[Number(idStr)] = snap.value;
    }
    try {
      inst.update({
        locale: cur.locale,
        config: cur.config,
        primaryChannel: cur.primaryChannel,
        value: pSnap?.value ?? null,
        rawKind: pSnap?.kind ?? DdsfSnapshotKind.Scalar,
        channels: cur.channels,
        valuesByChannelId,
      });
    } catch (err) {
      setLoadError(err instanceof Error ? err.message : String(err));
    }
  };

  // Seed latestValuesRef whenever bound channels change
  useEffect(() => {
    const ids = new Set<number>();
    if (primaryChannelId !== null) ids.add(primaryChannelId);
    for (const b of widget.bindings) ids.add(b.channelId);

    for (const id of ids) {
      const seed = bridgeClient.topicBus.getLatestSnapshot(id);
      if (seed) {
        latestValuesRef.current[id] = { value: seed.valF64, kind: seed.kind };
      }
    }
    pushStateToPlugin();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [primaryChannelId, widget.bindings, widget.pluginConfig, locale, channelsById]);

  // Mount & clean up the external widget ES module
  useEffect(() => {
    let cancelled = false;
    const el = mountRef.current;
    if (!el) return;

    if (pluginInfo && !pluginInfo.available) {
      setLoadError(pluginInfo.error ?? t("widget.plugin.unavailable", { id: pluginId }));
      return;
    }

    setLoadError(null);
    loadMountedWidgetPluginModule(pluginId, (id) =>
      bridgeClient.loadWidgetPluginBundle(id),
    )
      .then((mod) => {
        if (cancelled || !mountRef.current) return;
        const instance = mod.mount(mountRef.current, {
          sendDownlink: (payload) => {
            if (onSendDownlink && payload && typeof payload === "object") {
              void onSendDownlink(payload as SendDownlinkRequestDto_Deserialize);
            }
          },
          updateConfig: (patch) => {
            if (!patch || typeof patch !== "object") return;
            onUpdateWidget((prev) => ({
              ...prev,
              pluginConfig: {
                ...(prev.pluginConfig ?? {}),
                ...patch,
              },
            }));
          },
        });
        instanceRef.current = instance ?? null;
        pushStateToPlugin();
      })
      .catch((err: unknown) => {
        if (cancelled) return;
        setLoadError(err instanceof Error ? err.message : String(err));
      });

    return () => {
      cancelled = true;
      try {
        instanceRef.current?.unmount?.();
      } catch {
        // Ignore unmount errors from third-party widget scripts
      }
      instanceRef.current = null;
      if (mountRef.current) {
        mountRef.current.replaceChildren();
      }
    };
    // Re-mount if catalog reloads or plugin availability changes
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pluginId, pluginInfo?.available, pluginInfo?.manifest_path, catalog]);

  // Throttled live snapshot subscription (~40 Hz max DOM updates inside plugin)
  useEffect(() => {
    const watched = new Set<number>();
    if (primaryChannelId !== null) watched.add(primaryChannelId);
    for (const b of widget.bindings) watched.add(b.channelId);
    if (watched.size === 0) return;

    let pending = false;
    let lastFlushMs = 0;

    const unsub = bridgeClient.topicBus.subscribeSnapshots((snaps) => {
      let dirty = false;
      for (let i = 0; i < snaps.length; i += 1) {
        const snap = snaps[i];
        if (watched.has(snap.channelNumId)) {
          latestValuesRef.current[snap.channelNumId] = {
            value: snap.valF64,
            kind: snap.kind,
          };
          dirty = true;
        }
      }
      if (!dirty) return;
      const now = performance.now();
      if (now - lastFlushMs >= 25) {
        lastFlushMs = now;
        pending = false;
        pushStateToPlugin();
      } else if (!pending) {
        pending = true;
        requestAnimationFrame(() => {
          if (!pending) return;
          pending = false;
          lastFlushMs = performance.now();
          pushStateToPlugin();
        });
      }
    });

    return unsub;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [primaryChannelId, widget.bindings]);

  const bindingMode = pluginInfo?.binding_mode ?? "single_channel";
  const params = pluginInfo?.params ?? [];
  const cfg = widget.pluginConfig ?? {};

  const updateParamValue = (param: PluginParamSpecDto_Deserialize, rawVal: unknown) => {
    onUpdateWidget((prev) => ({
      ...prev,
      pluginConfig: {
        ...(prev.pluginConfig ?? {}),
        [param.key]: rawVal,
      },
    }));
  };

  return (
    <div className="plugin-widget-host" data-testid={`plugin-widget-${pluginId}`}>
      {(bindingMode !== "none" || params.length > 0) && (
        <div className="plugin-widget-toolbar">
          {bindingMode === "single_channel" && (
            <ChannelBindSelect
              channelsById={channelsById}
              placeholder={
                primaryChannel ? primaryChannel.full_path : t("widget.selectChannel")
              }
              excludeChannelIds={
                primaryChannelId !== null ? new Set([primaryChannelId]) : undefined
              }
              onBindChannel={onBindChannel}
            />
          )}
          {bindingMode === "multi_channel" && (
            <div className="plugin-widget-multi-bind">
              <ChannelBindSelect
                channelsById={channelsById}
                placeholder={t("widget.selectChannel")}
                excludeChannelIds={new Set(widget.bindings.map((b) => b.channelId))}
                onBindChannel={onBindChannel}
              />
              {boundChannels.map((ch) => (
                <span key={ch.channel_id} className="plugin-widget-ch-chip">
                  <span>{ch.full_path}</span>
                  <button
                    type="button"
                    className="scope-ch-del"
                    onClick={() =>
                      onUpdateWidget((prev) => ({
                        ...prev,
                        bindings: prev.bindings.filter((b) => b.channelId !== ch.channel_id),
                      }))
                    }
                  >
                    ×
                  </button>
                </span>
              ))}
            </div>
          )}
          {params.length > 0 && (
            <button
              type="button"
              className="widget-inline-btn"
              onClick={() => setConfigOpen((v) => !v)}
            >
              {t("widget.configToggle")}
            </button>
          )}
        </div>
      )}

      {configOpen && params.length > 0 && (
        <div className="plugin-widget-config" onPointerDown={(ev) => ev.stopPropagation()}>
          {params.map((p) => {
            const label = resolvePluginText(p.label_en, p.label_zh, locale) || p.key;
            const curVal = cfg[p.key] ?? parsePluginParamDefault(p.default_json);
            if (p.param_type === "boolean") {
              return (
                <label key={p.key} className="plugin-widget-param-check">
                  <input
                    type="checkbox"
                    checked={Boolean(curVal)}
                    onChange={(e) => updateParamValue(p, e.target.checked)}
                  />
                  <span>{label}</span>
                </label>
              );
            }
            if (p.param_type === "select") {
              return (
                <label key={p.key} className="plugin-widget-param-field">
                  <span>{label}</span>
                  <select
                    value={String(curVal ?? p.options[0]?.value ?? "")}
                    onChange={(e) => updateParamValue(p, e.target.value)}
                  >
                    {p.options.map((opt) => (
                      <option key={opt.value} value={opt.value}>
                        {resolvePluginText(opt.label_en, opt.label_zh, locale) || opt.value}
                      </option>
                    ))}
                  </select>
                </label>
              );
            }
            if (p.param_type === "number") {
              return (
                <label key={p.key} className="plugin-widget-param-field">
                  <span>{label}</span>
                  <input
                    type="number"
                    min={p.min ?? undefined}
                    max={p.max ?? undefined}
                    step={p.step ?? "any"}
                    value={typeof curVal === "number" ? curVal : Number(curVal ?? 0)}
                    onChange={(e) => {
                      const num = Number(e.target.value);
                      if (Number.isFinite(num)) {
                        updateParamValue(p, num);
                      }
                    }}
                  />
                </label>
              );
            }
            return (
              <label key={p.key} className="plugin-widget-param-field">
                <span>{label}</span>
                <input
                  type="text"
                  value={String(curVal ?? "")}
                  onChange={(e) => updateParamValue(p, e.target.value)}
                />
              </label>
            );
          })}
        </div>
      )}

      {loadError ? (
        <div className="plugin-widget-error">
          <strong>{t("widget.plugin.missingTitle", { id: pluginId })}</strong>
          <span>{loadError}</span>
        </div>
      ) : (
        <div ref={mountRef} className="plugin-widget-mount" />
      )}
    </div>
  );
}
