import { useEffect, useRef, useState } from "react";
import type { ChannelNodeDto_Serialize } from "../bridge/bindings.ts";
import { bridgeClient } from "../bridge/control-client.ts";
import { DdsfSnapshotKind } from "../bridge/ddsf-spec.ts";
import type { UiLocale } from "../i18n/index.ts";
import { translate } from "../i18n/index.ts";
import { ChannelBindSelect } from "./ChannelBindSelect.tsx";
import type { DashboardWidgetItem } from "./types.ts";
import { WidgetConfigModal } from "./WidgetConfigModal.tsx";
import { WidgetHeaderActions } from "./WidgetHeaderSlot.tsx";

export interface StatusLedWidgetProps {
  widget: DashboardWidgetItem;
  channelsById: Record<number, ChannelNodeDto_Serialize>;
  locale: UiLocale;
  onBindChannel?: (channelId: number) => void;
}

export function StatusLedWidget({
  widget,
  channelsById,
  locale,
  onBindChannel,
}: StatusLedWidgetProps) {
  const t = (key: Parameters<typeof translate>[1], params?: Record<string, string | number>) =>
    translate(locale, key, params);

  const [configOpen, setConfigOpen] = useState(false);
  const lampRef = useRef<HTMLSpanElement | null>(null);
  const labelRef = useRef<HTMLDivElement | null>(null);
  const enumPillRefs = useRef<Map<number, HTMLSpanElement>>(new Map());

  const binding = widget.primaryBinding;
  const ch = binding ? channelsById[binding.channelId] : undefined;
  const stateRef = useRef({ ch, locale });
  stateRef.current = { ch, locale };

  useEffect(() => {
    const chId = binding?.channelId;
    if (chId === undefined) return;

    const applySample = (rawVal: number, kind: number) => {
      const { ch: curCh, locale: curLocale } = stateRef.current;
      if (kind === DdsfSnapshotKind.StateEnum || curCh?.modality === "state_machine") {
        const code = Math.round(rawVal);
        const match = curCh?.state_labels.find((l) => l.code === code);
        const text = match ? match.name : `STATE_${code}`;
        const tone = code === 3 ? "fault" : code === 2 ? "ok" : code === 1 ? "warn" : "idle";
        if (lampRef.current) lampRef.current.className = `widget-led-lamp tone-${tone}`;
        if (labelRef.current) labelRef.current.textContent = text;
        enumPillRefs.current.forEach((pill, itemCode) => {
          pill.className = itemCode === code ? `widget-fsm-pill active tone-${tone}` : "widget-fsm-pill";
        });
      } else if (kind === DdsfSnapshotKind.Bool || curCh?.modality === "digital_bool") {
        const high = rawVal > 0.5;
        if (lampRef.current) lampRef.current.className = high ? "widget-led-lamp tone-ok" : "widget-led-lamp tone-idle";
        if (labelRef.current) {
          labelRef.current.textContent = high
            ? translate(curLocale, "sidebar.valHigh")
            : translate(curLocale, "sidebar.valLow");
        }
      } else {
        const active = rawVal >= 0.5;
        if (lampRef.current) lampRef.current.className = active ? "widget-led-lamp tone-ok" : "widget-led-lamp tone-idle";
        if (labelRef.current) labelRef.current.textContent = rawVal.toFixed(2);
      }
    };

    const seed = bridgeClient.topicBus.getLatestSnapshot(chId);
    if (seed) {
      applySample(seed.valF64, seed.kind);
    }

    let lastFlushMs = 0;
    let latestVal = seed?.valF64 ?? 0;
    let latestKind = seed?.kind ?? DdsfSnapshotKind.Scalar;
    let pending = false;

    const unsub = bridgeClient.topicBus.subscribeSnapshots((snaps) => {
      for (let i = 0; i < snaps.length; i += 1) {
        if (snaps[i].channelNumId === chId) {
          latestVal = snaps[i].valF64;
          latestKind = snaps[i].kind;
          pending = true;
          break;
        }
      }
      if (!pending) return;
      const now = performance.now();
      if (now - lastFlushMs < 55) return;
      lastFlushMs = now;
      pending = false;
      applySample(latestVal, latestKind);
    });
    return unsub;
  }, [binding?.channelId, locale]);

  const headerControls = (
    <WidgetHeaderActions>
      <button
        type="button"
        className={`widget-header-btn ${configOpen ? "active" : ""}`}
        title={t("widget.configToggle")}
        data-testid={`led-config-btn-${widget.id}`}
        onClick={() => setConfigOpen((v) => !v)}
      >
        ⚙
      </button>
    </WidgetHeaderActions>
  );

  const configOverlay = (
    <WidgetConfigModal
      open={configOpen}
      title={`${widget.title} · ${t("widget.configToggle")}`}
      doneLabel={t("widget.configDone")}
      width="sm"
      onClose={() => setConfigOpen(false)}
    >
      <div className="widget-inline-config">
        <label style={{ gridColumn: "span 2" }}>
          {t("widget.bindChannelLabel")}
          <ChannelBindSelect
            selectedChannelId={binding?.channelId ?? null}
            channelsById={channelsById}
            placeholder={t("widget.selectChannel")}
            title={t("widget.switchChannelTitle")}
            className="widget-config-select"
            dataTestId={`led-bind-select-${widget.id}`}
            onBindChannel={onBindChannel}
          />
        </label>
      </div>
    </WidgetConfigModal>
  );

  if (!binding) {
    return (
      <>
        {headerControls}
        <div className="widget-unbound-box">
          <span>{t("widget.unbound")}</span>
          {onBindChannel && (
            <button
              type="button"
              className="scope-btn-xs"
              onClick={() => setConfigOpen(true)}
            >
              {t("widget.selectChannel")}
            </button>
          )}
        </div>
        {configOverlay}
      </>
    );
  }

  return (
    <div className="widget-led-body" data-testid={`widget-led-${widget.id}`}>
      {headerControls}
      <div className="widget-led-center">
        <span className="widget-led-lamp tone-idle" ref={lampRef} />
        <div className="widget-led-label" ref={labelRef}>
          --
        </div>
      </div>
      {ch && ch.state_labels.length > 0 && (
        <div className="widget-fsm-row">
          {ch.state_labels.map((lbl) => (
            <span
              key={lbl.code}
              className="widget-fsm-pill"
              ref={(el) => {
                if (el) enumPillRefs.current.set(lbl.code, el);
                else enumPillRefs.current.delete(lbl.code);
              }}
            >
              {lbl.name}
            </span>
          ))}
        </div>
      )}
      {configOverlay}
    </div>
  );
}
