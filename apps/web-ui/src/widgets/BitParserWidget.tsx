import { useEffect, useMemo, useRef, useState } from "react";
import type { ChannelNodeDto_Serialize } from "../bridge/bindings.ts";
import { bridgeClient } from "../bridge/control-client.ts";
import type { UiLocale } from "../i18n/index.ts";
import { translate } from "../i18n/index.ts";
import {
  VALID_BIT_TONES,
  VALID_BIT_WIDTHS,
  createDefaultBitParserConfig,
  evaluateActiveBits,
  formatBitWordHex,
  isBitSet,
  toUnsignedBitWord,
  upsertBitConfigItem,
} from "./bit-parser.ts";
import { ChannelBindSelect } from "./ChannelBindSelect.tsx";
import type {
  BitParserTone,
  BitParserWidth,
  DashboardWidgetItem,
  WidgetBitItem,
  WidgetBitParserConfig,
} from "./types.ts";

export interface BitParserWidgetProps {
  widget: DashboardWidgetItem;
  channelsById: Record<number, ChannelNodeDto_Serialize>;
  locale: UiLocale;
  onUpdateWidget: (updater: (prev: DashboardWidgetItem) => DashboardWidgetItem) => void;
  onBindChannel?: (channelId: number) => void;
}

/**
 * Decodes a bound channel's integer sample into individual bits and displays
 * the configured message for each bit whose live state is `1`.
 */
