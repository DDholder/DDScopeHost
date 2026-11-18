import { useCallback, useEffect, useMemo, useRef } from "react";
import type { ChannelNodeDto_Serialize } from "../bridge/bindings.ts";
import { bridgeClient } from "../bridge/control-client.ts";
import { DdsfSnapshotKind } from "../bridge/ddsf-spec.ts";
import type { UiLocale } from "../i18n/index.ts";
import { translate } from "../i18n/index.ts";
import { formatMetricValue } from "../scope-engine/time-format.ts";
import { ChannelBindSelect } from "./ChannelBindSelect.tsx";
import type { DashboardWidgetItem, WidgetChannelBinding } from "./types.ts";

export interface WatchTableWidgetProps {
  widget: DashboardWidgetItem;
  channelsById: Record<number, ChannelNodeDto_Serialize>;
  locale: UiLocale;
  onUpdateWidget: (updater: (prev: DashboardWidgetItem) => DashboardWidgetItem) => void;
  onBindChannel?: (channelId: number) => void;
}

export function WatchTableWidget({
  widget,
  channelsById,
  locale,
  onUpdateWidget,
  onBindChannel,
}: WatchTableWidgetProps) {
  const t = (key: Parameters<typeof translate>[1], params?: Record<string, string | number>) =>
    translate(locale, key, params);

  const valCellRefs = useRef<Map<number, HTMLTableCellElement>>(new Map());

  const bindingsRef = useRef<WidgetChannelBinding[]>(widget.bindings);
  bindingsRef.current = widget.bindings;
  const channelsRef = useRef(channelsById);
  channelsRef.current = channelsById;
  const localeRef = useRef(locale);
  localeRef.current = locale;
  const latestMapRef = useRef<Map<number, { kind: number; val: number }>>(new Map());

  const flushRows = useCallback(() => {
    const curLocale = localeRef.current;
    const chMap = channelsRef.current;
    const snaps = latestMapRef.current;
    const rows = bindingsRef.current;

    for (let i = 0; i < rows.length; i += 1) {
      const b = rows[i];
      const snap = snaps.get(b.channelId);
      if (!snap) continue;
      const ch = chMap[b.channelId];
      const valCell = valCellRefs.current.get(b.channelId);
      if (!valCell) continue;

      if (snap.kind === DdsfSnapshotKind.Bool) {
        valCell.textContent =
          snap.val > 0.5
            ? translate(curLocale, "sidebar.valHigh")
            : translate(curLocale, "sidebar.valLow");
        valCell.className = snap.val > 0.5 ? "watch-val val-high" : "watch-val val-low";
      } else if (snap.kind === DdsfSnapshotKind.StateEnum) {
        const code = Math.round(snap.val);
        valCell.textContent =
          ch?.state_labels.find((l) => l.code === code)?.name ?? `S${code}`;
        valCell.className = code === 3 ? "watch-val val-fault" : "watch-val val-state";
      } else {
        valCell.textContent = formatMetricValue(snap.val, ch?.unit ?? null);
        valCell.className = "watch-val";
      }
    }
  }, []);

  useEffect(() => {
    for (const b of bindingsRef.current) {
      const seed = bridgeClient.topicBus.getLatestSnapshot(b.channelId);
      if (seed) {
        latestMapRef.current.set(b.channelId, { kind: seed.kind, val: seed.valF64 });
      }
    }
    flushRows();

    let lastFlushMs = 0;
    const unsub = bridgeClient.topicBus.subscribeSnapshots((snaps) => {
      for (let i = 0; i < snaps.length; i += 1) {
        const s = snaps[i];
        latestMapRef.current.set(s.channelNumId, { kind: s.kind, val: s.valF64 });
      }
      const now = performance.now();
      if (now - lastFlushMs < 65) return;
      lastFlushMs = now;
      flushRows();
    });
    return unsub;
  }, [flushRows]);

  useEffect(() => {
    for (const b of widget.bindings) {
      const seed = bridgeClient.topicBus.getLatestSnapshot(b.channelId);
      if (seed) {
        latestMapRef.current.set(b.channelId, { kind: seed.kind, val: seed.valF64 });
      }
    }
    flushRows();
  }, [widget.bindings, channelsById, locale, flushRows]);

  const boundChannelIds = useMemo(
    () => new Set(widget.bindings.map((b) => b.channelId)),
    [widget.bindings],
  );

  const handleAddChannel = useCallback(
    (channelId: number) => {
      if (onBindChannel) {
        onBindChannel(channelId);
        return;
      }
      onUpdateWidget((prev) =>
        prev.bindings.some((b) => b.channelId === channelId)
          ? prev
          : {
              ...prev,
              bindings: [...prev.bindings, { channelId, scale: 1, offset: 0 }],
            },
      );
    },
    [onBindChannel, onUpdateWidget],
  );

  if (widget.bindings.length === 0) {
    return (
      <div className="widget-unbound-box">
        <span>{t("widget.watch.empty")}</span>
        <ChannelBindSelect
          channelsById={channelsById}
          placeholder={t("widget.selectChannel")}
          dataTestId={`watch-add-select-${widget.id}`}
          onBindChannel={handleAddChannel}
        />
      </div>
    );
  }

  return (
    <div className="widget-watch-wrap" data-testid={`widget-watch-${widget.id}`}>
      <div className="watch-table-toolbar">
        <span className="watch-count-badge">
          {t("widget.watch.countBadge", { count: widget.bindings.length })}
        </span>
        <ChannelBindSelect
          channelsById={channelsById}
          excludeChannelIds={boundChannelIds}
          placeholder={t("widget.watch.addChannel")}
          className="watch-add-select"
          dataTestId={`watch-add-select-${widget.id}`}
          onBindChannel={handleAddChannel}
        />
      </div>
      <div className="widget-watch-table-scroll">
        <table className="widget-watch-table">
        <thead>
          <tr>
            <th>{t("widget.watch.ch")}</th>
            <th>{t("widget.watch.scaled")}</th>
            <th />
          </tr>
        </thead>
        <tbody>
          {widget.bindings.map((b) => {
            const ch = channelsById[b.channelId];
            return (
              <tr key={b.channelId}>
                <td className="watch-ch-name">{ch ? ch.full_path : `ch${b.channelId}`}</td>
                <td
                  className="watch-val"
                  ref={(el) => {
                    if (el) valCellRefs.current.set(b.channelId, el);
                    else valCellRefs.current.delete(b.channelId);
                  }}
                >
                  --
                </td>
                <td>
                  <button
                    type="button"
                    className="scope-ch-del"
                    title={t("scope.unbindTrack")}
                    onClick={() =>
                      onUpdateWidget((prev) => ({
                        ...prev,
                        bindings: prev.bindings.filter((item) => item.channelId !== b.channelId),
                      }))
                    }
                  >
                    ×
                  </button>
                </td>
              </tr>
            );
          })}
        </tbody>
        </table>
      </div>
    </div>
  );
}
