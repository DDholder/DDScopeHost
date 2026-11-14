import { useEffect, useState, type FormEvent } from "react";
import type { SendDownlinkRequestDto_Deserialize } from "../bridge/bindings.ts";
import { useWorkspaceStore } from "./store.ts";

type DownlinkPriorityDto = SendDownlinkRequestDto_Deserialize["priority"];
type ManualPayloadMode = "ascii_text" | "hex_bytes" | "key_value";

export function DownlinkBar() {
  const collapsed = useWorkspaceStore((s) => s.downlinkBarCollapsed);
  const setCollapsed = useWorkspaceStore((s) => s.setDownlinkBarCollapsed);
  const channelGroups = useWorkspaceStore((s) => s.channelGroups);
  const downlinkStats = useWorkspaceStore((s) => s.downlinkStats);
  const sliderEventCount = useWorkspaceStore((s) => s.sliderEventCount);
  const txLog = useWorkspaceStore((s) => s.txLog);
  const sendDownlink = useWorkspaceStore((s) => s.sendDownlink);
  const injectGlitch = useWorkspaceStore((s) => s.injectGlitch);

  const [targetSource, setTargetSource] = useState("sim0");
  const [priority, setPriority] = useState<DownlinkPriorityDto>("normal");
  const [kpLocal, setKpLocal] = useState(downlinkStats.motorKp);
  const [freqLocal, setFreqLocal] = useState(downlinkStats.motorFreqHz);

  const [manualMode, setManualMode] = useState<ManualPayloadMode>("ascii_text");
  const [asciiText, setAsciiText] = useState("SET_MODE=FOC_RUN");
  const [appendNewline, setAppendNewline] = useState(true);
  const [hexText, setHexText] = useState("AA 55 01 10");
  const [kvKey, setKvKey] = useState("kp");
  const [kvVal, setKvVal] = useState("1.75");
  const [manualError, setManualError] = useState<string | null>(null);

  useEffect(() => {
    setKpLocal(downlinkStats.motorKp);
  }, [downlinkStats.motorKp]);

  useEffect(() => {
    setFreqLocal(downlinkStats.motorFreqHz);
  }, [downlinkStats.motorFreqHz]);

  useEffect(() => {
    if (channelGroups.length === 0) return;
    if (!channelGroups.some((g) => g.source.source_id === targetSource)) {
      setTargetSource(channelGroups[0].source.source_id);
    }
  }, [channelGroups, targetSource]);

  const handleSliderKp = (val: number) => {
    setKpLocal(val);
    void sendDownlink(
      {
        source_id: targetSource,
        priority,
        coalesce_key: `${targetSource}:kp`,
        payload: { kind: "key_value", key: "kp", value: val },
      },
      `kp=${val.toFixed(2)}`,
      true,
    );
  };

  const handleSliderFreq = (val: number) => {
    setFreqLocal(val);
    void sendDownlink(
      {
        source_id: targetSource,
        priority,
        coalesce_key: `${targetSource}:freq_hz`,
        payload: { kind: "key_value", key: "freq_hz", value: val },
      },
      `freq_hz=${val.toFixed(0)}`,
      true,
    );
  };

  const handleBurstCoalesce = async () => {
    const base = kpLocal;
    const tasks: Promise<unknown>[] = [];
    for (let i = 1; i <= 10; i += 1) {
      const nextVal = Number((1.0 + ((base * 10 + i) % 35) * 0.1).toFixed(2));
      if (i === 10) setKpLocal(nextVal);
      tasks.push(
        sendDownlink(
          {
            source_id: targetSource,
            priority: "normal",
            coalesce_key: `${targetSource}:kp`,
            payload: { kind: "key_value", key: "kp", value: nextVal },
          },
          `burst kp=${nextVal.toFixed(2)}`,
          true,
        ),
      );
    }
    await Promise.all(tasks);
  };

  const handleManualSend = async (ev: FormEvent) => {
    ev.preventDefault();
    setManualError(null);
    try {
      if (manualMode === "ascii_text") {
        await sendDownlink(
          {
            source_id: targetSource,
            priority,
            coalesce_key: null,
            payload: {
              kind: "ascii_text",
              text: asciiText,
              append_newline: appendNewline,
            },
          },
          `ASCII "${asciiText}"`,
        );
      } else if (manualMode === "hex_bytes") {
        const cleanHex = hexText.trim();
        await sendDownlink(
          {
            source_id: targetSource,
            priority,
            coalesce_key: null,
            payload: { kind: "hex_bytes", hex: cleanHex },
          },
          `HEX [${cleanHex}]`,
        );
      } else {
        const numVal = kvVal.trim() === "" ? null : Number(kvVal);
        if (numVal !== null && !Number.isFinite(numVal)) {
          setManualError("KV value must be numeric or empty");
          return;
        }
        await sendDownlink(
          {
            source_id: targetSource,
            priority,
            coalesce_key: `${targetSource}:${kvKey.trim() || "param"}`,
            payload: {
              kind: "key_value",
              key: kvKey.trim() || "param",
              value: numVal,
            },
          },
          `KV ${kvKey.trim()}=${numVal ?? "null"}`,
        );
      }
    } catch (err: unknown) {
      setManualError(err instanceof Error ? err.message : String(err));
    }
  };

  if (collapsed) {
    return (
      <section className="downlink-bar-collapsed" data-testid="downlink-bar-collapsed">
        <div className="downlink-collapsed-left">
          <button
            type="button"
            className="scope-btn scope-btn-accent"
            data-testid="downlink-expand-btn"
            onClick={() => setCollapsed(false)}
          >
            ▲ TX Downlink &amp; Coalescer
          </button>
          <span className="downlink-stat-pill">
            Sliders:{" "}
            <strong data-testid="downlink-slider-count">{sliderEventCount}</strong>
          </span>
          <span className="downlink-stat-pill">
            Dispatched:{" "}
            <strong data-testid="downlink-dispatched-count">
              {downlinkStats.totalDispatched}
            </strong>
          </span>
          <span className="downlink-stat-pill">
            Coalesced:{" "}
            <strong data-testid="downlink-coalesced-count">
              {downlinkStats.coalescedOverwrites}
            </strong>
          </span>
        </div>
        <div className="downlink-collapsed-right">
          <button
            type="button"
            className="scope-btn scope-btn-warn"
            data-testid="downlink-inject-glitch-btn"
            onClick={() => void injectGlitch()}
          >
            ⚡ Inject +80V Glitch
          </button>
        </div>
      </section>
    );
  }

  return (
    <section className="downlink-bar" data-testid="downlink-bar">
      <div className="downlink-header">
        <div className="downlink-header-left">
          <span className="downlink-title">Full-Duplex TX Downlink &amp; Coalescer</span>
          <span className="downlink-stat-pill">
            Sliders:{" "}
            <strong data-testid="downlink-slider-count">{sliderEventCount}</strong>
          </span>
          <span className="downlink-stat-pill">
            Dispatched:{" "}
            <strong data-testid="downlink-dispatched-count">
              {downlinkStats.totalDispatched}
            </strong>
          </span>
          <span className="downlink-stat-pill pill-coalesce">
            Coalesced:{" "}
            <strong data-testid="downlink-coalesced-count">
              {downlinkStats.coalescedOverwrites}
            </strong>
          </span>
          <span className="downlink-stat-pill">
            Queue:{" "}
            <strong data-testid="downlink-queue-depth">
              {downlinkStats.pendingQueueDepth}
            </strong>
          </span>
        </div>

        <div className="downlink-header-right">
          <button
            type="button"
            className="scope-btn scope-btn-warn"
            data-testid="downlink-inject-glitch-btn"
            onClick={() => void injectGlitch()}
          >
            ⚡ Inject +80V Glitch
          </button>
          <button
            type="button"
            className="icon-btn"
            data-testid="downlink-collapse-btn"
            title="Collapse TX Downlink Bar"
            onClick={() => setCollapsed(true)}
          >
            ▼
          </button>
        </div>
      </div>

      <div className="downlink-body">
        {/* Left: Target & High-Frequency Coalescing Sliders */}
        <div className="downlink-card downlink-sliders-card">
          <div className="downlink-target-row">
            <label className="downlink-inline-field">
              <span>Target</span>
              <select
                className="scope-select"
                value={targetSource}
                onChange={(e) => setTargetSource(e.target.value)}
              >
                {channelGroups.map((g) => (
                  <option key={g.source.source_id} value={g.source.source_id}>
                    {g.source.source_id}
                  </option>
                ))}
              </select>
            </label>
            <label className="downlink-inline-field">
              <span>Priority</span>
              <select
                className="scope-select"
                value={priority}
                onChange={(e) => setPriority(e.target.value as DownlinkPriorityDto)}
              >
                <option value="normal">Normal (Coalesce Key / FIFO)</option>
                <option value="high">High (Preempt Immediate)</option>
              </select>
            </label>
            <button
              type="button"
              className="scope-btn scope-btn-accent"
              data-testid="burst-coalesce-btn"
              title="Send 10 rapid keyed updates to verify Latest-Value-Wins coalescing"
              onClick={() => void handleBurstCoalesce()}
            >
              Burst 10x Drag Test
            </button>
          </div>

          <div className="downlink-slider-row">
            <span className="downlink-slider-key">{targetSource}:kp</span>
            <input
              type="range"
              min={0.1}
              max={5.0}
              step={0.05}
              value={kpLocal}
              data-testid="slider-kp"
              onChange={(e) => handleSliderKp(Number(e.target.value))}
            />
            <span className="downlink-slider-val">{kpLocal.toFixed(2)}</span>
          </div>

          <div className="downlink-slider-row">
            <span className="downlink-slider-key">{targetSource}:freq_hz</span>
            <input
              type="range"
              min={5}
              max={200}
              step={1}
              value={freqLocal}
              data-testid="slider-freq"
              onChange={(e) => handleSliderFreq(Number(e.target.value))}
            />
            <span className="downlink-slider-val">{freqLocal.toFixed(0)} Hz</span>
          </div>
        </div>

        {/* Center: Manual ASCII / HEX / KV TX Sender */}
        <form className="downlink-card downlink-manual-card" onSubmit={handleManualSend}>
          <div className="downlink-manual-top">
            <select
              className="scope-select"
              value={manualMode}
              data-testid="manual-tx-mode"
              onChange={(e) => setManualMode(e.target.value as ManualPayloadMode)}
            >
              <option value="ascii_text">ASCII</option>
              <option value="hex_bytes">HEX Bytes</option>
              <option value="key_value">Key = Value</option>
            </select>

            {manualMode === "ascii_text" && (
              <>
                <input
                  type="text"
                  className="downlink-input"
                  value={asciiText}
                  data-testid="manual-tx-ascii-input"
                  onChange={(e) => setAsciiText(e.target.value)}
                  placeholder="ASCII command..."
                />
                <label className="downlink-nl-check">
                  <input
                    type="checkbox"
                    checked={appendNewline}
                    onChange={(e) => setAppendNewline(e.target.checked)}
                  />
                  \n
                </label>
              </>
            )}

            {manualMode === "hex_bytes" && (
              <input
                type="text"
                className="downlink-input"
                value={hexText}
                data-testid="manual-tx-hex-input"
                onChange={(e) => setHexText(e.target.value)}
                placeholder="AA 55 01 FF"
              />
            )}

            {manualMode === "key_value" && (
              <>
                <input
                  type="text"
                  className="downlink-input downlink-input-key"
                  value={kvKey}
                  onChange={(e) => setKvKey(e.target.value)}
                  placeholder="key"
                />
                <input
                  type="text"
                  className="downlink-input downlink-input-val"
                  value={kvVal}
                  onChange={(e) => setKvVal(e.target.value)}
                  placeholder="value"
                />
              </>
            )}

            <button
              type="submit"
              className="scope-btn scope-btn-accent"
              data-testid="manual-tx-send-btn"
            >
              Send TX
            </button>
          </div>
          {manualError && <div className="downlink-error">{manualError}</div>}
          <div className="downlink-hint">
            Coalescing key overwrites stale queued commands in-place under TxCoalescer (&lt; 90 ms window).
          </div>
        </form>

        {/* Right: TX Receipt Log */}
        <div className="downlink-card downlink-log-card" data-testid="downlink-tx-log">
          {txLog.length === 0 ? (
            <div className="downlink-log-empty">
              Drag sliders or click Send TX to inspect coalescing &amp; TX receipts.
            </div>
          ) : (
            <div className="downlink-log-list">
              {txLog.slice(0, 8).map((entry) => (
                <div key={entry.id} className="downlink-log-row">
                  <span className="log-ts">{entry.timestamp}</span>
                  <span className="log-src">{entry.sourceId}</span>
                  <span className="log-msg">{entry.summary}</span>
                  {entry.coalesced && (
                    <span className="log-coalesced-badge">COALESCED</span>
                  )}
                  <span className="log-res">{entry.resultKind}</span>
                </div>
              ))}
            </div>
          )}
        </div>
      </div>
    </section>
  );
}