export function BitParserWidget({
  widget,
  channelsById,
  locale,
  onUpdateWidget,
  onBindChannel,
}: BitParserWidgetProps) {
  const t = (key: Parameters<typeof translate>[1], params?: Record<string, string | number>) =>
    translate(locale, key, params);

  const [configOpen, setConfigOpen] = useState(false);
  const [liveWord, setLiveWord] = useState<number | null>(null);
  const prevWordRef = useRef<number | null>(null);

  const cfg = widget.bitParserConfig ?? createDefaultBitParserConfig();
  const binding = widget.primaryBinding;
  const ch = binding ? channelsById[binding.channelId] : undefined;

  useEffect(() => {
    const chId = binding?.channelId;
    if (chId === undefined) {
      prevWordRef.current = null;
      setLiveWord(null);
      return;
    }

    const commitRaw = (rawVal: number) => {
      const nextWord = toUnsignedBitWord(rawVal, cfg.bitWidth);
      if (nextWord !== prevWordRef.current) {
        prevWordRef.current = nextWord;
        setLiveWord(nextWord);
      }
    };

    const seed = bridgeClient.topicBus.getLatestSnapshot(chId);
    if (seed) {
      commitRaw(seed.valF64);
    } else {
      prevWordRef.current = null;
      setLiveWord(null);
    }

    let lastFlushMs = 0;
    let latestVal = seed?.valF64 ?? 0;
    let pending = false;

    const unsub = bridgeClient.topicBus.subscribeSnapshots((snaps) => {
      for (let i = 0; i < snaps.length; i += 1) {
        if (snaps[i].channelNumId === chId) {
          latestVal = snaps[i].valF64;
          pending = true;
          break;
        }
      }
      if (!pending) return;
      const now = performance.now();
      if (now - lastFlushMs < 60) return;
      lastFlushMs = now;
      pending = false;
      commitRaw(latestVal);
    });

    return unsub;
  }, [binding?.channelId, cfg.bitWidth]);

  const bitMap = useMemo(() => {
    const map = new Map<number, WidgetBitItem>();
    for (let i = 0; i < cfg.bits.length; i += 1) {
      const item = cfg.bits[i];
      if (item.bit >= 0 && item.bit < cfg.bitWidth) {
        map.set(item.bit, item);
      }
    }
    return map;
  }, [cfg.bits, cfg.bitWidth]);

  const configuredCount = useMemo(() => {
    let count = 0;
    bitMap.forEach((item) => {
      if (item.label.trim().length > 0) count += 1;
    });
    return count;
  }, [bitMap]);

  const activeBits = useMemo(
    () => (liveWord !== null ? evaluateActiveBits(liveWord, cfg) : []),
    [liveWord, cfg],
  );

  const msbToLsbIndices = useMemo(
    () => Array.from({ length: cfg.bitWidth }, (_, i) => cfg.bitWidth - 1 - i),
    [cfg.bitWidth],
  );

  const lsbToMsbIndices = useMemo(
    () => Array.from({ length: cfg.bitWidth }, (_, i) => i),
    [cfg.bitWidth],
  );

  const updateBitConfig = (
    updater: (prevCfg: WidgetBitParserConfig) => WidgetBitParserConfig,
  ) => {
    onUpdateWidget((prev) => ({
      ...prev,
      bitParserConfig: updater(prev.bitParserConfig ?? createDefaultBitParserConfig()),
    }));
  };

  if (!binding) {
    return (
      <div className="widget-unbound-box">
        <span>{t("widget.unbound")}</span>
        <ChannelBindSelect
          channelsById={channelsById}
          placeholder={t("widget.selectChannel")}
          dataTestId={`bit-parser-bind-${widget.id}`}
          onBindChannel={onBindChannel}
        />
      </div>
    );
  }

  return (
    <div
      className="widget-bit-parser-body"
      data-testid={`widget-bit-parser-${widget.id}`}
    >
      <div className="widget-meter-top">
        <span className="widget-ch-badge">
          {ch ? ch.full_path : `ch${binding.channelId}`}
        </span>
        <div className="bit-parser-header-right">
          <span className="bit-parser-word-badge">
            {liveWord !== null
              ? `${formatBitWordHex(liveWord, cfg.bitWidth)} (${liveWord})`
              : "--"}
          </span>
          {activeBits.length > 0 && (
            <span className="bit-parser-active-badge">
              {t("widget.bitParser.activeCount", { count: activeBits.length })}
            </span>
          )}
          <button
            type="button"
            className={`scope-btn-xs ${configOpen ? "active" : ""}`}
            data-testid={`bit-parser-config-btn-${widget.id}`}
            onClick={() => setConfigOpen((v) => !v)}
          >
            {t("widget.configToggle")}
          </button>
        </div>
      </div>

      <div className="bit-parser-strip" aria-label="Bit State Strip">
        {msbToLsbIndices.map((b) => {
          const isHigh = liveWord !== null && isBitSet(liveWord, b);
          const item = bitMap.get(b);
          const hasLabel = Boolean(item && item.label.trim().length > 0);
          const tipLabel = hasLabel ? item!.label : `Bit ${b}`;
          const nibbleGap = b > 0 && b % 4 === 0;
          return (
            <span
              key={b}
              className={`bit-parser-cell${isHigh ? " is-high" : ""}${hasLabel ? " is-labeled" : ""}${nibbleGap ? " nibble-gap" : ""}`}
              title={`B${b}: ${tipLabel} = ${isHigh ? 1 : 0}`}
            >
              <span className="bit-parser-cell-idx">{b}</span>
              <span className="bit-parser-cell-val">{isHigh ? "1" : "0"}</span>
            </span>
          );
        })}
      </div>

      <div className="bit-parser-active-list">
        {liveWord === null ? (
          <div className="bit-parser-empty">{t("widget.bitParser.waitingData")}</div>
        ) : activeBits.length === 0 ? (
          <div className="bit-parser-empty">{t("widget.bitParser.noneActive")}</div>
        ) : (
          activeBits.map((entry) => (
            <div
              key={entry.bit}
              className={`bit-parser-msg-item tone-${entry.tone}`}
              data-testid={`bit-parser-active-${widget.id}-${entry.bit}`}
            >
              <span className="bit-parser-idx">B{entry.bit}</span>
              <span className="bit-parser-msg-text">{entry.label}</span>
            </div>
          ))
        )}

        {configuredCount === 0 && !configOpen && (
          <button
            type="button"
            className="bit-parser-hint-btn"
            onClick={() => setConfigOpen(true)}
          >
            {t("widget.bitParser.unconfiguredHint")}
          </button>
        )}
      </div>

      {configOpen && (
        <div
          className="bit-parser-config-panel"
          data-testid={`bit-parser-config-panel-${widget.id}`}
        >
          <div className="bit-parser-config-toolbar">
            <label className="bit-parser-toolbar-field">
              <span>{t("widget.bitParser.width")}</span>
              <select
                value={cfg.bitWidth}
                onChange={(ev) => {
                  const nextW = Number(ev.target.value) as BitParserWidth;
                  if (!VALID_BIT_WIDTHS.includes(nextW)) return;
                  updateBitConfig((prevCfg) => ({
                    ...prevCfg,
                    bitWidth: nextW,
                  }));
                }}
              >
                {VALID_BIT_WIDTHS.map((w) => (
                  <option key={w} value={w}>
                    {w} bit
                  </option>
                ))}
              </select>
            </label>
            <label className="bit-parser-toolbar-check">
              <input
                type="checkbox"
                checked={Boolean(cfg.showUnlabeled)}
                onChange={(ev) => {
                  const checked = ev.target.checked;
                  updateBitConfig((prevCfg) => ({
                    ...prevCfg,
                    showUnlabeled: checked,
                  }));
                }}
              />
              <span>{t("widget.bitParser.showUnlabeled")}</span>
            </label>
          </div>

          <div className="bit-parser-config-rows">
            {lsbToMsbIndices.map((bit) => {
              const item = bitMap.get(bit);
              const isHigh = liveWord !== null && isBitSet(liveWord, bit);
              return (
                <div key={bit} className="bit-parser-config-row">
                  <span
                    className={`bit-parser-row-dot${isHigh ? " is-high" : ""}`}
                    title={`B${bit} = ${isHigh ? 1 : 0}`}
                  >
                    {isHigh ? "1" : "0"}
                  </span>
                  <span className="bit-parser-row-idx">B{bit}</span>
                  <input
                    type="text"
                    className="bit-parser-row-input"
                    value={item?.label ?? ""}
                    placeholder={t("widget.bitParser.bitPlaceholder", { bit })}
                    onChange={(ev) => {
                      const val = ev.target.value;
                      updateBitConfig((prevCfg) =>
                        upsertBitConfigItem(prevCfg, bit, { label: val }),
                      );
                    }}
                  />
                  <select
                    className="bit-parser-row-tone"
                    value={item?.tone ?? "info"}
                    onChange={(ev) => {
                      const tone = ev.target.value as BitParserTone;
                      if (!VALID_BIT_TONES.includes(tone)) return;
                      updateBitConfig((prevCfg) =>
                        upsertBitConfigItem(prevCfg, bit, { tone }),
                      );
                    }}
                  >
                    {VALID_BIT_TONES.map((tone) => (
                      <option key={tone} value={tone}>
                        {t(`widget.bitParser.tone.${tone}`)}
                      </option>
                    ))}
                  </select>
                </div>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
}
