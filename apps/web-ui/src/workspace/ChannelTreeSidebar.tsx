import { useEffect, useMemo, useRef, useState, type DragEvent } from "react";
import type { ChannelModalityDto, ChannelNodeDto_Serialize } from "../bridge/bindings.ts";
import { bridgeClient } from "../bridge/control-client.ts";
import { DdsfSnapshotKind } from "../bridge/ddsf-spec.ts";
import {
  formatBytesCompact,
  formatSampleRate,
} from "../scope-engine/time-format.ts";
import { useWorkspaceStore } from "./store.ts";

function modalityBadge(modality: ChannelModalityDto): { label: string; cls: string } {
  switch (modality) {
    case "digital_bool":
      return { label: "DIG", cls: "mod-dig" };
    case "state_machine":
      return { label: "FSM", cls: "mod-fsm" };
    case "frame":
      return { label: "FFT", cls: "mod-fft" };
    default:
      return { label: "ANA", cls: "mod-ana" };
  }
}

export function ChannelTreeSidebar() {
  const collapsed = useWorkspaceStore((s) => s.sidebarCollapsed);
  const setCollapsed = useWorkspaceStore((s) => s.setSidebarCollapsed);
  const channelGroups = useWorkspaceStore((s) => s.channelGroups);
  const channelsById = useWorkspaceStore((s) => s.channelsById);
  const telemetryBySource = useWorkspaceStore((s) => s.telemetryBySource);
  const focusedScopeId = useWorkspaceStore((s) => s.focusedScopeId);
  const scopeOrder = useWorkspaceStore((s) => s.scopeOrder);
  const setSourceWizardOpen = useWorkspaceStore((s) => s.setSourceWizardOpen);
  const startSource = useWorkspaceStore((s) => s.startSource);
  const stopSource = useWorkspaceStore((s) => s.stopSource);
  const removeSource = useWorkspaceStore((s) => s.removeSource);
  const toggleRawRecorder = useWorkspaceStore((s) => s.toggleRawRecorder);
  const seekReplay = useWorkspaceStore((s) => s.seekReplay);
  const bindChannelToScope = useWorkspaceStore((s) => s.bindChannelToScope);

  const [filter, setFilter] = useState("");
  const valueRefs = useRef<Map<number, HTMLSpanElement>>(new Map());
  const channelsRef = useRef(channelsById);
  channelsRef.current = channelsById;

  // 15Hz DOM-ref update for live channel snapshot values without React re-renders
  useEffect(() => {
    let lastFlushMs = 0;
    const latestVals = new Map<number, { kind: number; val: number }>();

    const unsub = bridgeClient.topicBus.subscribeSnapshots((snapshots) => {
      for (let i = 0; i < snapshots.length; i += 1) {
        const s = snapshots[i];
        latestVals.set(s.channelNumId, { kind: s.kind, val: s.valF64 });
      }
      const now = performance.now();
      if (now - lastFlushMs < 65) return;
      lastFlushMs = now;

      valueRefs.current.forEach((el, chId) => {
        const entry = latestVals.get(chId);
        if (!entry) return;
        const ch = channelsRef.current[chId];
        if (entry.kind === DdsfSnapshotKind.Bool) {
          el.textContent = entry.val > 0.5 ? "HIGH (1)" : "LOW (0)";
          el.className = entry.val > 0.5 ? "ch-live-val val-high" : "ch-live-val val-low";
        } else if (entry.kind === DdsfSnapshotKind.StateEnum) {
          const code = Math.round(entry.val);
          const lbl = ch?.state_labels.find((l) => l.code === code)?.name ?? `S${code}`;
          el.textContent = lbl;
          el.className = code === 3 ? "ch-live-val val-fault" : "ch-live-val val-state";
        } else {
          const unit = ch?.unit ? ` ${ch.unit}` : "";
          el.textContent = `${entry.val.toFixed(2)}${unit}`;
          el.className = "ch-live-val";
        }
      });
    });

    return unsub;
  }, []);

  const filteredGroups = useMemo(() => {
    const q = filter.trim().toLowerCase();
    if (!q) return channelGroups;
    return channelGroups
      .map((grp) => ({
        ...grp,
        channels: grp.channels.filter(
          (c) =>
            c.full_path.toLowerCase().includes(q) ||
            c.display_name.toLowerCase().includes(q),
        ),
      }))
      .filter((grp) => grp.channels.length > 0 || grp.source.source_id.toLowerCase().includes(q));
  }, [channelGroups, filter]);

  const targetScopeId = focusedScopeId ?? scopeOrder[0] ?? null;

  const handleDragStart = (
    ev: DragEvent<HTMLDivElement>,
    ch: ChannelNodeDto_Serialize,
  ) => {
    ev.dataTransfer.setData(
      "application/x-ddscope-channel",
      JSON.stringify({
        channelId: ch.channel_id,
        modality: ch.modality,
        fullPath: ch.full_path,
      }),
    );
    ev.dataTransfer.effectAllowed = "copy";
  };

  if (collapsed) {
    return (
      <aside className="sidebar-collapsed">
        <button
          type="button"
          className="icon-btn"
          title="Expand Sources & Channels"
          onClick={() => setCollapsed(false)}
        >
          »
        </button>
      </aside>
    );
  }

  return (
    <aside className="ddscope-sidebar" data-testid="channel-tree-sidebar">
      <div className="sidebar-header">
        <span className="sidebar-title">Sources &amp; Channel Tree</span>
        <div className="sidebar-header-actions">
          <button
            type="button"
            className="scope-btn scope-btn-accent"
            data-testid="add-source-btn"
            onClick={() => setSourceWizardOpen(true)}
          >
            + New Source
          </button>
          <button
            type="button"
            className="icon-btn"
            title="Collapse Sidebar"
            onClick={() => setCollapsed(true)}
          >
            «
          </button>
        </div>
      </div>

      <div className="sidebar-search">
        <input
          type="search"
          placeholder="Filter <source>/<channel>..."
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        />
      </div>

      <div className="sidebar-tree-scroll">
        {filteredGroups.map((grp) => {
          const src = grp.source;
          const tel = telemetryBySource[src.source_id];
          const isRunning = src.state === "running";
          const isRecording = Boolean(src.raw_recording_path);

          return (
            <section
              key={src.source_id}
              className="source-group"
              data-testid={`source-group-${src.source_id}`}
            >
              <div className="source-header">
                <div className="source-title-row">
                  <span className={`source-dot state-${src.state}`} />
                  <strong className="source-id">{src.source_id}</strong>
                  <span className="source-kind">{src.transport_kind}</span>
                  <span className="source-state">{src.state}</span>
                </div>
                <div className="source-meta-row">
                  <span>
                    {tel?.samples_per_sec != null
                      ? formatSampleRate(tel.samples_per_sec)
                      : "--"}
                  </span>
                  <span>
                    {tel?.bytes_per_sec != null
                      ? `${formatBytesCompact(tel.bytes_per_sec)}/s`
                      : "--"}
                  </span>
                </div>
                <div className="source-actions-row">
                  {isRunning ? (
                    <button
                      type="button"
                      className="scope-btn-xs"
                      onClick={() => stopSource(src.source_id)}
                    >
                      Stop
                    </button>
                  ) : (
                    <button
                      type="button"
                      className="scope-btn-xs scope-btn-accent"
                      onClick={() => startSource(src.source_id)}
                    >
                      Start
                    </button>
                  )}
                  <button
                    type="button"
                    className={`scope-btn-xs ${isRecording ? "rec-active" : ""}`}
                    title="Toggle Bypass .ddraw Raw Recorder"
                    onClick={() => toggleRawRecorder(src.source_id, !isRecording)}
                  >
                    {isRecording ? "● REC" : "Rec .ddraw"}
                  </button>
                  {src.source_id !== "sim0" && (
                    <button
                      type="button"
                      className="scope-btn-xs"
                      title="Remove Source"
                      onClick={() => removeSource(src.source_id)}
                    >
                      Del
                    </button>
                  )}
                </div>

                {src.transport_kind === "raw_file_replay" && (
                  <div className="replay-controls">
                    <input
                      type="range"
                      min={0}
                      max={100}
                      defaultValue={0}
                      title="Seek File Position"
                      onChange={(e) =>
                        seekReplay({
                          source_id: src.source_id,
                          offset_ratio: Number(e.target.value) / 100,
                          paused: null,
                          playback_speed: null,
                        })
                      }
                    />
                    <select
                      defaultValue="1"
                      title="Playback Speed"
                      onChange={(e) =>
                        seekReplay({
                          source_id: src.source_id,
                          offset_ratio: null,
                          paused: null,
                          playback_speed: Number(e.target.value),
                        })
                      }
                    >
                      <option value="0.25">0.25x</option>
                      <option value="1">1x</option>
                      <option value="5">5x</option>
                      <option value="20">20x</option>
                    </select>
                  </div>
                )}
              </div>

              <div className="channel-list">
                {grp.channels.map((ch) => {
                  const badge = modalityBadge(ch.modality);
                  return (
                    <div
                      key={ch.channel_id}
                      className="channel-row"
                      draggable
                      data-testid={`channel-row-${ch.full_path}`}
                      onDragStart={(ev) => handleDragStart(ev, ch)}
                      title="Drag onto any MSO Oscilloscope pane or click + to bind"
                    >
                      <span className={`ch-mod-badge ${badge.cls}`}>{badge.label}</span>
                      <span className="ch-path">{ch.local_path}</span>
                      <span
                        className="ch-live-val"
                        ref={(el) => {
                          if (el) valueRefs.current.set(ch.channel_id, el);
                          else valueRefs.current.delete(ch.channel_id);
                        }}
                      >
                        --
                      </span>
                      {targetScopeId !== null && (
                        <button
                          type="button"
                          className="ch-bind-btn"
                          title={`Bind to Scope ${targetScopeId}`}
                          data-testid={`bind-ch-${ch.full_path}`}
                          onClick={() => bindChannelToScope(targetScopeId, ch.channel_id)}
                        >
                          +
                        </button>
                      )}
                    </div>
                  );
                })}
              </div>
            </section>
          );
        })}
      </div>
    </aside>
  );
}
